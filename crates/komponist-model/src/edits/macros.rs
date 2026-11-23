//! Pure edits for track macros. The caller owns undo labels and gesture boundaries.

use super::automation::track_has_target;
use super::{EditError, EditResult, locate_track, update_track};
use crate::automation::AutomationTarget;
use crate::macros::{MAX_MAPPINGS, Mapping, TrackMacro, TrackMacros};
use crate::{DeviceId, Project, TrackId};

fn edit_macros(
    project: &Project,
    track: TrackId,
    edit: impl FnOnce(&mut TrackMacros) -> Result<(), EditError>,
) -> EditResult {
    let index = locate_track(project, track)?;
    let mut next = project.clone();
    update_track(&mut next, index, |track| edit(&mut track.macros))?;
    Ok(next)
}

fn slot(macros: &mut TrackMacros, index: u8) -> Result<&mut TrackMacro, EditError> {
    let index = usize::from(index);
    macros.0.get_mut(index).ok_or(EditError::IndexOutOfRange {
        what: "track macro",
        index,
    })
}

fn normalized(value: f32) -> Result<f32, EditError> {
    if value.is_nan() {
        return Err(EditError::Invalid("track macro value is not a number"));
    }
    Ok(value.clamp(0.0, 1.0))
}

fn bend_value(bend: f32) -> Result<f32, EditError> {
    if bend.is_nan() {
        return Err(EditError::Invalid("mapping bend is not a number"));
    }
    Ok(bend.clamp(-1.0, 1.0))
}

fn edit_mapping(
    project: &Project,
    track: TrackId,
    index: u8,
    target: &AutomationTarget,
    edit: impl FnOnce(&mut Mapping),
) -> EditResult {
    edit_macros(project, track, |macros| {
        let mapping = slot(macros, index)?
            .mappings
            .iter_mut()
            .find(|mapping| mapping.target == *target)
            .ok_or(EditError::Invalid("mapping target not found"))?;
        edit(mapping);
        Ok(())
    })
}

/// Sets a slot's normalized base value.
pub fn set_macro_value(project: &Project, track: TrackId, index: u8, value: f32) -> EditResult {
    let value = normalized(value)?;
    edit_macros(project, track, |macros| {
        slot(macros, index)?.value = value;
        Ok(())
    })
}

/// Sets a slot's user name, with an empty name restoring the display fallback.
pub fn rename_macro(project: &Project, track: TrackId, index: u8, name: String) -> EditResult {
    edit_macros(project, track, |macros| {
        slot(macros, index)?.name = name;
        Ok(())
    })
}

/// Adds a mapping using the caller's range, moving it from another slot if already mapped.
/// Adding the same target to the same slot leaves its mapping in place.
/// The caller resolves which device parameters are automatable.
pub fn add_mapping(
    project: &Project,
    track: TrackId,
    index: u8,
    mut mapping: Mapping,
) -> EditResult {
    let owner = project
        .track(track)
        .ok_or(EditError::TrackNotFound(track))?;
    if matches!(mapping.target, AutomationTarget::Macro { .. })
        || !track_has_target(owner, &mapping.target)
    {
        return Err(EditError::Invalid(
            "mapping target does not belong to the track",
        ));
    }
    mapping.min = normalized(mapping.min)?;
    mapping.max = normalized(mapping.max)?;
    mapping.bend = bend_value(mapping.bend)?;
    if mapping.min > mapping.max {
        return Err(EditError::Invalid("mapping minimum is above maximum"));
    }
    edit_macros(project, track, |macros| {
        let destination = slot(macros, index)?;
        if destination
            .mappings
            .iter()
            .any(|item| item.target == mapping.target)
        {
            return Ok(());
        }
        if destination.mappings.len() >= MAX_MAPPINGS {
            return Err(EditError::MacroMappingLimit { index });
        }
        remove_targets(macros, |target| *target == mapping.target);
        macros.0[usize::from(index)].mappings.push(mapping);
        Ok(())
    })
}

/// Removes the target from this slot, preserving the order of the other mappings.
pub fn remove_mapping(
    project: &Project,
    track: TrackId,
    index: u8,
    target: &AutomationTarget,
) -> EditResult {
    edit_macros(project, track, |macros| {
        slot(macros, index)?
            .mappings
            .retain(|mapping| mapping.target != *target);
        Ok(())
    })
}

/// Removes all mappings from the slot, preserving its name and value.
pub fn clear_macro(project: &Project, track: TrackId, index: u8) -> EditResult {
    edit_macros(project, track, |macros| {
        slot(macros, index)?.mappings.clear();
        Ok(())
    })
}

/// Sets the range, stopping a handle at the opposite handle when they meet.
pub fn set_mapping_range(
    project: &Project,
    track: TrackId,
    index: u8,
    target: &AutomationTarget,
    min: f32,
    max: f32,
) -> EditResult {
    let min = normalized(min)?;
    let max = normalized(max)?;
    edit_mapping(project, track, index, target, |mapping| {
        if min > max && max == mapping.max {
            mapping.min = max;
        } else {
            mapping.min = min;
            mapping.max = max.max(min);
        }
    })
}

/// Sets the mapping's direction.
pub fn set_mapping_invert(
    project: &Project,
    track: TrackId,
    index: u8,
    target: &AutomationTarget,
    invert: bool,
) -> EditResult {
    edit_mapping(project, track, index, target, |mapping| {
        mapping.invert = invert
    })
}

/// Sets the bend of the mapping's interpolation curve.
pub fn set_mapping_bend(
    project: &Project,
    track: TrackId,
    index: u8,
    target: &AutomationTarget,
    bend: f32,
) -> EditResult {
    let bend = bend_value(bend)?;
    edit_mapping(project, track, index, target, |mapping| mapping.bend = bend)
}

pub(super) fn remove_targets(
    macros: &mut TrackMacros,
    mut remove: impl FnMut(&AutomationTarget) -> bool,
) {
    for slot in &mut macros.0 {
        slot.mappings.retain(|mapping| !remove(&mapping.target));
    }
}

pub(super) fn remove_device_mappings(macros: &mut TrackMacros, device: DeviceId) {
    remove_targets(
        macros,
        |target| matches!(target, AutomationTarget::Param { device: id, .. } if *id == device),
    );
}

#[cfg(test)]
mod tests;
