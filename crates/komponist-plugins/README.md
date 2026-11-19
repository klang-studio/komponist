# komponist-plugins

Manages plugin discovery and bridge processes on the app side. It supplies the engine processor
that sends audio and events to a hosted plugin.

## Structure

- [src/catalog.rs](src/catalog.rs) — Combines scanned bundles with the built-in catalog and defines
  the scan cache.
- [src/scan.rs](src/scan.rs) — Discovers bundles and runs background scan processes.
- [src/manager.rs](src/manager.rs) — Tracks device lifecycle, state and app notifications.
- [src/bridge.rs](src/bridge.rs) — Spawns and monitors an instance process and exchanges control
  messages.
- [src/external.rs](src/external.rs) — Adapts the engine's processor interface to shared-memory
  bridge requests.
- [src/params.rs](src/params.rs) — Converts parameter metadata to the model's representation.

## How it works

A scan reuses cached results for unchanged bundles and launches a separate process for bundles that
need inspection. Loading a device starts an instance bridge and restores saved state. During
processing, the external processor passes through an effect's audio or silences an instrument when
its output is unavailable. The manager surfaces failures and can reload the instance.
