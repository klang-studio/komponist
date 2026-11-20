# sampler-core

The realtime engine behind komponist's built-in **Klopfer** and **Sampler**: voices, pads, layers, envelopes, filters,
modulation, interpolation, slicing and time stretching. GPL-3.0-or-later.

Provenance: copied from the `dsp` crate of klopfer (commit `190581c`) on 2026-11-15 by klopfer's author, who licenses
the copy under GPL-3.0-or-later. klopfer continues as a separate plugin project; this copy is developed independently
since.

## Structure

- [src/engine.rs](src/engine.rs) — Owns pads, samples and the voice pool.
- [src/pad.rs](src/pad.rs) — Defines pad settings and per-pad processing parameters.
- [src/layers.rs](src/layers.rs) — Maps triggered pads to sample layers.
- [src/voice.rs](src/voice.rs) — Renders active sample voices.
- [src/sample.rs](src/sample.rs) — Defines the sample data passed to the engine.
- [src/slicing.rs](src/slicing.rs) — Computes slice boundaries.
- [src/stretch.rs](src/stretch.rs) — Implements sampler time stretching.

## How it works

The caller supplies decoded samples and configures pads and layers before playback. A triggered pad
selects its layers and starts voices in a fixed pool. Each voice reads sample data, applies
interpolation, envelopes, filters and modulation, then contributes to an output. The rendering path
uses preallocated state. The [shared loader](../src/load.rs) hands samples to
[Klopfer](../src/lib.rs) and the [general sampler](../../general-sampler/src/lib.rs).
