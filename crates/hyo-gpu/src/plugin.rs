//! Dynamic loader for the cuda-oxide plugin.
//!
//! The plugin is a `cdylib` produced by `gpu/hyo-cuda-oxide` with NVIDIA's
//! `cargo oxide`. It is intentionally loaded at runtime: the spreadsheet's
//! stable build must not depend on the cuda-oxide nightly, CUDA headers or a
//! Linux CUDA toolkit. When the plugin is absent, `GpuBackend` uses the CPU
//! reference implementation.

use hyo_core::{Aggregate, BackendKind, DotProduct};
use libloading::Library;
use std::ffi::{c_char, c_void};
use std::path::PathBuf;

pub const STATUS_OK: i32 = 0;
pub const STATUS_NULL: i32 = 1;
pub const STATUS_DEVICE: i32 = 2;

/// C ABI mirror of `hyo_cuda_oxide::HyoCudaAggregate`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CAggregate {
    count: u64,
    sum: f64,
    min: f64,
    has_min: u32,
    max: f64,
    has_max: u32,
    sum_sq: f64,
}

type CreateFn = unsafe extern "C" fn() -> *mut c_void;
type DestroyFn = unsafe extern "C" fn(*mut c_void);
type DeviceNameFn = unsafe extern "C" fn(*mut c_void, *mut c_char, usize) -> usize;
type AggregateFn = unsafe extern "C" fn(*mut c_void, *const f64, usize, *mut CAggregate) -> i32;
type DotFn = unsafe extern "C" fn(*mut c_void, *const f64, *const f64, usize, *mut f64) -> i32;

pub struct CudaPlugin {
    handle: *mut c_void,
    device_name: String,
    path: PathBuf,
    destroy: DestroyFn,
    aggregate: AggregateFn,
    dot: DotFn,
    // Kept alive until `Drop`; must be dropped after the function pointers are
    // no longer used.
    _library: Library,
}

impl std::fmt::Debug for CudaPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CudaPlugin")
            .field("device", &self.device_name)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl CudaPlugin {
    pub fn load() -> Result<Self, String> {
        let path = locate_plugin()?;
        // SAFETY: loading a dynamic library executes its initializers. Hyo only
        // loads the cuda-oxide plugin from an explicit path or beside its own
        // executable; it is part of the trusted distribution.
        let library = unsafe { Library::new(&path) }
            .map_err(|error| format!("loading {}: {error}", path.display()))?;

        // SAFETY: symbol names and signatures are the plugin ABI defined in
        // `gpu/hyo-cuda-oxide/src/lib.rs`. A mismatched plugin fails the
        // signature check only at the ABI level, which is why the plugin and
        // app are versioned together.
        unsafe {
            let create: CreateFn = *library
                .get(b"hyo_cuda_create\0")
                .map_err(|error| format!("hyo_cuda_create: {error}"))?;
            let destroy: DestroyFn = *library
                .get(b"hyo_cuda_destroy\0")
                .map_err(|error| format!("hyo_cuda_destroy: {error}"))?;
            let device_name: DeviceNameFn = *library
                .get(b"hyo_cuda_device_name\0")
                .map_err(|error| format!("hyo_cuda_device_name: {error}"))?;
            let aggregate: AggregateFn = *library
                .get(b"hyo_cuda_aggregate\0")
                .map_err(|error| format!("hyo_cuda_aggregate: {error}"))?;
            let dot: DotFn = *library
                .get(b"hyo_cuda_dot\0")
                .map_err(|error| format!("hyo_cuda_dot: {error}"))?;

            let handle = create();
            if handle.is_null() {
                return Err("cuda-oxide plugin could not create a CUDA context".to_owned());
            }

            let mut buffer = [0 as c_char; 512];
            let needed = device_name(handle, buffer.as_mut_ptr(), buffer.len());
            if needed == 0 {
                destroy(handle);
                return Err("cuda-oxide plugin returned an empty device name".to_owned());
            }
            let bytes: Vec<u8> = buffer
                .iter()
                .take_while(|byte| **byte != 0)
                .map(|byte| *byte as u8)
                .collect();
            let device_name = String::from_utf8_lossy(&bytes).into_owned();

            Ok(Self {
                handle,
                device_name,
                path,
                destroy,
                aggregate,
                dot,
                _library: library,
            })
        }
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    pub fn try_aggregate(&self, values: &[f64]) -> Result<Aggregate, String> {
        if values.is_empty() {
            return Ok(Aggregate::empty(BackendKind::Cuda));
        }
        let mut out = CAggregate::default();
        // SAFETY: `values` points to `values.len()` initialized f64 values and
        // remains alive for the duration of the call. `out` is a valid,
        // exclusively borrowed C-layout structure.
        let status =
            unsafe { (self.aggregate)(self.handle, values.as_ptr(), values.len(), &mut out) };
        match status {
            STATUS_OK => Ok(Aggregate {
                count: out.count as usize,
                sum: out.sum,
                min: (out.has_min != 0).then_some(out.min),
                max: (out.has_max != 0).then_some(out.max),
                sum_sq: out.sum_sq,
                backend: BackendKind::Cuda,
            }),
            STATUS_NULL => Err("cuda-oxide plugin: null argument".to_owned()),
            STATUS_DEVICE => Err("cuda-oxide plugin: device failure".to_owned()),
            other => Err(format!("cuda-oxide plugin: unknown status {other}")),
        }
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
        let mut value = 0.0;
        // SAFETY: both slices have equal length and live for the call; `value`
        // is a valid output slot.
        let status = unsafe {
            (self.dot)(
                self.handle,
                lhs.as_ptr(),
                rhs.as_ptr(),
                lhs.len(),
                &mut value,
            )
        };
        if status == STATUS_OK {
            Ok(DotProduct {
                value,
                backend: BackendKind::Cuda,
            })
        } else {
            Err("cuda-oxide plugin: dot product failed".to_owned())
        }
    }
}

impl Drop for CudaPlugin {
    fn drop(&mut self) {
        // SAFETY: `handle` was created by this plugin's `hyo_cuda_create` and is
        // destroyed exactly once, before the library is unloaded.
        unsafe {
            (self.destroy)(self.handle);
        }
    }
}

fn locate_plugin() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("HYO_CUDA_LIBRARY") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "HYO_CUDA_LIBRARY does not point to a file: {}",
            path.display()
        ));
    }

    let file_name = format!(
        "{}hyo_cuda_oxide{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(directory) = exe.parent()
    {
        candidates.push(directory.join(&file_name));
        candidates.push(directory.join("gpu").join(&file_name));
    }
    if let Ok(directory) = std::env::current_dir() {
        candidates.push(directory.join(&file_name));
        candidates.push(directory.join("gpu").join(&file_name));
    }
    candidates.push(PathBuf::from(&file_name));

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "cuda-oxide plugin not found ({}); set HYO_CUDA_LIBRARY or build gpu/hyo-cuda-oxide",
                file_name
            )
        })
}
