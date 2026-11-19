# komponist-model

Defines the persistent project and its editing rules. This crate supplies musical time, routing,
undo, parameter descriptions and project serialization independently of the UI and engine.

## Structure

- [src/project.rs](src/project.rs) — Defines tracks, clips, devices, channels and routing fields.
- [src/time.rs](src/time.rs) — Defines musical positions, tempo and sample conversions.
- [src/audio.rs](src/audio.rs) — Models linked audio, takes, fades and input settings.
- [src/warp.rs](src/warp.rs) — Maps musical positions to source frames for Warp clips.
- [src/document.rs](src/document.rs) — Owns the current snapshot and undo/redo history.
- [src/edits/](src/edits/) — Implements pure, fallible project edits.
- [src/routing.rs](src/routing.rs) — Validates routing and computes dependencies and mute/solo
  gates.
- [src/persist.rs](src/persist.rs) — Reads, migrates and writes versioned project files.

## How it works

An edit builds a new project by cloning only the changed paths of an `Arc` -backed project tree.
The document stores old roots for undo and groups continuous changes into one gesture. Edits and
loading enforce track content and routing constraints. Saved projects link to audio sources and
store plugin identity and state.
