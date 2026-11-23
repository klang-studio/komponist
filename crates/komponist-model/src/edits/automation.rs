//! Pure document edits for automation. The caller chooses an undo label and groups drag gestures.

use std::collections::HashSet;
use std::sync::Arc;

use super::{EditError, EditResult, edit_clip, locate_track, track_mut, update_track};
use crate::automation::{
    AutomationMode, AutomationOwner, AutomationTarget, ClipAutomation, LANE_HEIGHT_DEFAULT,
    LANE_HEIGHT_MAX, LANE_HEIGHT_MIN, Lane, LaneId, Points, TrackAutomation,
};
use crate::{ClipId, ClipKind, DeviceId, Project, Track};

/// The lanes owned by a track or the master.
pub fn lanes(project: &Project, owner: AutomationOwner) -> Result<&TrackAutomation, EditError> {
    match owner {
        AutomationOwner::Track(id) => project
            .track(id)
            .map(|track| &track.automation)
            .ok_or(EditError::TrackNotFound(id)),
        AutomationOwner::Master => Ok(&project.master_automation),
    }
}

fn owner_mut(
    project: &mut Project,
    owner: AutomationOwner,
) -> Result<&mut TrackAutomation, EditError> {
    match owner {
        AutomationOwner::Track(id) => {
            let index = locate_track(project, id)?;
            Ok(&mut track_mut(project, index).automation)
        }
        AutomationOwner::Master => Ok(Arc::make_mut(&mut project.master_automation)),
    }
}

fn edit_owner(
    project: &Project,
    owner: AutomationOwner,
    edit: impl FnOnce(&mut TrackAutomation) -> Result<(), EditError>,
) -> EditResult {
    let mut next = project.clone();
    match owner {
        AutomationOwner::Track(id) => {
            let index = locate_track(project, id)?;
            update_track(&mut next, index, |track| edit(&mut track.automation))?;
        }
        AutomationOwner::Master => {
            let mut automation = (*project.master_automation).clone();
            edit(&mut automation)?;
            if automation != *project.master_automation {
                next.master_automation = Arc::new(automation);
            }
        }
    }
    Ok(next)
}

fn find_lane(automation: &TrackAutomation, lane: LaneId) -> Result<usize, EditError> {
    automation
        .lanes
        .iter()
        .position(|item| item.id == lane)
        .ok_or(EditError::Invalid("automation lane not found"))
}

fn edit_lane(
    project: &Project,
    owner: AutomationOwner,
    lane: LaneId,
    edit: impl FnOnce(&mut Lane),
) -> EditResult {
    edit_owner(project, owner, |automation| {
        let index = find_lane(automation, lane)?;
        edit(Arc::make_mut(&mut automation.lanes[index]));
        Ok(())
    })
}

/// Whether a target belongs to the track. Plugin parameter metadata is resolved at runtime.
pub fn track_has_target(track: &Track, target: &AutomationTarget) -> bool {
    match target {
        AutomationTarget::Param { device, .. } => {
            track.channel.devices.iter().any(|d| d.id == *device)
        }
        AutomationTarget::Send { send } => track.sends.iter().any(|s| s.id == *send),
        AutomationTarget::Macro { index } => usize::from(*index) < crate::macros::MACRO_COUNT,
        AutomationTarget::Volume | AutomationTarget::Pan => true,
    }
}

/// Whether a target belongs to the channel. A missing plugin still has a document device.
pub fn owner_has_target(
    project: &Project,
    owner: AutomationOwner,
    target: &AutomationTarget,
) -> bool {
    match owner {
        AutomationOwner::Track(id) => project
            .track(id)
            .is_some_and(|track| track_has_target(track, target)),
        AutomationOwner::Master => match target {
            AutomationTarget::Param { device, .. } => {
                project.master.devices.iter().any(|d| d.id == *device)
            }
            AutomationTarget::Volume | AutomationTarget::Pan => true,
            AutomationTarget::Send { .. } | AutomationTarget::Macro { .. } => false,
        },
    }
}

fn check_target(
    project: &Project,
    owner: AutomationOwner,
    target: &AutomationTarget,
) -> Result<(), EditError> {
    if owner_has_target(project, owner, target) {
        Ok(())
    } else {
        Err(EditError::Invalid(
            "automation target is not on the channel",
        ))
    }
}

fn new_lane(
    project: &mut Project,
    owner: AutomationOwner,
    target: AutomationTarget,
    label: String,
) -> Result<LaneId, EditError> {
    if let Some(lane) = lanes(project, owner)?.lane(&target) {
        return Ok(lane.id);
    }
    let id = LaneId(project.alloc_id()?);
    owner_mut(project, owner)?.lanes.push(Arc::new(Lane {
        id,
        target,
        label,
        mode: AutomationMode::Read,
        points: Points::default(),
        height: LANE_HEIGHT_DEFAULT,
    }));
    Ok(id)
}

/// Adds an empty Read lane or returns the existing lane of the target.
pub fn add_lane(
    project: &Project,
    owner: AutomationOwner,
    target: AutomationTarget,
    label: String,
) -> Result<(Project, LaneId), EditError> {
    check_target(project, owner, &target)?;
    let mut next = project.clone();
    let id = new_lane(&mut next, owner, target, label)?;
    Ok((next, id))
}

fn clip_uses_target(project: &Project, owner: AutomationOwner, target: &AutomationTarget) -> bool {
    match owner {
        AutomationOwner::Track(id) => project.track(id).is_some_and(|track| {
            track
                .clips
                .iter()
                .any(|clip| clip.automation.iter().any(|entry| &entry.target == target))
        }),
        AutomationOwner::Master => false,
    }
}

/// Deletes the lane, or clears only the arrangement points if a clip still uses the target.
pub fn remove_lane(project: &Project, owner: AutomationOwner, lane: LaneId) -> EditResult {
    edit_owner(project, owner, |automation| {
        let index = find_lane(automation, lane)?;
        if clip_uses_target(project, owner, &automation.lanes[index].target) {
            Arc::make_mut(&mut automation.lanes[index]).points = Points::default();
        } else {
            automation.lanes.remove(index);
        }
        Ok(())
    })
}

/// Clears the arrangement points while preserving the target, mode and lane settings.
pub fn clear_lane(project: &Project, owner: AutomationOwner, lane: LaneId) -> EditResult {
    set_lane_points(project, owner, lane, Points::default())
}

/// Assigns a new target. A clip still using the former target keeps an empty lane of its own.
pub fn retarget_lane(
    project: &Project,
    owner: AutomationOwner,
    lane: LaneId,
    target: AutomationTarget,
    label: String,
) -> EditResult {
    check_target(project, owner, &target)?;
    let automation = lanes(project, owner)?;
    if automation
        .lane(&target)
        .is_some_and(|existing| existing.id != lane)
    {
        return Err(EditError::Invalid("automation target already has a lane"));
    }
    let old = &automation.lanes[find_lane(automation, lane)?];
    let keep = old.target != target && clip_uses_target(project, owner, &old.target);
    let mut next = edit_lane(project, owner, lane, |lane| {
        lane.target = target;
        lane.label = label;
    })?;
    if keep {
        let id = LaneId(next.alloc_id()?);
        owner_mut(&mut next, owner)?.lanes.push(Arc::new(Lane {
            id,
            points: Points::default(),
            ..Lane::clone(old)
        }));
    }
    Ok(next)
}

/// Replaces the arrangement points of a lane.
pub fn set_lane_points(
    project: &Project,
    owner: AutomationOwner,
    lane: LaneId,
    points: Points,
) -> EditResult {
    edit_lane(project, owner, lane, |lane| lane.points = points)
}

/// Sets the mode of one lane.
pub fn set_lane_mode(
    project: &Project,
    owner: AutomationOwner,
    lane: LaneId,
    mode: AutomationMode,
) -> EditResult {
    edit_lane(project, owner, lane, |lane| lane.mode = mode)
}

/// Sets the mode of every existing lane on the channel in one edit.
pub fn set_all_modes(
    project: &Project,
    owner: AutomationOwner,
    mode: AutomationMode,
) -> EditResult {
    edit_owner(project, owner, |automation| {
        for lane in &mut automation.lanes {
            if lane.mode != mode {
                Arc::make_mut(lane).mode = mode;
            }
        }
        Ok(())
    })
}

/// Sets the lane height, clamped to the supported range.
pub fn set_lane_height(
    project: &Project,
    owner: AutomationOwner,
    lane: LaneId,
    height: u16,
) -> EditResult {
    edit_lane(project, owner, lane, |lane| {
        lane.height = height.clamp(LANE_HEIGHT_MIN, LANE_HEIGHT_MAX)
    })
}

/// Changes lane visibility. Hiding prunes empty Read lanes that no clip uses.
pub fn set_shown(project: &Project, owner: AutomationOwner, shown: bool) -> EditResult {
    edit_owner(project, owner, |automation| {
        automation.shown = shown;
        if !shown {
            automation.lanes.retain(|lane| {
                lane.mode != AutomationMode::Read
                    || !lane.points.is_empty()
                    || clip_uses_target(project, owner, &lane.target)
            });
        }
        Ok(())
    })
}

/// Adds an empty envelope in a MIDI or step clip, along with its arrangement lane if necessary.
pub fn add_clip_automation(
    project: &Project,
    clip: ClipId,
    target: AutomationTarget,
    label: String,
) -> EditResult {
    let (track, current) = project
        .find_clip(clip)
        .ok_or(EditError::ClipNotFound(clip))?;
    if current.kind() == ClipKind::Audio {
        return Err(EditError::Invalid(
            "audio clips do not have automation envelopes",
        ));
    }
    let owner = AutomationOwner::Track(track.id);
    check_target(project, owner, &target)?;
    let mut next = edit_clip(project, clip, |clip| {
        if !clip
            .automation
            .iter()
            .any(|existing| existing.target == target)
        {
            clip.automation.push(ClipAutomation {
                target: target.clone(),
                label: label.clone(),
                points: Points::default(),
                precedence: None,
            });
        }
        Ok(())
    })?;
    new_lane(&mut next, owner, target, label)?;
    Ok(next)
}

/// Deletes an entire clip envelope. Other envelopes and arrangement points are preserved.
pub fn remove_clip_automation(
    project: &Project,
    clip: ClipId,
    target: &AutomationTarget,
) -> EditResult {
    let (track, _) = project
        .find_clip(clip)
        .ok_or(EditError::ClipNotFound(clip))?;
    let track_id = track.id;
    let mut next = edit_clip(project, clip, |clip| {
        clip.automation
            .retain(|envelope| &envelope.target != target);
        Ok(())
    })?;
    prune_hidden_unused(&mut next, track_id)?;
    Ok(next)
}

/// Replaces the points of an existing clip envelope, in content-relative ticks.
pub fn set_clip_points(
    project: &Project,
    clip: ClipId,
    target: &AutomationTarget,
    points: Points,
) -> EditResult {
    edit_clip(project, clip, |clip| {
        let envelope = clip
            .automation
            .iter_mut()
            .find(|envelope| &envelope.target == target)
            .ok_or(EditError::Invalid("clip automation target not found"))?;
        envelope.points = points;
        Ok(())
    })
}

pub(super) fn prune_hidden_unused(
    project: &mut Project,
    id: crate::TrackId,
) -> Result<(), EditError> {
    let index = locate_track(project, id)?;
    update_track(project, index, |track| {
        if !track.automation.shown {
            track.automation.lanes.retain(|lane| {
                lane.mode != AutomationMode::Read
                    || !lane.points.is_empty()
                    || track.clips.iter().any(|clip| {
                        clip.automation
                            .iter()
                            .any(|entry| entry.target == lane.target)
                    })
            });
        }
        Ok(())
    })
}

pub(super) fn ensure_clip_lanes(project: &mut Project, index: usize) -> Result<(), EditError> {
    let track = &project.tracks[index];
    let owner = AutomationOwner::Track(track.id);
    let entries: Vec<_> = track
        .clips
        .iter()
        .flat_map(|clip| &clip.automation)
        .filter(|entry| {
            track.automation.lane(&entry.target).is_none() && track_has_target(track, &entry.target)
        })
        .map(|entry| (entry.target.clone(), entry.label.clone()))
        .collect();
    for (target, label) in entries {
        new_lane(project, owner, target, label)?;
    }
    Ok(())
}

pub(super) fn remove_device_lanes(
    project: &mut Project,
    owner: AutomationOwner,
    device: DeviceId,
) -> Result<(), EditError> {
    if lanes(project, owner)?
        .lanes
        .iter()
        .any(|lane| is_device_lane(lane, device))
    {
        owner_mut(project, owner)?
            .lanes
            .retain(|lane| !is_device_lane(lane, device));
    }
    Ok(())
}

fn is_device_lane(lane: &Lane, id: DeviceId) -> bool {
    matches!(lane.target, AutomationTarget::Param { device, .. } if device == id)
}

/// Takes the automation lanes of a device for Copy/Paste.
pub fn device_lanes(project: &Project, device: DeviceId) -> Vec<Arc<Lane>> {
    project
        .find_device(device)
        .and_then(|(channel, _, _)| lanes(project, channel.into()).ok())
        .map_or_else(Vec::new, |automation| {
            automation
                .lanes
                .iter()
                .filter(|lane| is_device_lane(lane, device))
                .cloned()
                .collect()
        })
}

/// Copies captured device lanes to an inserted device, assigning fresh globally unique ids.
pub fn paste_device_lanes(project: &Project, device: DeviceId, source: &[Arc<Lane>]) -> EditResult {
    let (channel, _, _) = project
        .find_device(device)
        .ok_or(EditError::DeviceNotFound(device))?;
    let owner = AutomationOwner::from(channel);
    let mut next = project.clone();
    for lane in source {
        let AutomationTarget::Param { param, .. } = &lane.target else {
            continue;
        };
        let target = AutomationTarget::Param {
            device,
            param: param.clone(),
        };
        if lanes(&next, owner)?.lane(&target).is_some() {
            continue;
        }
        let id = LaneId(next.alloc_id()?);
        owner_mut(&mut next, owner)?.lanes.push(Arc::new(Lane {
            id,
            target,
            ..Lane::clone(lane)
        }));
    }
    Ok(next)
}

pub(super) fn remap_duplicate_targets(
    project: &mut Project,
    original: &Track,
    duplicate: &mut Track,
) -> Result<(), EditError> {
    let remap = |target: &mut AutomationTarget| match target {
        AutomationTarget::Param { device, .. } => {
            if let Some(index) = original
                .channel
                .devices
                .iter()
                .position(|item| item.id == *device)
            {
                *device = duplicate.channel.devices[index].id;
            }
        }
        AutomationTarget::Send { send } => {
            if let Some(index) = original.sends.iter().position(|item| item.id == *send) {
                *send = duplicate.sends[index].id;
            }
        }
        _ => {}
    };
    for lane in &mut duplicate.automation.lanes {
        let lane = Arc::make_mut(lane);
        lane.id = LaneId(project.alloc_id()?);
        remap(&mut lane.target);
    }
    for clip in &mut duplicate.clips {
        if !clip.automation.is_empty() {
            for envelope in &mut Arc::make_mut(clip).automation {
                remap(&mut envelope.target);
            }
        }
    }
    for slot in &mut duplicate.macros.0 {
        for mapping in &mut slot.mappings {
            remap(&mut mapping.target);
        }
    }
    Ok(())
}

pub(crate) fn repair_loaded(
    project: &mut Project,
    repairs: &mut crate::validate::Repairs,
) -> Result<(), EditError> {
    fn repair_lanes(automation: &mut TrackAutomation, repairs: &mut crate::validate::Repairs) {
        let mut targets = HashSet::new();
        automation.lanes.retain(|lane| {
            let keep = targets.insert(lane.target.clone());
            if !keep {
                repairs.add(crate::validate::RepairKind::AutomationLanes, 1);
            }
            keep
        });
        for lane in &mut automation.lanes {
            let height = lane.height.clamp(LANE_HEIGHT_MIN, LANE_HEIGHT_MAX);
            if height != lane.height {
                Arc::make_mut(lane).height = height;
                repairs.add(crate::validate::RepairKind::MixerValues, 1);
            }
        }
    }
    if !project.master_automation.lanes.is_empty() {
        repair_lanes(Arc::make_mut(&mut project.master_automation), repairs);
    }
    for index in 0..project.tracks.len() {
        let track = &project.tracks[index];
        if !track.automation.lanes.is_empty()
            || track.clips.iter().any(|clip| !clip.automation.is_empty())
        {
            let track = track_mut(project, index);
            repair_lanes(&mut track.automation, repairs);
            for clip in &mut track.clips {
                if clip.automation.is_empty() {
                    continue;
                }
                let clip = Arc::make_mut(clip);
                let before = clip.automation.len();
                if clip.kind() == ClipKind::Audio {
                    clip.automation.clear();
                } else {
                    let mut targets = HashSet::new();
                    clip.automation
                        .retain(|envelope| targets.insert(envelope.target.clone()));
                }
                repairs.add(
                    crate::validate::RepairKind::AutomationLanes,
                    before - clip.automation.len(),
                );
            }
            let lanes = project.tracks[index].automation.lanes.len();
            ensure_clip_lanes(project, index)?;
            repairs.add(
                crate::validate::RepairKind::AutomationLanes,
                project.tracks[index]
                    .automation
                    .lanes
                    .len()
                    .saturating_sub(lanes),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::{Point, clip_points, value_at};
    use crate::edits;
    use crate::fixtures::{automation_project, demo_song};
    use crate::{Document, Tick};

    #[test]
    fn mode_and_point_edits_make_undoable_independent_lanes() {
        let mut history = Document::new(demo_song());
        let track = history.project().tracks[0].id;
        let owner = AutomationOwner::Track(track);
        let before = history.project().clone();
        let lane = history
            .apply_with("add", |p| {
                add_lane(p, owner, AutomationTarget::Volume, "Volume".into())
            })
            .unwrap();
        let (same, duplicate) = add_lane(
            history.project(),
            owner,
            AutomationTarget::Volume,
            "Volume".into(),
        )
        .unwrap();
        assert_eq!(duplicate, lane);
        assert_eq!(&same, history.project().as_ref());
        let automation = lanes(history.project(), owner).unwrap();
        assert_eq!(automation.lanes[0].mode, AutomationMode::Read);
        history
            .apply("height", |p| set_lane_height(p, owner, lane, 999))
            .unwrap();
        history
            .apply("mode", |p| {
                set_lane_mode(p, owner, lane, AutomationMode::Touch)
            })
            .unwrap();
        history
            .apply("bulk", |p| set_all_modes(p, owner, AutomationMode::Latch))
            .unwrap();
        history
            .apply("show", |p| set_shown(p, owner, true))
            .unwrap();
        history
            .apply("retarget", |p| {
                retarget_lane(p, owner, lane, AutomationTarget::Pan, "Pan".into())
            })
            .unwrap();
        history.begin_gesture("draw");
        for tick in [10, 20, 30] {
            history
                .apply("point", |p| {
                    set_lane_points(
                        p,
                        owner,
                        lane,
                        Points::new(vec![Point::linear(Tick(tick), 0.5)]),
                    )
                })
                .unwrap();
        }
        history.end_gesture();
        history
            .apply("clear", |p| clear_lane(p, owner, lane))
            .unwrap();
        history
            .apply("remove", |p| remove_lane(p, owner, lane))
            .unwrap();
        let after = history.project().clone();
        let mut count = 0;
        while history.undo() {
            count += 1;
        }
        assert_eq!(count, 9);
        assert!(Arc::ptr_eq(&before, history.project()));
        while history.redo() {}
        assert!(Arc::ptr_eq(&after, history.project()));
    }

    #[test]
    fn clip_only_lanes_survive_hiding_and_deleting_arrangement_points() {
        let p = automation_project();
        let track = &p.tracks[0];
        let clip = track.clips[0].id;
        let owner = AutomationOwner::Track(track.id);
        let target = AutomationTarget::Pan;
        let lane = track.automation.lane(&target).unwrap().id;
        let p = set_lane_points(
            &p,
            owner,
            lane,
            Points::new(vec![Point::linear(Tick(10), 0.2)]),
        )
        .unwrap();
        let p = remove_lane(&p, owner, lane).unwrap();
        let p = set_shown(&p, owner, false).unwrap();
        assert!(
            lanes(&p, owner)
                .unwrap()
                .lane(&target)
                .unwrap()
                .points
                .is_empty()
        );
        let p = remove_clip_automation(&p, clip, &target).unwrap();
        assert!(lanes(&p, owner).unwrap().lane(&target).is_none());
    }

    #[test]
    fn cross_track_clip_move_preserves_unresolved_envelope_without_recreating_removed_device_lanes()
    {
        let p = automation_project();
        let track = &p.tracks[2];
        let clip = &track.clips[0];
        let device = track.channel.instrument().unwrap().1.id;
        let target = clip.automation[0].target.clone();
        let clip_id = clip.id;
        let p = edits::move_clips(&p, &[clip_id], Tick::ZERO, -1).unwrap();
        let (other, moved) = p.find_clip(clip_id).unwrap();
        assert_eq!(moved.automation, clip.automation);
        assert!(other.automation.lane(&target).is_none());
        let p = edits::move_clips(&p, &[clip_id], Tick::ZERO, 1).unwrap();
        assert!(
            p.find_clip(clip_id)
                .unwrap()
                .0
                .automation
                .lane(&target)
                .is_some()
        );
        let p = edits::remove_device(&p, device).unwrap();
        let (track, moved) = p.find_clip(clip_id).unwrap();
        assert!(track.automation.lane(&target).is_none());
        assert_eq!(moved.automation, clip.automation);
        let reloaded = crate::persist::from_json(&crate::persist::to_json(&p).unwrap()).unwrap();
        assert_eq!(reloaded, p);
    }

    #[test]
    fn copying_a_track_remaps_lane_device_send_and_clip_targets() {
        let project = automation_project();
        let track = &project.tracks[2];
        let target = track.clips[0].automation[0].target.clone();
        let (p, copies) = edits::duplicate_tracks(&project, &[track.id]).unwrap();
        let copy = p.track(copies[0]).unwrap();
        let copy_target = &copy.clips[0].automation[0].target;
        assert_ne!(*copy_target, target);
        let before = track.automation.lane(&target).unwrap();
        let after = copy.automation.lane(copy_target).unwrap();
        assert_ne!(after.id, before.id);
        assert_eq!(after.points, before.points);
        assert!(track_has_target(copy, copy_target));
        assert!(
            copy.automation
                .lane(&AutomationTarget::Send {
                    send: copy.sends[0].id
                })
                .is_some()
        );
        let send = copy.sends[0].id;
        let p = edits::remove_send(&p, send).unwrap();
        assert!(
            p.track(copies[0])
                .unwrap()
                .automation
                .lane(&AutomationTarget::Send { send })
                .is_none()
        );
    }

    #[test]
    fn splitting_a_looped_clip_preserves_the_curve_in_each_half() {
        let project = automation_project();
        let track = &project.tracks[2];
        let old = &track.clips[0];
        let start = old.start;
        let at = Tick(2110);
        let (p, new) = edits::split_clips(&project, &[old.id], start + at).unwrap();
        let left = p.find_clip(old.id).unwrap().1;
        let right = p.find_clip(new[0]).unwrap().1;
        assert_eq!(left.loop_length, None);
        assert_eq!(right.loop_length, None);
        let old_points = clip_points(old, &old.automation[0]);
        for (clip, offset) in [(left, Tick::ZERO), (right, at)] {
            let points = clip_points(clip, &clip.automation[0]);
            for tick in 0..clip.length.0 {
                let old_tick = Tick((tick + offset.0) % old.loop_length.unwrap().0);
                let expected = value_at(old_points, old_tick).unwrap();
                assert!((value_at(points, Tick(tick)).unwrap() - expected).abs() < 1e-6);
            }
        }
    }
}
