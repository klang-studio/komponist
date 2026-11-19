# kp-test-sc

Tests discovery, channel routing and timing of an external plugin's sidechain.

## Structure

- [src/lib.rs](src/lib.rs) — Defines the main stereo input and output, an auxiliary stereo input,
  the `level` parameter and both format exports.

## How it works

The output is the main input plus the sidechain scaled by `level` , without added latency. At the
default 0 dB the sidechain is added at unity. CLAP exposes it as a second input port and VST3 as an
auxiliary bus.

See the [test workspace README](../README.md) for fixture setup.
