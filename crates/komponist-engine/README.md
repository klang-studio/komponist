# komponist-engine

Turns project snapshots into playable audio. It owns transport, sequencing, routing, mixing,
recording and offline rendering. UI, hardware backends and plugin formats connect through separate
crates.

## Structure

- [src/compile.rs](src/compile.rs) — Compiles the project into an engine program.
- [src/api.rs](src/api.rs) — Defines commands, events, telemetry and the engine handle.
- [src/engine.rs](src/engine.rs) — Processes blocks, manages processor slots and drives transport.
- [komponist-processor](../komponist-processor/src/lib.rs) — Defines audio and MIDI processing contracts.
- [src/graph.rs](src/graph.rs) — Processes routing, taps and aligned inputs.
- [src/workers.rs](src/workers.rs) — Schedules independent jobs in parallel.
- [src/audio/](src/audio/) — Handles clip playback and background read-ahead.
- [src/recording.rs](src/recording.rs) — Queues captured input and writes recordings on a worker.
- [src/export.rs](src/export.rs) — Renders and writes WAV output.

## How it works

The compiler prepares sequencer data, processor ordering, routing and latency compensation outside
the callback. The engine consumes commands and events, runs MIDI stages and track/bus jobs, then
mixes the master. Realtime work uses bounded queues and preallocated buffers. Offline export uses
the same render path with synchronous audio read-ahead.
