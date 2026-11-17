# Vendored Signalsmith Stretch / Linear

- Stretch **1.3.2**: https://github.com/Signalsmith-Audio/signalsmith-stretch,
  commit `88061fd4eda56a2353c5d65c74aa3f4ccf76f9da`.
- Linear **0.6.6**: https://github.com/Signalsmith-Audio/linear,
  commit `7bb1c8f36794d6980080ce817c91f8da8fe56a66`.
- Both are MIT: complete upstream `LICENSE.txt` files, copyright notices, READMEs and SUPPORT
  notices are retained. These permissive licenses are compatible with komponist's GPL-3.0-or-later.
- Imported byte-for-byte on 2026-11-17 for issue #14 WP-B. `SHA256SUMS` records every copied file.
  Run `sha256sum --check SHA256SUMS` in this directory to verify them. `.gitattributes` preserves
  upstream bytes/whitespace; do not reformat vendored files.

The subset contains `signalsmith-stretch.h` and its portable backend dependencies,
`signalsmith-linear/{stft.h,fft.h}`. Tests, CLI/web examples, platform FFT backends and unused
Linear APIs are omitted. No Accelerate/PFFFT/FFTW/IPP/xsimd backend is enabled; no external FFT
library is required. The wrapper uses Default with unsplit computation, unity pitch and a fixed
seed. It has no transpose/formant/quality-mode controls.

`crates/komponist-stretch/build.rs` compiles C++17 at optimization level 3 even for dev/test. It
uses exception handling (caught at every fallible C ABI entry), precise floating point, no
fast-math and no `-march=native`. `cc` selects the platform compiler/C++ standard library:
GCC/Clang + libstdc++ on Linux, Xcode Clang + libc++ on macOS, MSVC + its runtime on Windows.
Builds use local sources and perform no network access. Native platform validation is documented
in the WP-B internal notes; support is not inferred merely from a cross Rust target being installed.
