# hexa-core

The 6-operator FM engine behind **hexa**, komponist's built-in synth. GPL-3.0-or-later.

Provenance: copied from the `hexa-core` crate of pisynth (commit `b621aec`) on 2026-11-15 by pisynth's author, who
licenses the copy under GPL-3.0-or-later. pisynth continues as a separate hardware-synth project; this copy is
developed independently since.

## Structure

- [src/synth.rs](src/synth.rs) — Provides the synth API used by the built-in device.
- [src/engine/](src/engine/) — Runs synthesis and voice rendering.
- [src/patch.rs](src/patch.rs) — Defines patch data.
- [src/operator.rs](src/operator.rs) — Implements FM operators.
- [src/modulation.rs](src/modulation.rs) — Defines modulation routes.
- [src/presets/](src/presets/) — Contains factory patches.

## How it works

A caller loads a patch and supplies note and control changes through the synth API. The engine
allocates voices from fixed storage and renders the patch's operator algorithm, envelopes,
modulation and effects into caller-provided audio buffers. The DSP supports `no_std` . The optional
`alloc` and `serde` features add boxed construction and patch serialization for the DAW.
