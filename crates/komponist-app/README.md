# komponist-app

The application entry point and the layer that connects the egui interface, project document, audio
engine and plugin manager. The `komponist` binary starts the desktop app or dispatches `--bridge`
to the plugin bridge. The library also supports headless acceptance tests.

## Structure

- [src/app.rs](src/app.rs) — Owns the session and UI state and dispatches actions.
- [src/actions.rs](src/actions.rs) — Defines edit, transport and UI actions shared by views and
  keybindings.
- [src/views/](src/views/) — Implements the arrangement, clip editors, mixer, browser and device
  views.
- [komponist-session](../komponist-session/README.md) — Owns the document and its path, synchronizes
  changes to the engine and provides packaged and bridged processors.
- [src/project_io.rs](src/project_io.rs) — Coordinates the project lifecycle, linked assets and
  recently opened projects. [Session project I/O](../komponist-session/src/project_io.rs) handles
  file loading and publication.

## How it works

Views and shortcuts produce actions that update the document, change UI state or send engine
commands. Structural edits produce a new engine program. Live controls can send parameter and mixer
commands during a gesture. The UI reads telemetry for meters and transport, while recording and
export coordinate with dedicated workers.
