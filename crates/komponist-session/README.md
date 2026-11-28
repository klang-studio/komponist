# komponist-session

Owns the open document, its optional project path and the live engine connection on the control
thread. The crate can run without a window or an egui dependency.

## Structure

- [src/session.rs](src/session.rs) — Owns `Document` and `EngineSync` and handles project
  replacement, saved-document adoption and synchronization with the engine.
- [src/engine_sync.rs](src/engine_sync.rs) — Compiles document revisions and playback drafts,
  manages processor slots and coordinates audio and MIDI I/O.
- [src/device_host.rs](src/device_host.rs) — Defines the host capabilities used by the session.
  `PluginHost` runs verified bundled CLAP devices directly and hosts external plugins in bridge
  processes.
- [src/project_io.rs](src/project_io.rs) — Loads and validates project files, reports repairs,
  resolves project paths and atomically publishes saves.
- [src/recording/](src/recording/) — Captures MIDI laps and automation segments, builds live
  previews and prepares document edits. The app supplies localized MIDI take and clip names
  and coordinates transport events, audio capture and take finalization.

## Project lifecycle

`Session::replace_project` stops playback, resets the engine and clears the previous project's
runtime devices and events before installing the new document and its path. The project directory
becomes the base for relative media and plugin assets. After a save publishes the file, the caller
passes the rebased document to `Session::accept_saved_project`. The session keeps the running
program and processors in place, updates the path and synchronizes changed asset links with the
host. `save_project_reporting` distinguishes a failed write from a published file whose directory
entry could not be flushed.

`Session::sync` publishes the document revision with the current automation playback draft.
`Session::sync_committed` publishes the document for offline rendering. `EngineSync` exposes engine
and device event polling. The application handles those events, processes queued actions and calls
session sync in its frame loop. `App::step` drives that path without a window. [Application crate](../komponist-app/README.md)

## Headless test

From the workspace root, run the project lifecycle unit test with the null backend.

```sh
cargo test --locked -p komponist-session session::tests::
```
