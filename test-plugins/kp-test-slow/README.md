# kp-test-slow

Exercises the bridge deadline and the engine's fallback when a plugin block finishes late.

## Structure

- [src/lib.rs](src/lib.rs) — Defines a stereo passthrough effect, the stall trigger and both format
  exports.

## How it works

A note-on for key 126 makes the process callback sleep for 50 ms. That exceeds the realtime budget
in bridge tests. Ordinary input passes through, so tests can check recovery on subsequent blocks.
A note-on for key 125 delays the next state snapshot by 600 ms on the bridge control thread. A note-on for key 124 makes
the next state snapshot change the `gain` parameter while it serializes.

See the [test workspace README](../README.md) for fixture setup.
