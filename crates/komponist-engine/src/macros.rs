//! Compiled track macros and their bounded realtime state.

use std::sync::Arc;

use komponist_model::DeviceId;
use komponist_model::macros::{MACRO_COUNT, MAX_MAPPINGS, Mapping};
use komponist_model::params::ParamId;

use crate::automation::{EngineTarget, Parameter, PlainMap, normalized_to_gain};
use crate::processor::ParameterEditKind;
use crate::program::SlotId;

const CAPACITY: usize = MACRO_COUNT * MAX_MAPPINGS;
const REASSERT_SECONDS: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CompiledMapping {
    pub(crate) target: EngineTarget,
    pub(crate) map: PlainMap,
    pub(crate) driver: u8,
    min: f32,
    max: f32,
    invert: bool,
    k: f32,
    inv_den: f32,
}

impl CompiledMapping {
    pub(crate) fn new(target: EngineTarget, map: PlainMap, driver: u8, mapping: &Mapping) -> Self {
        let k = 6.0 * mapping.bend.clamp(-1.0, 1.0);
        let k = if k.abs() >= 1e-6 { k } else { 0.0 };
        Self {
            target,
            map,
            driver,
            min: mapping.min,
            max: mapping.max,
            invert: mapping.invert,
            k,
            inv_den: if k == 0.0 { 0.0 } else { 1.0 / k.exp_m1() },
        }
    }

    pub(crate) fn normalized(self, value: f32) -> f32 {
        let t = if self.invert { 1.0 - value } else { value };
        let fraction = if self.k == 0.0 || t == 0.0 || t == 1.0 {
            t
        } else {
            (self.k * t).exp_m1() * self.inv_den
        };
        self.min + (self.max - self.min) * fraction
    }
}

/// The compile-time descriptor retained for recompilation on the export worker.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MacroParameter {
    pub(crate) device: DeviceId,
    pub(crate) param: ParamId,
    pub(crate) descriptor: Parameter,
}

/// Stable input slots and resolved mappings for one track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MacroProgram {
    pub(crate) base: [f32; MACRO_COUNT],
    pub(crate) targets: Arc<[CompiledMapping]>,
    pub(crate) parameters: Arc<[MacroParameter]>,
}

impl MacroProgram {
    pub(crate) fn mapping(&self, target: EngineTarget) -> Option<(usize, CompiledMapping)> {
        self.targets
            .iter()
            .copied()
            .enumerate()
            .find(|&(_, mapping)| mapping.target == target)
    }
}

#[derive(Clone, Copy)]
struct TargetState {
    input: f32,
    value: f32,
    gesture: bool,
    reassert_at: Option<u64>,
    next_reassert: u64,
}

impl TargetState {
    const fn new() -> Self {
        Self {
            input: f32::NAN,
            value: f32::NAN,
            gesture: false,
            reassert_at: None,
            next_reassert: 0,
        }
    }
}

pub(crate) struct State {
    pub(crate) values: [f32; MACRO_COUNT],
    targets: [TargetState; CAPACITY],
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            values: [0.0; MACRO_COUNT],
            targets: [TargetState::new(); CAPACITY],
        }
    }

    pub(crate) fn install(&mut self, old: Option<&MacroProgram>, new: &MacroProgram) {
        self.values = new.base;
        if old.is_some_and(|old| old.targets == new.targets) {
            return;
        }
        let previous = self.targets;
        for (index, mapping) in new.targets.iter().enumerate() {
            self.targets[index] = old
                .and_then(|p| {
                    let (old_index, old_mapping) = p.mapping(mapping.target)?;
                    (*mapping == old_mapping).then_some(previous[old_index])
                })
                .unwrap_or_else(TargetState::new);
        }
    }

    pub(crate) fn set(&mut self, index: u8, value: f32) {
        if let Some(current) = self.values.get_mut(usize::from(index))
            && value.is_finite()
        {
            *current = value.clamp(0.0, 1.0);
        }
    }

    pub(crate) fn reassert(&mut self, program: &MacroProgram, slot: Option<SlotId>) {
        for (mapping, state) in program.targets.iter().zip(&mut self.targets) {
            if slot.is_none()
                || matches!(mapping.target, EngineTarget::Slot { slot: found, .. } if Some(found) == slot)
            {
                *state = TargetState::new();
            }
        }
    }

    pub(crate) fn plugin_edit(
        &mut self,
        program: &MacroProgram,
        target: EngineTarget,
        edit: ParameterEditKind,
        time: u64,
        rate: f64,
    ) -> bool {
        let Some((index, _)) = program.mapping(target) else {
            return false;
        };
        let state = &mut self.targets[index];
        match edit {
            ParameterEditKind::Gesture { begin: true, .. } => {
                state.gesture = true;
                state.reassert_at = None;
            }
            ParameterEditKind::Gesture { begin: false, .. } => {
                state.gesture = false;
                state.reassert_at = Some(time);
            }
            ParameterEditKind::Value { .. } if !state.gesture => {
                state.reassert_at = Some(time + reassert_frames(rate));
            }
            ParameterEditKind::Value { .. } => {}
        }
        true
    }

    pub(crate) fn slot_value(
        &mut self,
        index: usize,
        mapping: CompiledMapping,
        time: u64,
    ) -> Option<(f32, f32)> {
        let state = &mut self.targets[index];
        if state.gesture {
            return None;
        }
        if let Some(at) = state.reassert_at
            && time < at.max(state.next_reassert)
        {
            return None;
        }
        let input = self.values[usize::from(mapping.driver)];
        if state.reassert_at.is_none() && state.input == input {
            return None;
        }
        let value = mapping.normalized(input);
        if state.reassert_at.is_some()
            || !state.value.is_finite()
            || if mapping.map.is_stepped() {
                mapping.map.plain(state.value) != mapping.map.plain(value)
            } else {
                (state.value - value).abs() > 1e-5
            }
        {
            Some((input, value))
        } else {
            // Remember the evaluation, leaving the delivered value available for the next delta.
            state.input = input;
            None
        }
    }

    pub(crate) fn delivered(&mut self, index: usize, input: f32, value: f32, next_reassert: u64) {
        let state = &mut self.targets[index];
        state.input = input;
        state.value = value;
        if state.reassert_at.take().is_some() {
            state.next_reassert = next_reassert;
        }
    }

    pub(crate) fn mixer_value(&self, program: &MacroProgram, target: EngineTarget) -> Option<f32> {
        let (_, mapping) = program.mapping(target)?;
        let value = mapping.normalized(self.values[usize::from(mapping.driver)]);
        Some(match target {
            EngineTarget::Gain | EngineTarget::Send(_) => normalized_to_gain(value),
            EngineTarget::Pan => value.mul_add(2.0, -1.0),
            _ => return None,
        })
    }
}

pub(crate) fn reassert_frames(rate: f64) -> u64 {
    (rate * REASSERT_SECONDS).round().max(1.0) as u64
}
