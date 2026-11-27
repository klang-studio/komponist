# komponist-ipc

Defines the shared memory, control messages and process monitoring used by the app and its plugin
bridges.

## Structure

- [src/shm.rs](src/shm.rs) — Defines the shared layout and its app and bridge access roles.
- [src/ring.rs](src/ring.rs) — Transfers bounded event lists through single-producer,
  single-consumer rings.
- [src/signal.rs](src/signal.rs) — Provides deadline waits and wakes.
- [src/control.rs](src/control.rs) — Defines framed control messages using shared extension
  and frame types from [komponist-plugin-api](../komponist-plugin-api/README.md).
- [src/process.rs](src/process.rs) — Watches bridge exits.

## How it works

For each block the app writes audio and events and advances a request sequence word. The bridge
reads the request, processes the block and publishes completion. Sequence checks distinguish
current results from stale ones. A separate postcard channel carries state, lifecycle and GUI
messages over Unix sockets or Windows named pipes. The underlying memory and wake primitives are
platform-specific.
