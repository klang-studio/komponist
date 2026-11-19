# komponist-hosting

Implements the bridge's CLAP and VST3 host layer. It exposes a shared interface for bundle
inspection, plugin lifecycle, audio, state, parameters and editors.

## Structure

- [src/plugin.rs](src/plugin.rs) — Defines the main-thread and realtime hosting interfaces.
- [src/clap/](src/clap/) — Adapts CLAP through clack-host.
- [src/vst3/](src/vst3/) — Adapts VST3 components and controllers through the vst3 bindings.
- [src/host.rs](src/host.rs) — Defines callbacks and services that formats receive from the bridge.
- [src/host_loop.rs](src/host_loop.rs) — Provides event-loop helpers for host callbacks.
- [src/library.rs](src/library.rs) — Loads native plugin libraries.

## How it works

The format layer loads or scans a bundle, initializes a plugin and exposes its metadata. After
activation the realtime adapter translates audio buffers and timestamped events, including a
sidechain if present. Main-thread adapters handle state, GUI requests, timers and callbacks. The
bridge owns process isolation and IPC.
