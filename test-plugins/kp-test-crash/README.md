# kp-test-crash

Exercises crash detection and recovery for an isolated plugin bridge.

## Structure

- [src/lib.rs](src/lib.rs) — Defines the stereo passthrough effect, its `crash` parameter, the
  trigger logic and both format exports.

## How it works

The process callback deliberately terminates its hosting process on note-on key 127 or when `crash`
crosses the threshold from below. State loaded before activation establishes the baseline for the
threshold check. Bridge tests use the fixture to verify that the DAW can survive and reload a
failed instance.

See the [test workspace README](../README.md) for fixture setup.
