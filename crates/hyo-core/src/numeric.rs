//! Numeric backend abstraction used by range aggregations.
//!
//! The evaluator always supplies `f64` values, matching Excel's binary64 cell
//! storage. A backend may compute in a different association order (as a GPU
//! reduction does), so callers compare results with an explicitly documented
//! tolerance rather than bit equality.

use std::fmt::{self, Display, Formatter};

/// Which implementation produced a numeric result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Cpu,
    Cuda,
}

impl Display for BackendKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cpu => f.write_str("CPU"),
            Self::Cuda => f.write_str("CUDA (cuda-oxide)"),
        }
    }
}

/// One-pass statistics over a numeric range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aggregate {
    pub count: usize,
    pub sum: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub sum_sq: f64,
    pub backend: BackendKind,
}

impl Aggregate {
    pub fn empty(backend: BackendKind) -> Self {
        Self {
            count: 0,
            sum: 0.0,
            min: None,
            max: None,
            sum_sq: 0.0,
            backend,
        }
    }

    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }
}

/// A dot product / `SUMPRODUCT` result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DotProduct {
    pub value: f64,
    pub backend: BackendKind,
}

/// Device-independent numeric primitive used by the formula evaluator.
///
/// Implementations must be usable from the UI thread and must not panic on
/// mismatched lengths; callers validate lengths before invoking `dot`.
pub trait NumericBackend {
    fn name(&self) -> &'static str;
    fn aggregate(&self, values: &[f64]) -> Aggregate;
    fn dot(&self, lhs: &[f64], rhs: &[f64]) -> DotProduct;
}

/// Deterministic, allocation-free CPU implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuBackend;

impl NumericBackend for CpuBackend {
    fn name(&self) -> &'static str {
        "CPU"
    }

    fn aggregate(&self, values: &[f64]) -> Aggregate {
        if values.is_empty() {
            return Aggregate::empty(BackendKind::Cpu);
        }
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for &value in values {
            sum += value;
            sum_sq += value * value;
            if value < min {
                min = value;
            }
            if value > max {
                max = value;
            }
        }
        Aggregate {
            count: values.len(),
            sum,
            min: min.is_finite().then_some(min),
            max: max.is_finite().then_some(max),
            sum_sq,
            backend: BackendKind::Cpu,
        }
    }

    fn dot(&self, lhs: &[f64], rhs: &[f64]) -> DotProduct {
        debug_assert_eq!(lhs.len(), rhs.len());
        let mut value = 0.0;
        for (a, b) in lhs.iter().zip(rhs) {
            value += a * b;
        }
        DotProduct {
            value,
            backend: BackendKind::Cpu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_match_a_naive_loop() {
        let values = [1.0, 2.0, -3.0, 4.5];
        let result = CpuBackend.aggregate(&values);
        assert_eq!(result.count, 4);
        assert_eq!(result.sum, 4.5);
        assert_eq!(result.min, Some(-3.0));
        assert_eq!(result.max, Some(4.5));
        assert_eq!(result.sum_sq, 1.0 + 4.0 + 9.0 + 20.25);
        assert_eq!(result.mean(), Some(1.125));
    }

    #[test]
    fn empty_aggregate_is_neutral() {
        let result = CpuBackend.aggregate(&[]);
        assert_eq!(result, Aggregate::empty(BackendKind::Cpu));
        assert_eq!(result.mean(), None);
    }
}
