# komponist-builtin

Adapts the built-in instruments, audio effects and MIDI effects to komponist's engine and device
cards. These processors run in the app process.

## Structure

- [src/processor.rs](src/processor.rs) — Adapts the Hexa FM synth to the engine processor
  interface.
- [src/device.rs](src/device.rs) — Maps Hexa card parameters to patch fields and defines its
  layout.
- [src/internal/](src/internal/) — Defines Volt, audio effects, MIDI effects and shared device
  specifications.
- [src/sampler/](src/sampler/) — Adapts sampler-core and manages sample loading and links.
- [src/state.rs](src/state.rs) — Encodes and decodes Hexa patch state.

## How it works

The app creates a processor from the saved device state and activates it at the engine's rate.
Audio processors apply parameter changes at event offsets. MIDI effects transform events before an
instrument. Device specifications describe the controls the app shows. The Sampler loads linked
samples off the audio thread and shares buffers through the audio file pool.
