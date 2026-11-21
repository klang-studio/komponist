use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use komponist_model::automation::{AutomationTarget, TrackAutomation};
use komponist_model::{Device, DeviceId, Project};

use super::LegacyDevicePlan;

/// A fully transformed project copy and preset labels recovered from its legacy states.
#[derive(Debug, Clone)]
pub struct LegacyProjectMigration {
    pub project: Project,
    /// Preset labels by retained device instance ID. The project format does not store them
    /// alongside opaque CLAP state.
    pub legacy_presets: BTreeMap<DeviceId, String>,
}

/// Apply verified plans to a project copy. Device instance IDs and normalized automation and
/// macro values are retained. Each plan must still match its source device in the project, and
/// every reference to a migrated device must resolve within its owner channel. If any plan or
/// reference fails validation, no transformed project is returned.
pub fn migrate_legacy_bundled_project(
    source: &Project,
    plans: impl IntoIterator<Item = LegacyDevicePlan>,
) -> Result<LegacyProjectMigration, String> {
    let mut devices = BTreeMap::new();
    for plan in plans {
        let id = plan.source.id;
        if devices.insert(id, plan).is_some() {
            return Err(format!("duplicate migration plan for device {}", id.0));
        }
    }
    let mut remaining = devices.keys().copied().collect();
    let mut project = source.clone();
    if !devices.is_empty() {
        let master = replace_devices(
            &mut Arc::make_mut(&mut project.master).devices,
            &devices,
            &mut remaining,
        )?;
        remap_lanes(
            Arc::make_mut(&mut project.master_automation),
            &master,
            &devices,
        )?;
        for track in &mut project.tracks {
            let track = Arc::make_mut(track);
            let owned = replace_devices(&mut track.channel.devices, &devices, &mut remaining)?;
            remap_lanes(&mut track.automation, &owned, &devices)?;
            for clip in &mut track.clips {
                for index in 0..clip.automation.len() {
                    if let Some(target) =
                        remap_target(&clip.automation[index].target, &owned, &devices)?
                    {
                        Arc::make_mut(clip).automation[index].target = target;
                    }
                }
            }
            for slot in &mut track.macros.0 {
                for mapping in &mut slot.mappings {
                    if let Some(target) = remap_target(&mapping.target, &owned, &devices)? {
                        if !(0.0..=1.0).contains(&mapping.min)
                            || !(mapping.min..=1.0).contains(&mapping.max)
                            || !(-1.0..=1.0).contains(&mapping.bend)
                        {
                            return Err(format!(
                                "invalid normalized macro mapping for {:?}",
                                mapping.target
                            ));
                        }
                        mapping.target = target;
                    }
                }
            }
        }
    }
    if let Some(id) = remaining.first() {
        return Err(format!("missing source device {} for migration plan", id.0));
    }
    let legacy_presets = devices
        .into_iter()
        .filter_map(|(id, plan)| plan.legacy_preset.map(|preset| (id, preset)))
        .collect();
    Ok(LegacyProjectMigration {
        project,
        legacy_presets,
    })
}

fn replace_devices(
    chain: &mut [Arc<Device>],
    plans: &BTreeMap<DeviceId, LegacyDevicePlan>,
    remaining: &mut BTreeSet<DeviceId>,
) -> Result<BTreeSet<DeviceId>, String> {
    let mut owned = BTreeSet::new();
    for device in chain {
        let Some(plan) = plans.get(&device.id) else {
            continue;
        };
        if !remaining.remove(&device.id) {
            return Err(format!("duplicate source device {}", device.id.0));
        }
        if **device != plan.source {
            return Err(format!(
                "source device {} changed since migration planning",
                device.id.0
            ));
        }
        owned.insert(device.id);
        *device = Arc::new(plan.replacement.clone());
    }
    Ok(owned)
}

fn remap_lanes(
    automation: &mut TrackAutomation,
    owned: &BTreeSet<DeviceId>,
    plans: &BTreeMap<DeviceId, LegacyDevicePlan>,
) -> Result<(), String> {
    for lane in &mut automation.lanes {
        if let Some(target) = remap_target(&lane.target, owned, plans)? {
            Arc::make_mut(lane).target = target;
        }
    }
    Ok(())
}

fn remap_target(
    target: &AutomationTarget,
    owned: &BTreeSet<DeviceId>,
    plans: &BTreeMap<DeviceId, LegacyDevicePlan>,
) -> Result<Option<AutomationTarget>, String> {
    let AutomationTarget::Param { device, param } = target else {
        return Ok(None);
    };
    let Some(plan) = plans.get(device) else {
        return Ok(None);
    };
    if !owned.contains(device) {
        return Err(format!(
            "parameter {param} refers to migrated device {} on another channel",
            device.0
        ));
    }
    let binding = plan
        .parameter(param)
        .ok_or_else(|| format!("unknown legacy parameter {param} on device {}", device.0))?;
    if let Some(reason) = binding.automation_issue {
        return Err(format!(
            "cannot migrate parameter {param} on device {}: {reason}",
            device.0
        ));
    }
    Ok(Some(AutomationTarget::Param {
        device: *device,
        param: binding.id.clone(),
    }))
}
