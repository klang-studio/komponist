# Fuzz targets

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the parsers of untrusted input
(#230): what a bridge process, a plugin, a project file or a file on disk hands to komponist.
This folder is its own Cargo workspace, outside the komponist build; libFuzzer needs a nightly
toolchain.

| Target | Input |
|---|---|
| `control_decode` | a control message payload (postcard) from the bridge or the app |
| `control_stream` | the raw byte stream a bridge writes into the control socket (frames, fragments) |
| `wire_event` | one event slot of a shared-memory ring |
| `native_layout` | a plugin's native layout descriptor (JSON) |
| `plugin_messages` | sampler snapshot, waveform markers, device assets and edits, device canvas, pad activity |
| `vst3_state` | a saved VST3 state blob (`KPV3`) |
| `vst3_stream` | the `IBStream` calls a VST3 plugin makes on the host's stream |
| `audio_file` | an audio file (WAV, AIFF, FLAC, OGG Vorbis, MP3): reads, seek, whole-file decode, peaks |
| `peak_cache` | a peak cache record (`KPK3`) |
| `resampler` | the stream resampler at a rate from an untrusted file header |

The harnesses are in the crates (the `fuzzing` modules of komponist-audiofile, -plugin-api and
-hosting; `check_payload`, `receive_hostile_stream` and `check_wire_event` in komponist-ipc). The
proptests of `cargo test --workspace` run the same harnesses on mutated fixtures, so a fix for a
fuzz finding gets its regression test there.

## Running

From the repository root, with cargo-fuzz installed (`cargo install cargo-fuzz`, or ad hoc with
`nix shell nixpkgs#cargo-fuzz`):

```bash
cargo run --manifest-path fuzz/Cargo.toml --bin seed-corpora   # fuzz/corpus/<target>/ from the fixtures
cd fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run native_layout -- -max_total_time=120
```

`cargo fuzz run` adds what it finds to `fuzz/corpus/<target>/` and writes a crashing input to
`fuzz/artifacts/<target>/`; replay one with `cargo +nightly fuzz run <target> <file>`. Both folders
and `fuzz/target/` are ignored by git. To keep the build output elsewhere (komponist's `target/`
symlink, say), set `CARGO_TARGET_DIR`. `audio_file` writes each input to a file in the temporary
folder (`TMPDIR`).

The targets build with debug assertions and AddressSanitizer (cargo-fuzz's defaults). Running
them in CI is #229.
