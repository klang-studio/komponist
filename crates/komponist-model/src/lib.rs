//! The komponist project document.
//!
//! Musical time (ticks at 960 PPQ, tempo and meter maps), the project tree (tracks, clips, notes,
//! steps, devices), edits with undo and redo, serde persistence and the pure formatting functions
//! the UI shows. Depends on no other studio crate.
//!
//! Module map (ARCHITECTURE §5):
//! - [`audio`]: audio files, audio clips, audio track input and monitoring (epic #8).
//! - [`warp`]: optional affine Warp state and exact native frame/tick mapping (#14).
//! - [`paths`]: lexical path helpers for file links.
//! - [`ids`]: stable ids from one per-project counter.
//! - [`time`]: [`Tick`], [`Tempo`], [`Meter`], [`TickRange`], [`TempoMap`], grids.
//! - [`project`]: the document tree ([`Project`], [`Track`], [`Clip`], [`Device`], …).
//! - [`document`]: [`Document`] with undo, redo and gestures.
//! - [`edits`]: every change as a pure function.
//! - [`format`](mod@format): display strings (`10.2.120`, `−3.2`, `L 12`, `C4`).
//! - [`params`]: the parameter model (`ParamInfo`, scales, units, context placeholders).
//! - [`persist`]: the `*.komponist` JSON file.
//! - [`routing`]: the routing graph (outputs, sends, sidechains, groups): checks, valid targets,
//!   topological order, solo and mute gates (ARCHITECTURE §6.8).
//! - [`fixtures`]: the demo project "Night Drive" and the empty project.
//!
//! Threads: everything here is plain data and pure functions, used on the UI thread and on workers.
//! Nothing in this crate runs on the audio thread.

pub mod audio;
pub mod automation;
pub mod document;
pub mod edits;
pub mod fixtures;
pub mod format;
pub mod ids;
pub mod macros;
pub mod params;
pub mod paths;
pub mod persist;
pub mod project;
pub mod routing;
pub mod time;
pub mod warp;

#[cfg(test)]
mod proptests;

pub use audio::{
    AudioClip, AudioFile, AudioInput, AudioIo, AudioTake, CLIP_GAIN_DB_MAX, CLIP_GAIN_DB_MIN,
    ClipTimeMode, EDGE_FADE_MS, HardwareInput, MonitorMode,
};
pub use document::{Document, Gesture, UNDO_LIMIT, UndoStep};
pub use edits::EditError;
pub use ids::{ClipId, DeviceId, LaneId, NoteId, SendId, TrackId};
pub use project::{
    Channel, ChannelRef, Clip, ClipContent, ClipKind, Device, DeviceRole, Hue, MidiChannelSel,
    MidiClip, MidiInputSel, Note, PluginFormat, PluginRef, Project, Sidechain, Step, StepClip,
    StepHit, StepRow, Take, Tap, Track, TrackKind, TrackOutput, TrackSend,
};
pub use time::{
    Bbt, DISPLAY_TICKS_PER_BEAT, Grid, Meter, PPQ, StepResolution, TICKS_PER_STEP, Tempo, TempoMap,
    Tick, TickRange,
};
pub use warp::{AudioWarp, MAX_WARP_TICK, WarpError, WarpMap};
