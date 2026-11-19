# kp-test-fx

Provides a deterministic effect for plugin latency compensation and parameter tests.

## Structure

- [src/lib.rs](src/lib.rs) — Defines the effect, its `gain` parameter, delay storage and CLAP/VST3
  exports.

## How it works

The processor delays both channels by 64 samples and applies gain. It reports that same latency to
the host, so tests can compare the compensated signal with an undelayed reference.

See the [test workspace README](../README.md) for fixture setup.
