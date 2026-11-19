# komponist-stretch

Provides pitch-preserving audio stretch for Warp clips. The engine calls this library from its
read-ahead and offline render paths.

## Structure

- [src/lib.rs](src/lib.rs) — Exposes the checked Rust API for setup, seeking and processing.
- [native/](native/) — Contains the C++ adapter to Signalsmith Stretch.
- [build.rs](build.rs) — Compiles the native adapter against the vendored sources.
- [src/tests.rs](src/tests.rs) — Exercises validation, seeking, length and pitch behavior.

## How it works

The `Stretcher` accepts planar mono or stereo audio. The ratio is output duration divided by input
duration. The caller supplies look-ahead to prime the stretcher at the desired position, then asks
for the input needed for each 512-frame output chunk or final partial chunk. The caller sets the
finite output length and pads reads beyond EOF. Construction, processing and destruction stay on
worker threads.
