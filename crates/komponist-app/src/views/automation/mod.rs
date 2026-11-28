//! Shared arrangement and clip automation controls and editing.

mod chooser;
mod clip;
mod curve;
mod metrics;

pub(crate) use chooser::{
    Catalog, Choice, Chooser, Target, chooser_button, macro_label, mode_chip, mode_chip_width,
    mode_names, percentage, toggle,
};
pub(crate) use clip::{ClipEditor, toolbar_button};
pub(crate) use curve::{Editor, LaneFrame, Override, Paint};
pub(crate) use metrics::{CHIP_HEIGHT, RECENT_TARGETS, TOGGLE_WIDTH};

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
    /// Hide one arrangement lane in the current view.
    HideLane(LaneId),
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

/// The value shown by a control with an active automation lane.
pub(crate) fn automation_controls(
    ctx: &super::ViewCtx<'_>,
    owner: AutomationOwner,
    target: &AutomationTarget,
) -> Option<f32> {
    automation_control_value(ctx.project, ctx.engine, owner, target)
}

/// The current engine override or timeline value shared by views and edits.
pub(crate) fn automation_control_value(
    project: &Project,
    engine: &komponist_session::engine_sync::EngineSync,
    owner: AutomationOwner,
    target: &AutomationTarget,
) -> Option<f32> {
    if let AutomationOwner::Track(id) = owner
        && project.track(id)?.macros.driver(target).is_some()
    {
        return None;
    }
    let lane = lanes(project, owner)?.lane(target)?;
    if lane.mode == AutomationMode::Off {
        return None;
    }
    if let Some(value) = engine
        .program()
        .and_then(|program| program.automation_override(owner, target))
    {
        return Some(value);
    }
    let tick = engine.playhead();
    match owner {
        AutomationOwner::Track(id) => {
            komponist_model::automation::effective_value_at(project.track(id)?, target, tick)
        }
        AutomationOwner::Master => {
            komponist_model::automation::value_at(lane.points.as_slice(), tick)
        }
    }
}
