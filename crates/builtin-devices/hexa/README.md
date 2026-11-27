# komponist-hexa

**Hexa** is komponist's built-in six-operator FM synthesizer. Its [DSP core](core/README.md)
contains the synth engine and factory patches. This crate connects that engine to the host,
defines the device card and macros, and encodes project state as patch JSON.

- [src/processor.rs](src/processor.rs) applies note and parameter events at their sample offsets.
- [src/device.rs](src/device.rs) defines the four-page card and maps its controls to patch fields.
- [src/macros.rs](src/macros.rs) defines the eight macros and their stable wire IDs.
- [src/state.rs](src/state.rs) handles saved patches and provides the factory preset bank.
