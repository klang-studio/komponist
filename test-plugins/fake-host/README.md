# fake-host

Provides an in-process test harness for the nice-plug fixtures.

## Structure

- [src/lib.rs](src/lib.rs) — Implements `FakeHost` , the test processing context and helpers for
  constructing note events.

## How it works

The harness activates a plugin, prepares buffers and note events, then invokes its processing
callbacks directly with an allocation guard. Unit tests use it to exercise DSP and plugin behavior
without loading a CLAP or VST3 bundle.

See the [test workspace README](../README.md) for fixture setup.
