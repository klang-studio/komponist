//! Track macro slots and the normalized mapping shared by the UI and engine.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::TrackId;
use crate::automation::{AutomationTarget, bend};

/// Number of fixed macro slots on each track.
pub const MACRO_COUNT: usize = 8;
/// Maximum mappings added by edits or played by the engine per macro.
pub const MAX_MAPPINGS: usize = 16;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TrackMacros(pub [TrackMacro; MACRO_COUNT]);

impl TrackMacros {
    /// Whether every slot has its default contents and the file can omit the array.
    pub fn is_default(&self) -> bool {
        self.0
            .iter()
            .all(|slot| slot.name.is_empty() && slot.value == 0.0 && slot.mappings.is_empty())
    }

    /// The slot driving a target. Excess mappings loaded from disk are retained for editing.
    pub fn driver(&self, target: &AutomationTarget) -> Option<u8> {
        self.0
            .iter()
            .position(|slot| {
                slot.mappings
                    .iter()
                    .take(MAX_MAPPINGS)
                    .any(|m| &m.target == target)
            })
            .map(|index| index as u8)
    }

    /// Repair malformed ranges and resolve conflicting mappings in file order.
    pub(crate) fn repair(&mut self, track: TrackId) {
        let mut seen = HashSet::new();
        for (index, slot) in self.0.iter_mut().enumerate() {
            slot.value = slot.value.clamp(0.0, 1.0);
            slot.mappings.retain_mut(|mapping| {
                if matches!(mapping.target, AutomationTarget::Macro { .. })
                    || !seen.insert(mapping.target.clone())
                {
                    eprintln!(
                        "Ignored invalid or duplicate mapping on track {}, macro {}",
                        track.0,
                        index + 1
                    );
                    return false;
                }
                mapping.min = mapping.min.clamp(0.0, 1.0);
                mapping.max = mapping.max.clamp(0.0, 1.0);
                mapping.bend = mapping.bend.clamp(-1.0, 1.0);
                if mapping.min > mapping.max {
                    std::mem::swap(&mut mapping.min, &mut mapping.max);
                    eprintln!(
                        "Reordered mapping range on track {}, macro {}",
                        track.0,
                        index + 1
                    );
                }
                true
            });
            if slot.mappings.len() > MAX_MAPPINGS {
                eprintln!(
                    "Kept excess mappings on track {}, macro {}",
                    track.0,
                    index + 1
                );
            }
        }
    }
}

impl<'de> Deserialize<'de> for TrackMacros {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let slots = Vec::<TrackMacro>::deserialize(deserializer)?;
        if slots.len() > MACRO_COUNT {
            return Err(de::Error::invalid_length(
                slots.len(),
                &"at most eight track macros",
            ));
        }
        let mut slots = slots.into_iter();
        Ok(Self(std::array::from_fn(|_| {
            slots.next().unwrap_or_default()
        })))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackMacro {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub value: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mappings: Vec<Mapping>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub target: AutomationTarget,
    pub label: String,
    pub min: f32,
    pub max: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bend: f32,
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

impl Mapping {
    /// Evaluate the parameter's normalized position at a normalized macro value.
    pub fn value_at(&self, value: f32) -> f32 {
        let t = if self.invert { 1.0 - value } else { value };
        self.min + (self.max - self.min) * bend(t, self.bend)
    }
}

/// Choose a linear range for a new mapping that preserves the current parameter value.
///
/// Both inputs are normalized. A nearly constant range can use the full range only
/// if that also preserves the value at the macro's current position.
pub fn initial_range(value: f32, parameter: f32) -> (f32, f32, bool) {
    let (min, max) = if parameter >= value {
        let min = if value < 1.0 {
            (parameter - value) / (1.0 - value)
        } else {
            0.0
        };
        (min, 1.0)
    } else {
        (0.0, parameter / value)
    };
    if max - min < 0.01 && 1.0 - value == parameter {
        (0.0, 1.0, true)
    } else {
        (min, max, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn mapping_uses_the_automation_curve_and_inversion() {
        let mut mapping = Mapping {
            target: AutomationTarget::Pan,
            label: "Pan".into(),
            min: 0.25,
            max: 0.75,
            invert: false,
            bend: 0.0,
        };
        assert_eq!(mapping.value_at(0.25), 0.375);
        mapping.invert = true;
        assert_eq!(mapping.value_at(0.25), 0.625);
        mapping.bend = 0.4;
        assert_eq!(mapping.value_at(0.25), 0.25 + 0.5 * bend(0.75, 0.4));
        assert_eq!(mapping.value_at(0.0), 0.75);
        assert_eq!(mapping.value_at(1.0), 0.25);
    }

    #[test]
    fn narrow_mapping_ranges_still_preserve_the_value() {
        assert_eq!(initial_range(0.5, 1.0), (1.0, 1.0, false));
        assert_eq!(initial_range(0.5, 0.0), (0.0, 0.0, false));
        assert_eq!(initial_range(0.0, 1.0), (0.0, 1.0, true));
        assert_eq!(initial_range(1.0, 0.0), (0.0, 1.0, true));
    }

    proptest! {
        #[test]
        fn initial_range_preserves_the_value(value in 0.0f32..=1.0, parameter in 0.0f32..=1.0) {
            let (min, max, invert) = initial_range(value, parameter);
            let mapping = Mapping {
                target: AutomationTarget::Pan,
                label: String::new(),
                min,
                max,
                invert,
                bend: 0.0,
            };
            prop_assert!(0.0 <= min && min <= max && max <= 1.0);
            prop_assert!((mapping.value_at(value) - parameter).abs() <= f32::EPSILON);
        }
    }
}
