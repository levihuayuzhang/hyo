//! Numeric dispatch between a CPU reference path and the optional CUDA
//! (cuda-oxide) plugin.
//!
//! The cuda-oxide kernel lives in `gpu/hyo-cuda-oxide` and is compiled on a
//! Linux/CUDA machine with NVIDIA's `cargo oxide`. Keeping it behind a C ABI
//! means the stable Windows application never has to resolve the pinned
//! nightly or CUDA headers to build; it simply loads the plugin when present.
//!
//! The dispatch threshold exists because a spreadsheet range has to be
//! uploaded, reduced, and downloaded. For small ranges the PCIe round-trip
//! dominates, so the CPU path is faster and avoids jitter. The threshold is
//! configurable (`HYO_GPU_THRESHOLD`) for benchmarking.
//!
//! Numeric parity is defined by `PARITY_TOLERANCE`: a GPU reduction associates
//! additions differently, but stays within ~1e-12 relative error for the
//! well-conditioned ranges a spreadsheet is expected to aggregate. That is far
//! inside Excel's 15-significant-digit display precision.

use hyo_core::{Aggregate, CpuBackend, DotProduct, NumericBackend};

#[cfg(feature = "cuda-oxide")]
mod plugin;

/// Default number of cells before a range is sent to the GPU.
pub const DEFAULT_GPU_THRESHOLD: usize = 65_536;

/// Relative tolerance used by the compatibility tests and the benchmark
/// report. Absolute tolerance grows with the magnitude of the result.
pub const PARITY_TOLERANCE: f64 = 1e-12;

/// A `NumericBackend` that keeps a CPU reference implementation and an optional
/// CUDA implementation, dispatching by range size.
pub struct GpuBackend {
    cpu: CpuBackend,
    #[cfg(feature = "cuda-oxide")]
    cuda: Option<plugin::CudaPlugin>,
    threshold: usize,
    init_error: Option<String>,
}

impl std::fmt::Debug for GpuBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBackend")
            .field("device", &self.device_name())
            .field("threshold", &self.threshold)
            .field("init_error", &self.init_error)
            .finish()
    }
}

impl Default for GpuBackend {
    fn default() -> Self {
        Self::auto()
    }
}

impl GpuBackend {
    /// Uses the CUDA plugin when compiled with `--features cuda-oxide` and a
    /// plugin/device is available; otherwise silently falls back to CPU.
    pub fn auto() -> Self {
        let threshold = std::env::var("HYO_GPU_THRESHOLD")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_GPU_THRESHOLD);

        #[cfg(feature = "cuda-oxide")]
        let (cuda, init_error) = if std::env::var("HYO_DISABLE_CUDA").is_ok() {
            (None, Some("disabled by HYO_DISABLE_CUDA".to_owned()))
        } else {
            match plugin::CudaPlugin::load() {
                Ok(backend) => (Some(backend), None),
                Err(error) => (None, Some(error)),
            }
        };

        #[cfg(not(feature = "cuda-oxide"))]
        let (init_error, _) = (Some("built without the cuda-oxide feature".to_owned()), ());

        Self {
            cpu: CpuBackend,
            #[cfg(feature = "cuda-oxide")]
            cuda,
            threshold,
            init_error,
        }
    }

    /// CPU-only construction, used by tests and by `HYO_DISABLE_CUDA=1`.
    pub fn cpu_only() -> Self {
        Self {
            cpu: CpuBackend,
            #[cfg(feature = "cuda-oxide")]
            cuda: None,
            threshold: usize::MAX,
            init_error: Some("CPU-only".to_owned()),
        }
    }

    pub fn threshold(&self) -> usize {
        self.threshold
    }

    pub fn gpu_available(&self) -> bool {
        #[cfg(feature = "cuda-oxide")]
        {
            self.cuda.is_some()
        }
        #[cfg(not(feature = "cuda-oxide"))]
        {
            false
        }
    }

    pub fn device_name(&self) -> String {
        #[cfg(feature = "cuda-oxide")]
        if let Some(cuda) = &self.cuda {
            return cuda.device_name().to_owned();
        }
        "CPU reference".to_owned()
    }

    pub fn init_error(&self) -> Option<&str> {
        self.init_error.as_deref()
    }

    /// Short label for the status bar.
    pub fn status_label(&self) -> String {
        if self.gpu_available() {
            format!(
                "{} · cuda-oxide · threshold {}",
                self.device_name(),
                self.threshold
            )
        } else {
            "CPU reference path".to_owned()
        }
    }
}

impl NumericBackend for GpuBackend {
    fn name(&self) -> &'static str {
        if self.gpu_available() {
            "CUDA (cuda-oxide)"
        } else {
            "CPU"
        }
    }

    fn aggregate(&self, values: &[f64]) -> Aggregate {
        #[cfg(feature = "cuda-oxide")]
        if values.len() >= self.threshold
            && let Some(cuda) = &self.cuda
            && let Ok(result) = cuda.try_aggregate(values)
        {
            return result;
        }
        self.cpu.aggregate(values)
    }

    fn dot(&self, lhs: &[f64], rhs: &[f64]) -> DotProduct {
        #[cfg(feature = "cuda-oxide")]
        if lhs.len() >= self.threshold
            && let Some(cuda) = &self.cuda
            && let Ok(result) = cuda.try_dot(lhs, rhs)
        {
            return result;
        }
        self.cpu.dot(lhs, rhs)
    }
}

/// Relative-error helper shared by tests and the benchmark binary.
pub fn relative_error(expected: f64, actual: f64) -> f64 {
    if expected == actual {
        0.0
    } else {
        let scale = expected.abs().max(actual.abs()).max(1.0);
        (expected - actual).abs() / scale
    }
}

/// Runs a CPU/reference comparison for one range and returns
/// `(actual, worst_relative_error)`.
pub fn compare_with_reference(backend: &GpuBackend, values: &[f64]) -> (Aggregate, f64) {
    let expected = CpuBackend.aggregate(values);
    let actual = backend.aggregate(values);
    let error = relative_error(expected.sum, actual.sum)
        .max(relative_error(expected.sum_sq, actual.sum_sq));
    (actual, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_dispatch_is_exact() {
        let backend = GpuBackend::cpu_only();
        let values: Vec<f64> = (0..1000).map(|value| value as f64 * 0.5).collect();
        let expected = CpuBackend.aggregate(&values);
        assert_eq!(backend.aggregate(&values), expected);
        assert_eq!(
            backend.dot(&values, &values),
            CpuBackend.dot(&values, &values)
        );
    }

    #[test]
    fn error_helper_is_relative() {
        assert_eq!(relative_error(100.0, 100.0), 0.0);
        assert!(relative_error(1.0, 1.0 + 1e-14) < PARITY_TOLERANCE);
        assert!(relative_error(1.0e9, 1.0e9 + 1.0) < 2.0e-9);
    }

    #[test]
    fn cpu_only_has_no_device() {
        let backend = GpuBackend::cpu_only();
        assert!(!backend.gpu_available());
        assert_eq!(backend.name(), "CPU");
    }

    #[test]
    fn block_reduction_order_stays_within_excel_precision() {
        // Simulate the kernel's block-local tree reduction and host fold. The
        // values are deliberately large and mixed-sign, which is the worst
        // case for a different association order.
        let values: Vec<f64> = (0..2_000_000)
            .map(|index| {
                let x = (index as f64 * 0.000_01).sin();
                x * 1.0e6 + if index % 7 == 0 { -1.0e6 } else { 0.0 }
            })
            .collect();
        let expected = CpuBackend.aggregate(&values);

        let mut partials = Vec::new();
        for chunk in values.chunks(256) {
            partials.push(chunk.iter().sum::<f64>());
        }
        let gpu_order: f64 = partials.iter().sum();
        let error = relative_error(expected.sum, gpu_order);
        assert!(
            error < 1e-12,
            "association-order error {error:e} exceeds Excel precision budget"
        );
    }
}
