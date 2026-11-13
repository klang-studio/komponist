# Test plugins

Fixtures for komponist's plugin hosting, IPC and crash handling (PLAN WP-27). Each plugin is built
with [nice-plug](https://codeberg.org/RustAudio/nice-plug) 0.4.2 from crates.io and exported as
CLAP and VST3 from one source.

This is a Cargo workspace of its own, excluded from komponist's, so that nice-plug's egui (0.36) and
baseview never meet the DAW's.

```bash
scripts/build-test-plugins.sh   # from the komponist root: release build, then copies the bundles to
                                # target/test-plugins/clap/<name>.clap and
                                # target/test-plugins/vst3/<name>.vst3
cd test-plugins && cargo test   # unit tests through fake-host (no real host needed)
```

`cargo xtask bundle [-p <package>]... [--release]` bundles into `test-plugins/target/bundled/`. The
xtask calls nice-plug-xtask's build and bundle steps itself, because `nice_plug_xtask::main()`
would take komponist's root as the workspace root.

## Plugins

Vendor `klang-studio`. VST3 class IDs are the 16 ASCII bytes shown. Parameters have no smoothing,
and `SAMPLE_ACCURATE_AUTOMATION` is off: a change applies exactly at the next block. CLAP hosts see
continuous parameters as normalized values in `[0, 1]` (nice-plug's convention).

| Bundle | CLAP ID / VST3 class ID | Audio, notes | Parameters (ID: range, default) | Behaviour |
|---|---|---|---|---|
| `kp-test-synth` | `de.klang-studio.kp-test-synth` / `kpTestSynth00001` | 0 in, 2 out; note input | `gain`: −60…0 dB, −12 dB · `decay`: 0.01…10 s (skewed), 1 s | One sine per key (16 voices, the oldest is stolen), level = velocity, falls by 60 dB within `decay` while held and within 10 ms after the note-off. Same signal on both channels. State = the two parameters. Editor: 240 × 80 logical px, fixed size, window title `kp-test-synth`, a label and a slider for `gain`. |
| `kp-test-fx` | `de.klang-studio.kp-test-fx` / `kpTestFx00000001` | 2 in, 2 out | `gain`: −60…+12 dB, 0 dB | Output = input delayed by 64 samples × gain. Reports a latency of 64 samples and really has it, so latency compensation lines up. |
| `kp-test-crash` | `de.klang-studio.kp-test-crash` / `kpTestCrash00001` | 2 in, 2 out (passthrough); note input | `crash`: 0…1, 0 | In `process`, writes through a null pointer (SIGSEGV) on a note-on of key 127, or when `crash` rises past 0.9 (below 0.9 at the previous block or at activation, ≥ 0.9 now). A state with `crash` ≥ 0.9 loaded before activation does not crash; loaded while active, it does. |
| `kp-test-slow` | `de.klang-studio.kp-test-slow` / `kpTestSlow000001` | 2 in, 2 out (passthrough); note input | none | Sleeps 50 ms inside `process` for every note-on of key 126 (longer than a block of 2048 samples at 48 kHz). |

The bundles are release builds. Debug builds (`cargo xtask bundle`) abort on allocations inside
`process` (nice-plug's `assert_process_allocs`).

## clap-validator 0.4.1

`kp-test-synth` and `kp-test-fx` pass. The expected failures of the other two:

- `kp-test-crash` crashes (SIGSEGV) in the ten tests that send random notes (key 127 among them) or
  random/extreme parameter values: `process-audio-denormals`, `process-note-inconsistent`,
  `process-note-out-of-place-basic`, `process-note-wildcard`, `process-random-block-sizes`,
  `process-sleep-process-status`, `process-varying-block-sizes`, `process-varying-sample-rates`,
  `param-fuzz-basic`, `param-fuzz-bounds`.
- `kp-test-slow`: `param-conversions` crashes with "attempt to divide by zero. This is a bug in the
  validator": the test divides by the parameter count (`4000usize.div_ceil(param_info.len())`), and
  this plugin has none. The note tests pass, they do not measure time.

## CLAP note-name

nice-plug 0.4.2 does not implement `clap.note-name` (the CLAP wrapper's `get_extension` does not know
it, and the VST3 wrapper's `hasProgramPitchNames` says "TODO: Support note names once someone
requests it"). So `kp-test-synth` has no note names. Adding it to nice-plug (on the klang-studio fork,
offered upstream) would take:

1. **API.** Note names must be readable on the main thread while the audio thread owns the plugin,
   and klopfer's names change when a kit loads. So like `params()`, a plugin hands out a shared
   object once at creation, e.g. `ClapPlugin::note_names(&self) -> Option<Arc<dyn NoteNames>>`, with
   `trait NoteNames: Send + Sync { fn note_names(&self) -> Vec<NoteName> }` and
   `NoteName { key: u8, channel: Option<u8>, name: String }`.
2. **Change signal.** A `note_names_changed()` on the GUI, activate and process contexts that queues
   a new `Task::NoteNamesChanged`; the main-thread task executor then calls the host's
   `clap_host_note_name.changed()`.
3. **CLAP wrapper** (`src/wrapper/clap/wrapper.rs`): a `clap_plugin_note_name { count, get }` field,
   returned from `get_extension` for `CLAP_EXT_NOTE_NAME` when the plugin has note names; `count`
   snapshots the names into a cached `Vec<clap_note_name>` (port 0, channel −1 for all channels), `get`
   reads from that snapshot; query `clap_host_note_name` in `init()` next to `clap_host_latency`.
   clap-sys 0.5 already has the types. Roughly 150 lines plus tests.
4. **VST3** (optional): pitch names hang off a program list in `IUnitInfo`, so the wrapper would need
   one program list with one program, `hasProgramPitchNames` / `getProgramPitchName`, and
   `IUnitHandler::notifyProgramListChange` on changes. About 100 more lines; some hosts show program
   lists as presets.

Once that exists, `kp-test-synth` gets names for keys 36–43 to test the host side (WP-54).
