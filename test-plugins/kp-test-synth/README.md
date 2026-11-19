# kp-test-synth

Provides a predictable instrument and a small editor for CLAP and VST3 hosting tests.

## Structure

- [src/lib.rs](src/lib.rs) — Defines the instrument, gain and decay parameters, and both format
  exports.
- [src/voices.rs](src/voices.rs) — Manages a fixed pool of sine voices.
- [src/editor.rs](src/editor.rs) — Provides the minimal egui gain slider for embedding tests.

## How it works

Note events start and release sine voices. Velocity sets each voice's initial level. The held
envelope falls by 60 dB over the decay time, and release falls by 60 dB over 10 ms. The instrument
sums voices, applies gain and sends the same signal to both channels. The editor exercises
parameter changes through the same gain control.

See the [test workspace README](../README.md) for fixture setup.
