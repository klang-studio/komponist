//! Resolve track macros against the same processor metadata and slots as automation.

use komponist_model::Track;
use komponist_model::automation::AutomationTarget;
use komponist_model::macros::MAX_MAPPINGS;

use super::{CompileEnv, automation::resolve_parameter};
use crate::automation::{EngineTarget, PlainMap};
use crate::macros::{CompiledMapping, MacroParameter, MacroProgram};
use crate::program::ChainSlot;

pub(super) fn track(track: &Track, chain: &[ChainSlot], env: &dyn CompileEnv) -> MacroProgram {
    let base = track.macros.0.each_ref().map(|slot| {
        if slot.value.is_finite() {
            slot.value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    });
    let mut targets: Vec<CompiledMapping> = Vec::new();
    let mut parameters = Vec::new();
    for (index, slot) in track.macros.0.iter().enumerate() {
        for mapping in slot.mappings.iter().take(MAX_MAPPINGS) {
            let (target, map) = match &mapping.target {
                AutomationTarget::Param { device, param } => {
                    let Some((target, parameter)) =
                        resolve_parameter(&track.channel, chain, env, *device, param)
                    else {
                        continue;
                    };
                    parameters.push(MacroParameter {
                        device: *device,
                        param: param.clone(),
                        descriptor: parameter,
                    });
                    (target, parameter.map)
                }
                AutomationTarget::Volume => (EngineTarget::Gain, PlainMap::linear(0.0, 1.0, 0)),
                AutomationTarget::Pan => (EngineTarget::Pan, PlainMap::linear(0.0, 1.0, 0)),
                AutomationTarget::Send { send } if track.sends.iter().any(|s| s.id == *send) => {
                    (EngineTarget::Send(*send), PlainMap::linear(0.0, 1.0, 0))
                }
                _ => continue,
            };
            if !targets.iter().any(|mapping| mapping.target == target) {
                targets.push(CompiledMapping::new(target, map, index as u8, mapping));
            }
        }
    }
    MacroProgram {
        base,
        targets: targets.into(),
        parameters: parameters.into(),
    }
}
