//! Compiled track macros and their bounded realtime state.

use std::sync::Arc;

use komponist_model::DeviceId;
use komponist_model::macros::{MACRO_COUNT, MAX_MAPPINGS, Mapping};
use komponist_model::params::ParamId;

use crate::automation::{EngineTarget, Parameter, PlainMap, normalized_to_gain};
use crate::program::SlotId;
use komponist_processor::processor::ParameterEditKind;

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
pub(crate) struct TargetState {
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
    pub(crate) base: [f32; MACRO_COUNT],
    /// Effective positions at the end of the last rendered segment.
    pub(crate) values: [f32; MACRO_COUNT],
    targets: [TargetState; CAPACITY],
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            base: [0.0; MACRO_COUNT],
            values: [0.0; MACRO_COUNT],
            targets: [TargetState::new(); CAPACITY],
        }
    }

    pub(crate) fn install(&mut self, old: Option<&MacroProgram>, new: &MacroProgram) {
        self.base = new.base;
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
        if let Some(current) = self.base.get_mut(usize::from(index))
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

    pub(crate) fn slot_state(&self, index: usize) -> TargetState {
        self.targets[index]
    }

    pub(crate) fn delivered(&mut self, index: usize, input: f32, value: f32, next_reassert: u64) {
        self.targets[index].delivered(input, value, next_reassert);
    }

    pub(crate) fn mixer_value(&self, program: &MacroProgram, target: EngineTarget) -> Option<f32> {
        let (_, mapping) = program.mapping(target)?;
        mapping.mixer_value(self.base[usize::from(mapping.driver)])
    }
}

impl CompiledMapping {
    pub(crate) fn mixer_value(self, input: f32) -> Option<f32> {
        let value = self.normalized(input);
        Some(match self.target {
            EngineTarget::Gain | EngineTarget::Send(_) => normalized_to_gain(value),
            EngineTarget::Pan => value.mul_add(2.0, -1.0),
            _ => return None,
        })
    }
}

impl TargetState {
    pub(crate) fn needs_value(self) -> bool {
        !self.input.is_finite() || self.reassert_at.is_some()
    }

    pub(crate) fn slot_value(
        &self,
        mapping: CompiledMapping,
        input: f32,
        time: u64,
    ) -> Option<f32> {
        if self.gesture
            || self
                .reassert_at
                .is_some_and(|at| time < at.max(self.next_reassert))
        {
            return None;
        }
        if self.reassert_at.is_none() && self.input == input {
            return None;
        }
        let value = mapping.normalized(input);
        (self.reassert_at.is_some()
            || !self.value.is_finite()
            || if mapping.map.is_stepped() {
                mapping.map.plain(self.value) != mapping.map.plain(value)
            } else {
                (self.value - value).abs() > 1e-5
            })
        .then_some(value)
    }

    pub(crate) fn delivered(&mut self, input: f32, value: f32, next_reassert: u64) {
        self.input = input;
        self.value = value;
        if self.reassert_at.take().is_some() {
            self.next_reassert = next_reassert;
        }
    }
}

pub(crate) fn reassert_frames(rate: f64) -> u64 {
    (rate * REASSERT_SECONDS).round().max(1.0) as u64
}
