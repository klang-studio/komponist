//! Shared arrangement and clip automation controls and editing.

mod chooser;
mod curve;
mod metrics;

use chooser::Target;
pub(crate) use chooser::{
    Catalog, Choice, Chooser, automation_controls, chooser_button, mode_chip, mode_names,
    percentage, toggle,
};
pub(crate) use curve::{Editor, LaneFrame, Override, Paint};
pub(crate) use metrics::{CHIP_HEIGHT, MODE_WIDTH, RECENT_TARGETS, TOGGLE_WIDTH};

use komponist_model::automation::{
    AutomationMode, AutomationOwner, AutomationTarget, LaneId, Points, TrackAutomation,
};
use komponist_model::{ClipId, Project};

/// Which curve is edited. The arrangement owns a stable lane ID, while a clip addresses its
/// envelope by target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// An arrangement lane.
    Lane(AutomationOwner, LaneId),
    /// An envelope on a MIDI or step clip.
    Clip(ClipId, AutomationTarget),
}

/// Actions shared by arrangement, clip editor and parameter controls.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Show or hide all the lanes of this channel.
    Shown(AutomationOwner, bool),
    /// Add or reveal a target. An empty lane may be reused.
    ShowTarget {
        /// Track or master.
        owner: AutomationOwner,
        /// Empty lane to retarget, when applicable.
        replace: Option<LaneId>,
        /// The automated parameter.
        target: AutomationTarget,
        /// Last known name of the parameter.
        label: String,
    },
    /// Remove a lane or clip envelope.
    Remove(Source),
    /// Delete an arrangement lane's points.
    Clear(AutomationOwner, LaneId),
    /// Set a lane's stored height.
    Height(AutomationOwner, LaneId, u16),
    /// Set a lane's mode.
    Mode(AutomationOwner, LaneId, AutomationMode),
    /// Set all existing lanes' mode.
    AllModes(AutomationOwner, AutomationMode),
    /// Replace a curve, with the undo message for the actual edit.
    Points(Source, Points, &'static str),
    /// Add a clip envelope and ensure the target has a track lane.
    AddClip(ClipId, AutomationTarget, String),
}

/// The arrangement lanes of a track or the master.
pub fn lanes(project: &Project, owner: AutomationOwner) -> Option<&TrackAutomation> {
    match owner {
        AutomationOwner::Track(id) => project.track(id).map(|track| &track.automation),
        AutomationOwner::Master => Some(&project.master_automation),
    }
}
