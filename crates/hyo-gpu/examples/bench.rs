//! CPU/GPU range-aggregation benchmark.
//!
//! With the default build this measures the CPU reference path. With
//! `--features cuda-oxide` and `HYO_CUDA_LIBRARY` pointing at a plugin built by
//! `gpu/hyo-cuda-oxide`, it reports the CUDA path and the relative error
//! against the CPU result.

use hyo_core::{CpuBackend, NumericBackend};
use hyo_gpu::{GpuBackend, compare_with_reference};
use std::time::Instant;

fn main() {
    let backend = GpuBackend::auto();
    println!("backend: {}", backend.status_label());
    if let Some(error) = backend.init_error() {
        println!("cuda init: {error}");
    }
    println!("threshold: {} cells", backend.threshold());
    println!(
        "{:>10}  {:>12}  {:>12}  {:>10}  {:>12}",
        "cells", "CPU ms", "GPU ms", "speedup", "rel. error"
    );

    for size in [1_000usize, 65_536, 1_000_000, 8_000_000] {
        let values: Vec<f64> = (0..size)
            .map(|index| (index as f64 * 0.000_001).sin() + 0.25)
            .collect();

        let cpu_start = Instant::now();
        let cpu = CpuBackend.aggregate(&values);
        let cpu_time = cpu_start.elapsed();

        let gpu_start = Instant::now();
        let (gpu, error) = compare_with_reference(&backend, &values);
        let gpu_time = gpu_start.elapsed();

        let speedup = cpu_time.as_secs_f64() / gpu_time.as_secs_f64().max(f64::EPSILON);
        println!(
            "{size:>10}  {:>12.3}  {:>12.3}  {:>9.2}x  {:>12.3e}",
            cpu_time.as_secs_f64() * 1000.0,
            gpu_time.as_secs_f64() * 1000.0,
            speedup,
            error
        );
        assert!(
            (cpu.sum - gpu.sum).abs() <= 1e-12 * cpu.sum.abs().max(1.0),
            "sum drift at {size}"
        );
    }
}
