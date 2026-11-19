//! Compile one curve per target off-thread. Project data may remain unchanged when a
//! plugin's parameter list becomes available or a reload changes its range.

use std::sync::Arc;

use komponist_model::automation::{AutomationTarget, Lane, TrackAutomation, effective_points};
use komponist_model::params::db_to_p;
use komponist_model::project::Channel;
use komponist_model::{Project, Track};

use super::CompileEnv;
use crate::automation::{CompiledTarget, EngineTarget, MAX_TARGETS, Parameter};
use crate::program::{ChainSlot, Program, TrackProgram};

#[derive(PartialEq)]
struct Resolved<'a> {
    target: EngineTarget,
    lane: &'a Lane,
    parameter: Option<Parameter>,
    base: f32,
}

fn resolve<'a>(
    automation: &'a TrackAutomation,
    channel: &Channel,
    track: Option<&Track>,
    chain: &[ChainSlot],
    env: &dyn CompileEnv,
) -> Vec<Resolved<'a>> {
    let mut resolved: Vec<_> = automation
        .lanes
        .iter()
        .filter_map(|lane| {
            let (target, base, parameter) = match &lane.target {
                AutomationTarget::Volume => {
                    (EngineTarget::Gain, 1.0 - db_to_p(channel.volume_db), None)
                }
                AutomationTarget::Pan => (
                    EngineTarget::Pan,
                    (channel.pan.clamp(-1.0, 1.0) + 1.0) * 0.5,
                    None,
                ),
                AutomationTarget::Send { send } => {
                    let level = track?
                        .sends
                        .iter()
                        .find(|candidate| candidate.id == *send)?
                        .level_db;
                    (EngineTarget::Send(*send), 1.0 - db_to_p(level), None)
                }
                AutomationTarget::Param { device, param } => {
                    let slot = chain.iter().find(|slot| slot.device == *device)?.slot;
                    let state = channel
                        .devices
                        .iter()
                        .find(|candidate| candidate.id == *device)?;
                    let parameter = env.automation_parameter(state, param)?;
                    let target = EngineTarget::Slot {
                        slot,
                        wire_id: parameter.wire_id,
                    };
                    (
                        target,
                        parameter.map.normalize(parameter.base),
                        Some(parameter),
                    )
                }
                AutomationTarget::Macro { index } if track.is_some() && *index < 8 => {
                    (EngineTarget::Macro(*index), 0.0, None)
                }
                AutomationTarget::Macro { .. } => return None,
            };
            Some(Resolved {
                target,
                lane,
                parameter,
                base,
            })
        })
        .collect();
    resolved.sort_by_key(|r| r.target);
    resolved.dedup_by_key(|r| r.target);
    resolved.truncate(MAX_TARGETS);
    resolved
}

fn matches(prev: &[CompiledTarget], resolved: &[Resolved<'_>]) -> bool {
    prev.len() == resolved.len()
        && prev.iter().zip(resolved).all(|(old, new)| {
            old.model == new.lane.target
                && old.target == new.target
                && old.parameter == new.parameter
                && old.base == new.base
        })
}

pub(super) fn track(
    track: &Arc<Track>,
    chain: &[ChainSlot],
    env: &dyn CompileEnv,
    previous: Option<&TrackProgram>,
) -> Arc<[CompiledTarget]> {
    let resolved = resolve(&track.automation, &track.channel, Some(track), chain, env);
    if let Some(previous) = previous
        && (Arc::ptr_eq(&previous.source, track)
            || (previous.automation.is_empty() && resolved.is_empty()))
        && matches(&previous.automation, &resolved)
    {
        return Arc::clone(&previous.automation);
    }
    resolved
        .into_iter()
        .map(|new| {
            // Editing the channel or one lane need not rebuild neighbouring curves.
            if let Some(old) = previous
                && super::same_clips(&old.source.clips, &track.clips)
                && old.source.automation.lane(&new.lane.target) == Some(new.lane)
                && let Some(prev) = old.automation.iter().find(|prev| {
                    prev.model == new.lane.target
                        && prev.target == new.target
                        && prev.parameter == new.parameter
                        && prev.base == new.base
                })
            {
                return prev.clone();
            }
            CompiledTarget::new(
                new.target,
                new.lane.target.clone(),
                new.parameter,
                new.base,
                new.lane.mode,
                effective_points(track, &new.lane.target, new.base),
            )
        })
        .collect()
}

pub(super) fn master(
    project: &Project,
    chain: &[ChainSlot],
    env: &dyn CompileEnv,
    previous: Option<&Program>,
) -> Arc<[CompiledTarget]> {
    let resolved = resolve(
        &project.master_automation,
        &project.master,
        None,
        chain,
        env,
    );
    if let Some(old) = previous.map(|p| &p.master)
        && Arc::ptr_eq(&old.automation_source, &project.master_automation)
        && matches(&old.automation, &resolved)
    {
        return Arc::clone(&old.automation);
    }
    resolved
        .into_iter()
        .map(|new| {
            CompiledTarget::new(
                new.target,
                new.lane.target.clone(),
                new.parameter,
                new.base,
                new.lane.mode,
                new.lane.points.as_slice().to_vec(),
            )
        })
        .collect()
}
