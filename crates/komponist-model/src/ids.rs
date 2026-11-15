//! Stable identifiers for the objects of a project.
//!
//! All ids come from one counter, [`Project::next_id`](crate::Project::next_id), so an id is unique
//! across kinds within a project and stays the same across undo and redo (ARCHITECTURE §5.2). Ids are
//! never reused within a project. They are persisted as plain JSON numbers.
//!
//! Ids identify document objects only. Runtime handles (engine processor slots, MIDI ports, CLAP
//! note ids) have their own types in the crates that own them.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

id_type! {
    /// Identifies a [`Track`](crate::Track) within its project.
    TrackId
}

id_type! {
    /// Identifies a [`Clip`](crate::Clip) within its project, independent of the track that holds
    /// it (moving a clip to another track keeps its id).
    ClipId
}

id_type! {
    /// Identifies a [`Note`](crate::Note) within its project. Copied notes (Alt-drag, duplicate,
    /// split) get new ids.
    NoteId
}

id_type! {
    /// Identifies a [`Device`](crate::Device) (instrument or effect) within its project. The engine
    /// maps each device to one processor slot, so a new device id always means a new processor.
    DeviceId
}

id_type! {
    /// Identifies a send ([`TrackSend`](crate::TrackSend)) within its project. The mixer's send
    /// controls and the engine's send buffers follow this id, so reordering or retargeting a send
    /// keeps it (ARCHITECTURE §6.8).
    SendId
}
