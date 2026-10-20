# hyo-cuda-oxide

Range-reduction kernels for Hyo Sheet, written in pure Rust with NVIDIA's
[cuda-oxide](https://github.com/NVIDIA/cuda-rust) SIMT backend.

This crate is deliberately **outside** the stable Cargo workspace:

- cuda-oxide requires Linux, CUDA 13.x, Clang 21+, a pinned Rust nightly and
  NVIDIA's `cargo oxide` wrapper.
- Hyo Sheet's main application builds on stable Rust on Windows and loads this
  crate as a dynamic plugin at runtime. If the plugin is absent, Hyo Sheet uses
  its CPU reference backend; formula semantics do not change.

## Kernels

| Kernel | Purpose | Numerical contract |
| --- | --- | --- |
| `range_stats` | sum, min, max, sum of squares, count for `SUM/AVERAGE/MIN/MAX/COUNT` | f64; block-local tree reduction; per-block partials folded on the host |
| `dot_product` | `SUMPRODUCT` | f64; same block scheme |

The CPU and GPU paths associate additions differently. For well-conditioned
spreadsheet ranges the relative difference is below `1e-12`, well inside
Excel's 15-significant-digit precision. `hyo-gpu` exposes
`PARITY_TOLERANCE` and a benchmark example.

## Build (Linux, CUDA 13)

```bash
# One-time, pinned to the cuda-oxide toolchain:
rustup toolchain install nightly-2026-10-11
rustup component add rust-src rustc-dev llvm-tools --toolchain nightly-2026-10-11
cargo +nightly-2026-10-11 install --git https://github.com/NVIDIA/cuda-rust.git cargo-oxide

cd gpu/hyo-cuda-oxide
cargo oxide doctor
cargo oxide build --release
```

The generated shared library is `target/release/libhyo_cuda_oxide.so`. Point the
application at it:

```bash
export HYO_CUDA_LIBRARY="$PWD/target/release/libhyo_cuda_oxide.so"
cd ../..
cargo run -p hyo-app --features cuda-oxide
```

The status bar reports the adapter and `cuda-oxide` once the plugin answers.

## C ABI

`hyo-gpu` loads these symbols and does not link against CUDA:

```
hyo_cuda_create() -> *mut c_void
hyo_cuda_destroy(*mut c_void)
hyo_cuda_device_name(*mut c_void, *mut c_char, usize) -> usize
hyo_cuda_aggregate(*mut c_void, *const f64, usize, *mut HyoCudaAggregate) -> i32
hyo_cuda_dot(*mut c_void, *const f64, *const f64, usize, *mut f64) -> i32
```

`HyoCudaAggregate` is a `#[repr(C)]` mirror of
`hyo_gpu::plugin::CAggregate`:
`count: u64, sum: f64, min: f64, has_min: u32, max: f64, has_max: u32, sum_sq: f64`.
Status codes are `0 = OK`, `1 = bad argument`, `2 = device failure`; the host
falls back to CPU on any non-zero status rather than displaying a stale value.
