//! CUDA (SIMT) acceleration for Hyo Sheet, written in pure Rust with
//! [cuda-oxide](https://github.com/NVIDIA/cuda-rust).
//!
//! # Numeric contract
//!
//! Range aggregation is the only operation that crosses the PCIe boundary.
//! Values are `f64` on both sides. The kernel uses a block-local tree
//! reduction and emits per-block partials; the host folds those partials.
//! Addition order therefore differs from the CPU path, but every result stays
//! within ~1e-12 relative error for well-conditioned inputs, well inside the
//! 15 significant digits Excel exposes.
//!
//! # Build
//!
//! This crate deliberately has its own workspace and toolchain pin. It cannot
//! be built with stable `cargo`; use NVIDIA's `cargo oxide` on Linux with CUDA
//! 13, Clang 21 and the pinned nightly:
//!
//! ```text
//! cargo +nightly-2026-10-11 install --git https://github.com/NVIDIA/cuda-rust.git cargo-oxide
//! cd gpu/hyo-cuda-oxide
//! cargo oxide build --release
//! ```
//!
//! The rest of Hyo builds on stable Rust on Windows. The `cuda-oxide` feature
//! of `hyo-gpu` wires this crate in on a Linux/CUDA machine.

use cuda_core::simt::LaunchConfig;
use cuda_core::{CudaContext, CudaStream, DeviceBuffer};
use cuda_device::{DisjointSlice, SharedArray, kernel, thread};
use cuda_host::cuda_module;
use hyo_core::{Aggregate, BackendKind, CpuBackend, DotProduct, NumericBackend};
use std::sync::Arc;

/// Fixed block size used by both the Rust kernel and the host launch. Keep the
/// shared arrays and the host `LaunchConfig` in sync.
const BLOCK: u32 = 256;
const PARTIALS_PER_BLOCK: usize = 5;

// =============================================================================
// Device kernels
// =============================================================================

#[cuda_module]
mod kernels {
    use super::*;

    /// One pass over the range. Writes five per-block partials:
    /// `[sum, min, max, sum_sq, count]`.
    ///
    /// The grid-stride loop lets the same kernel handle ranges larger than one
    /// launch's worth of threads without a second dispatch.
    #[kernel]
    pub fn range_stats(values: &[f64], mut partials: DisjointSlice<f64>) {
        static mut SUM: SharedArray<f64, 256> = SharedArray::UNINIT;
        static mut MIN: SharedArray<f64, 256> = SharedArray::UNINIT;
        static mut MAX: SharedArray<f64, 256> = SharedArray::UNINIT;
        static mut SUM_SQ: SharedArray<f64, 256> = SharedArray::UNINIT;
        static mut COUNT: SharedArray<f64, 256> = SharedArray::UNINIT;

        let tid = thread::threadIdx_x() as usize;
        let block = thread::blockIdx_x() as usize;
        let block_size = thread::blockDim_x() as usize;
        let stride = thread::gridDim_x() as usize * block_size;
        let mut index = block * block_size + tid;

        let mut sum = 0.0f64;
        let mut min = 1.0e308f64;
        let mut max = -1.0e308f64;
        let mut sum_sq = 0.0f64;
        let mut count = 0.0f64;

        while index < values.len() {
            let value = values[index];
            sum += value;
            sum_sq += value * value;
            if value < min {
                min = value;
            }
            if value > max {
                max = value;
            }
            count += 1.0;
            index += stride;
        }

        unsafe {
            SUM[tid] = sum;
            MIN[tid] = min;
            MAX[tid] = max;
            SUM_SQ[tid] = sum_sq;
            COUNT[tid] = count;
        }
        thread::sync_threads();

        let mut step = block_size / 2;
        while step > 0 {
            if tid < step {
                unsafe {
                    SUM[tid] += SUM[tid + step];
                    SUM_SQ[tid] += SUM_SQ[tid + step];
                    COUNT[tid] += COUNT[tid + step];
                    if MIN[tid + step] < MIN[tid] {
                        MIN[tid] = MIN[tid + step];
                    }
                    if MAX[tid + step] > MAX[tid] {
                        MAX[tid] = MAX[tid + step];
                    }
                }
            }
            thread::sync_threads();
            step /= 2;
        }

        if tid == 0 {
            let base = block * 5;
            unsafe {
                *partials.get_unchecked_mut(base) = SUM[0];
                *partials.get_unchecked_mut(base + 1) = MIN[0];
                *partials.get_unchecked_mut(base + 2) = MAX[0];
                *partials.get_unchecked_mut(base + 3) = SUM_SQ[0];
                *partials.get_unchecked_mut(base + 4) = COUNT[0];
            }
        }
    }

    /// Dot product for `SUMPRODUCT`. One `f64` partial per block.
    #[kernel]
    pub fn dot_product(lhs: &[f64], rhs: &[f64], mut partials: DisjointSlice<f64>) {
        static mut SUM: SharedArray<f64, 256> = SharedArray::UNINIT;

        let tid = thread::threadIdx_x() as usize;
        let block = thread::blockIdx_x() as usize;
        let block_size = thread::blockDim_x() as usize;
        let stride = thread::gridDim_x() as usize * block_size;
        let mut index = block * block_size + tid;
        let mut sum = 0.0f64;

        while index < lhs.len() && index < rhs.len() {
            sum += lhs[index] * rhs[index];
            index += stride;
        }

        unsafe {
            SUM[tid] = sum;
        }
        thread::sync_threads();

        let mut step = block_size / 2;
        while step > 0 {
            if tid < step {
                unsafe {
                    SUM[tid] += SUM[tid + step];
                }
            }
            thread::sync_threads();
            step /= 2;
        }

        if tid == 0 {
            unsafe {
                *partials.get_unchecked_mut(block) = SUM[0];
            }
        }
    }
}

// =============================================================================
// Host side
// =============================================================================

/// A live CUDA context with the cuda-oxide module loaded.
pub struct CudaBackend {
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    module: kernels::LoadedModule,
    device_name: String,
}

impl CudaBackend {
    /// Opens device 0 and loads the embedded PTX bundle.
    pub fn new() -> Result<Self, String> {
        let context = CudaContext::new(0).map_err(|error| format!("CUDA context: {error}"))?;
        let device_name = context
            .device_name()
            .unwrap_or_else(|_| "NVIDIA GPU".to_owned());
        let stream = context.default_stream();
        let module = kernels::load(&context)
            .map_err(|error| format!("loading cuda-oxide module: {error}"))?;
        Ok(Self {
            context,
            stream,
            module,
            device_name,
        })
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn compute_capability(&self) -> Result<(i32, i32), String> {
        self.context
            .compute_capability()
            .map_err(|error| error.to_string())
    }

    /// Aggregate one numeric range. Returns `Err` on any driver/kernel error so
    /// the dispatcher can fall back to the CPU without surfacing a stale value.
    pub fn try_aggregate(&self, values: &[f64]) -> Result<Aggregate, String> {
        if values.is_empty() {
            return Ok(Aggregate::empty(BackendKind::Cuda));
        }
        let blocks = values.len().div_ceil(BLOCK as usize) as u32;
        let values_dev =
            DeviceBuffer::from_host(&self.stream, values).map_err(|error| error.to_string())?;
        let mut partials =
            DeviceBuffer::<f64>::zeroed(&self.stream, blocks as usize * PARTIALS_PER_BLOCK)
                .map_err(|error| error.to_string())?;
        let config = LaunchConfig {
            grid_dim: (blocks, 1, 1),
            block_dim: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };

        // SAFETY: the grid is one-dimensional, matches `thread::index_1d`-free
        // explicit block/thread arithmetic in `range_stats`, and the partial
        // buffer has exactly five elements per block. `values_dev` covers every
        // index the kernel reads because it is sized from the same slice.
        unsafe {
            self.module
                .range_stats(&self.stream, config, &values_dev, &mut partials)
        }
        .map_err(|error| error.to_string())?;

        let partials = partials
            .to_host_vec(&self.stream)
            .map_err(|error| error.to_string())?;
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for chunk in partials.chunks_exact(PARTIALS_PER_BLOCK) {
            if chunk[4] == 0.0 {
                continue;
            }
            sum += chunk[0];
            sum_sq += chunk[3];
            min = min.min(chunk[1]);
            max = max.max(chunk[2]);
        }

        Ok(Aggregate {
            count: values.len(),
            sum,
            min: min.is_finite().then_some(min),
            max: max.is_finite().then_some(max),
            sum_sq,
            backend: BackendKind::Cuda,
        })
    }

    pub fn try_dot(&self, lhs: &[f64], rhs: &[f64]) -> Result<DotProduct, String> {
        if lhs.len() != rhs.len() {
            return Err("dot product length mismatch".to_owned());
        }
        if lhs.is_empty() {
            return Ok(DotProduct {
                value: 0.0,
                backend: BackendKind::Cuda,
            });
        }
        let blocks = lhs.len().div_ceil(BLOCK as usize) as u32;
        let lhs_dev =
            DeviceBuffer::from_host(&self.stream, lhs).map_err(|error| error.to_string())?;
        let rhs_dev =
            DeviceBuffer::from_host(&self.stream, rhs).map_err(|error| error.to_string())?;
        let mut partials = DeviceBuffer::<f64>::zeroed(&self.stream, blocks as usize)
            .map_err(|error| error.to_string())?;
        let config = LaunchConfig {
            grid_dim: (blocks, 1, 1),
            block_dim: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };

        // SAFETY: both input buffers have the same length and the partial
        // buffer has one slot per block; the launch is one-dimensional.
        unsafe {
            self.module
                .dot_product(&self.stream, config, &lhs_dev, &rhs_dev, &mut partials)
        }
        .map_err(|error| error.to_string())?;

        let partials = partials
            .to_host_vec(&self.stream)
            .map_err(|error| error.to_string())?;
        Ok(DotProduct {
            value: partials.iter().sum(),
            backend: BackendKind::Cuda,
        })
    }
}

impl NumericBackend for CudaBackend {
    fn name(&self) -> &'static str {
        "CUDA (cuda-oxide)"
    }

    fn aggregate(&self, values: &[f64]) -> Aggregate {
        self.try_aggregate(values)
            .unwrap_or_else(|_| CpuBackend.aggregate(values))
    }

    fn dot(&self, lhs: &[f64], rhs: &[f64]) -> DotProduct {
        self.try_dot(lhs, rhs)
            .unwrap_or_else(|_| CpuBackend.dot(lhs, rhs))
    }
}

// =============================================================================
// Stable C ABI consumed by `hyo-gpu`'s dynamic loader
// =============================================================================

/// Mirrors `hyo_gpu::plugin::CAggregate`. Keep the field order and types in
/// lockstep; both sides are Rust `repr(C)` with no external ABI header.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HyoCudaAggregate {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub has_min: u32,
    pub max: f64,
    pub has_max: u32,
    pub sum_sq: f64,
}

/// Creates a backend and transfers ownership to the caller. Returns null when
/// no CUDA 13 driver/device is available.
#[unsafe(no_mangle)]
pub extern "C" fn hyo_cuda_create() -> *mut std::ffi::c_void {
    match std::panic::catch_unwind(CudaBackend::new) {
        Ok(Ok(backend)) => Box::into_raw(Box::new(backend)).cast(),
        _ => std::ptr::null_mut(),
    }
}

/// Destroys a backend created by [`hyo_cuda_create`].
///
/// # Safety
/// `handle` must be null or a pointer returned by `hyo_cuda_create` that has
/// not already been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hyo_cuda_destroy(handle: *mut std::ffi::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: upheld by the caller; ownership was transferred by
    // `Box::into_raw` in `hyo_cuda_create`.
    drop(unsafe { Box::from_raw(handle.cast::<CudaBackend>()) });
}

/// Writes a NUL-terminated UTF-8 device name into `buffer`, returning the
/// required byte count including the terminator (0 on a null handle).
///
/// # Safety
/// `handle` must be a live backend pointer. `buffer` must be writable for
/// `capacity` bytes, or null when `capacity` is zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hyo_cuda_device_name(
    handle: *mut std::ffi::c_void,
    buffer: *mut std::ffi::c_char,
    capacity: usize,
) -> usize {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: caller guarantees a live backend.
    let backend = unsafe { &*handle.cast::<CudaBackend>() };
    let name = backend.device_name();
    let needed = name.len() + 1;
    if !buffer.is_null() && capacity >= needed {
        // SAFETY: destination has at least `needed` writable bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                name.as_ptr().cast::<std::ffi::c_char>(),
                buffer,
                name.len(),
            );
            *buffer.add(name.len()) = 0;
        }
    }
    needed
}

/// Aggregates `len` f64 values. Returns 0 on success, 1 for a null pointer and
/// 2 for a device failure.
///
/// # Safety
/// `handle` must be live, `values` must point to `len` initialized f64 values,
/// and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hyo_cuda_aggregate(
    handle: *mut std::ffi::c_void,
    values: *const f64,
    len: usize,
    out: *mut HyoCudaAggregate,
) -> i32 {
    if handle.is_null() || out.is_null() || (values.is_null() && len != 0) {
        return 1;
    }
    if len == 0 {
        // SAFETY: `out` is non-null and writable per the contract.
        unsafe {
            *out = HyoCudaAggregate::default();
        }
        return 0;
    }
    // SAFETY: caller guarantees the pointer/length pair and output slot.
    let backend = unsafe { &*handle.cast::<CudaBackend>() };
    let values = unsafe { std::slice::from_raw_parts(values, len) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        backend.try_aggregate(values)
    })) {
        Ok(Ok(aggregate)) => {
            // SAFETY: `out` is non-null and writable per the contract.
            unsafe {
                *out = HyoCudaAggregate {
                    count: aggregate.count as u64,
                    sum: aggregate.sum,
                    min: aggregate.min.unwrap_or(0.0),
                    has_min: u32::from(aggregate.min.is_some()),
                    max: aggregate.max.unwrap_or(0.0),
                    has_max: u32::from(aggregate.max.is_some()),
                    sum_sq: aggregate.sum_sq,
                };
            }
            0
        }
        _ => 2,
    }
}

/// Dot product over two equal-length f64 arrays. Same status codes as
/// [`hyo_cuda_aggregate`].
///
/// # Safety
/// Both pointers must reference `len` initialized f64 values; `out` must be
/// writable and `handle` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hyo_cuda_dot(
    handle: *mut std::ffi::c_void,
    lhs: *const f64,
    rhs: *const f64,
    len: usize,
    out: *mut f64,
) -> i32 {
    if handle.is_null()
        || out.is_null()
        || (lhs.is_null() && len != 0)
        || (rhs.is_null() && len != 0)
    {
        return 1;
    }
    if len == 0 {
        // SAFETY: `out` is non-null and writable per the contract.
        unsafe {
            *out = 0.0;
        }
        return 0;
    }
    // SAFETY: caller guarantees both pointer/length pairs and the output slot.
    let backend = unsafe { &*handle.cast::<CudaBackend>() };
    let lhs = unsafe { std::slice::from_raw_parts(lhs, len) };
    let rhs = unsafe { std::slice::from_raw_parts(rhs, len) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.try_dot(lhs, rhs))) {
        Ok(Ok(result)) => {
            // SAFETY: `out` is non-null and writable per the contract.
            unsafe {
                *out = result.value;
            }
            0
        }
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_round_trip_matches_cpu() {
        // Ignored on machines without a CUDA 13 driver; run explicitly with
        // `cargo oxide test` on a GPU host.
        let Ok(backend) = CudaBackend::new() else {
            return;
        };
        let values: Vec<f64> = (0..1_000_000)
            .map(|index| (index as f64).sin() * 0.5)
            .collect();
        let expected = CpuBackend.aggregate(&values);
        let actual = backend.try_aggregate(&values).unwrap();
        let error = ((expected.sum - actual.sum).abs() / expected.sum.abs().max(1.0))
            .max((expected.sum_sq - actual.sum_sq).abs() / expected.sum_sq.abs().max(1.0));
        assert!(error < 1e-12, "relative error {error}");
    }
}
