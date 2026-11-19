# komponist-bridge

Runs foreign plugin code in a separate process for each instance. The app invokes the library
through `komponist --bridge` . A standalone bridge binary provides the same entry point for tests
and tools.

## Structure

- [src/instance.rs](src/instance.rs) — Handles instance lifecycle and control messages.
- [src/rt.rs](src/rt.rs) — Processes audio requests through shared memory.
- [src/scan.rs](src/scan.rs) — Examines a plugin bundle and writes descriptor JSON.
- [src/frame/](src/frame/) — Implements bridge-owned plugin frames on macOS and Windows.
- [src/event_loop.rs](src/event_loop.rs) — Provides the Linux bridge event loop, alongside platform
  loops for macOS and Windows.

## How it works

In instance mode the main thread handles control messages and plugin GUI work. An audio thread
waits for a shared-memory request, runs the hosted processor and signals completion. The app owns
the outer frame for X11 editors on Linux. Scan mode examines a bundle in a short-lived process and
keeps plugin output separate from descriptor JSON.
