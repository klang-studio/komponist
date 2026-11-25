use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use komponist_device_core::pinned_clap_state::FACTORY_PRESET_FIELD;
use komponist_device_core::{Preset, Spec};
use komponist_i18n::Lang;
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};
use nice_plug::prelude::*;

trait ParameterValue: Send + Sync {
    fn ptr(&self) -> ParamPtr;
    fn raw(&self) -> f64;

    /// The typed enum behind a choice: its variant names and stable IDs.
    fn choice(&self) -> Option<Choice> {
        None
    }
}

/// The names and stable IDs of a typed choice parameter, as the host and saved states see them.
#[derive(Clone, Copy)]
pub struct Choice {
    pub variants: &'static [&'static str],
    pub ids: Option<&'static [&'static str]>,
}

impl ParameterValue for FloatParam {
    fn ptr(&self) -> ParamPtr {
        self.as_ptr()
    }

    fn raw(&self) -> f64 {
        f64::from(self.value())
    }
}

impl ParameterValue for IntParam {
    fn ptr(&self) -> ParamPtr {
        self.as_ptr()
    }

    fn raw(&self) -> f64 {
        f64::from(self.value())
    }
}

impl ParameterValue for BoolParam {
    fn ptr(&self) -> ParamPtr {
        self.as_ptr()
    }

    fn raw(&self) -> f64 {
        f64::from(u8::from(self.value()))
    }
}

impl<E: Enum + PartialEq + Send + Sync + 'static> ParameterValue for EnumParam<E> {
    fn ptr(&self) -> ParamPtr {
        self.as_ptr()
    }

    fn raw(&self) -> f64 {
        self.value().to_index() as f64
    }

    fn choice(&self) -> Option<Choice> {
        Some(Choice {
            variants: E::variants(),
            ids: E::ids(),
        })
    }
}

/// Owns one nice-plug parameter used by the stable author ID of the device spec.
pub struct Binding(Box<dyn ParameterValue>);

impl Binding {
    /// Provide a static nice-plug enum for a categorical parameter.
    pub fn choice<E: Enum + PartialEq + Send + Sync + 'static>(info: &ParamInfo) -> Self {
        assert!(
            matches!(info.kind, ParamKind::Enum(_)),
            "{} is not a choice",
            info.id
        );
        // `assert_choice_bindings` checks the labels and the default against the enum: a mismatch
        // must fail in a test, not panic inside the CLAP factory.
        let default = E::from_index(info.default as usize);
        Self(Box::new(EnumParam::new(&info.name, default)))
    }

    /// The typed enum behind this binding, `None` for continuous, integer and boolean ones.
    pub fn choice_of(&self) -> Option<Choice> {
        self.0.choice()
    }

    fn from_info(info: &'static ParamInfo) -> Self {
        match info.kind {
            ParamKind::Float {
                steps: 0, scale, ..
            } => {
                // The wrapper publishes the normalized value via CLAP. Using the model's exact
                // inverse preserves log and fader-taper behavior for automation.
                let mut param = FloatParam::new(
                    &info.name,
                    info.default_normalized() as f32,
                    FloatRange::Linear { min: 0.0, max: 1.0 },
                )
                .with_value_to_string(Arc::new(move |value| {
                    format_continuous(info, info.to_plain(f64::from(value)))
                }))
                .with_string_to_value(Arc::new(move |text| {
                    let plain = if scale == Scale::Pan {
                        komponist_model::format::parse_pan(text).map(f64::from)
                    } else {
                        parse_parameter_text(info, text)
                    }?;
                    Some(info.to_normalized(plain) as f32)
                }));
                if !info.automatable {
                    param = param.non_automatable();
                }
                Self(Box::new(param))
            }
            ParamKind::Float { steps, .. } => Self(Box::new(integer_param(
                info,
                (((info.default - integer_offset(info)) / integer_multiplier(info)).round()) as i32,
                0,
                steps as i32,
            ))),
            ParamKind::Int { min, max } => {
                Self(Box::new(integer_param(info, info.default as i32, min, max)))
            }
            ParamKind::Bool => {
                let mut param = BoolParam::new(&info.name, info.default >= 0.5);
                if !info.automatable {
                    param = param.non_automatable();
                }
                Self(Box::new(param))
            }
            ParamKind::Enum(_) => panic!("{} requires a typed categorical binding", info.id),
        }
    }
}

/// Contract test for a device's typed choice bindings. For every choice parameter of the spec:
/// `D::choice` binds it, the English labels equal the enum's variant names in the same order,
/// the default is a valid index, and the stable IDs are set, unique and one per variant.
/// Binding by index is only correct while the labels and the enum agree, so a mismatch is a
/// test failure here rather than a panic when a host creates the plugin.
pub fn assert_choice_bindings<D: crate::DeviceDefinition>() {
    let mut problems = Vec::new();
    for info in D::SPEC.params_in(Lang::En) {
        let id = info.id.as_str();
        let bound = D::choice(info);
        let ParamKind::Enum(labels) = &info.kind else {
            if bound.is_some() {
                problems.push(format!("{id} is not a choice but has a typed binding"));
            }
            continue;
        };
        let Some(choice) = bound.as_ref().and_then(Binding::choice_of) else {
            problems.push(format!("{id} has no typed choice binding"));
            continue;
        };
        if labels.as_slice() != choice.variants {
            problems.push(format!(
                "{id}: enum variants {:?} differ from the model's labels {labels:?}",
                choice.variants
            ));
        }
        if !((0.0..labels.len() as f64).contains(&info.default) && info.default.fract() == 0.0) {
            problems.push(format!(
                "{id}: default {} is not a valid choice index",
                info.default
            ));
        }
        match choice.ids {
            Some(ids) if ids.len() == labels.len() => {
                let unique: std::collections::BTreeSet<_> = ids.iter().collect();
                if unique.len() != ids.len() {
                    problems.push(format!("{id}: stable IDs must be unique"));
                }
            }
            _ => problems.push(format!("{id}: the enum needs one stable ID per variant")),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

fn integer_multiplier(info: &ParamInfo) -> f64 {
    if let ParamKind::Float {
        min, max, steps, ..
    } = info.kind
    {
        (max - min) / f64::from(steps)
    } else {
        1.0
    }
}

fn integer_offset(info: &ParamInfo) -> f64 {
    match info.kind {
        ParamKind::Float { min, .. } => min,
        _ => 0.0,
    }
}

fn integer_param(info: &'static ParamInfo, default: i32, min: i32, max: i32) -> IntParam {
    let multiplier = integer_multiplier(info);
    let offset = integer_offset(info);
    let mut param = IntParam::new(&info.name, default, IntRange::Linear { min, max })
        .with_value_to_string(Arc::new(move |value| {
            info.format(f64::from(value) * multiplier + offset)
        }))
        .with_string_to_value(Arc::new(move |text| {
            parse_parameter_text(info, text)
                .map(|value| ((value - offset) / multiplier).round() as i32)
        }));
    if !info.automatable {
        param = param.non_automatable();
    }
    param
}

// CLAP hosts can round-trip a formatted value. Using one scale per unit avoids
// switching formatting branches when the parsed value is rounded to normalized f32.
fn format_continuous(info: &ParamInfo, value: f64) -> String {
    if info.format.is_some()
        || matches!(
            info.kind,
            ParamKind::Float {
                scale: Scale::Pan,
                ..
            }
        )
    {
        let custom = info.format(value);
        if !matches!(info.unit, Unit::Ms | Unit::S) || parse_number(info.unit, &custom).is_none() {
            return custom;
        }
    }
    let (scale, decimals, suffix) = match info.unit {
        Unit::None => (1.0, 3, ""),
        Unit::Hz => (1.0, 0, " Hz"),
        Unit::Ms => (1.0, 2, " ms"),
        Unit::S => (1.0, 3, " s"),
        Unit::Db => (1.0, 2, " dB"),
        Unit::DbWidth => (1.0, 0, " dB"),
        Unit::Percent => (100.0, 1, " %"),
        Unit::Semitones => (1.0, 2, " st"),
        Unit::Cents => (1.0, 2, " ct"),
        Unit::Octaves => (1.0, 2, " oct"),
        Unit::Ratio => (1.0, 2, ":1"),
        Unit::Meters => (1.0, 1, " m"),
    };
    format!("{:.decimals$}{suffix}", value * scale)
}

fn parse_parameter_text(info: &ParamInfo, text: &str) -> Option<f64> {
    if let Some(value) = parse_number(info.unit, text) {
        return Some(value);
    }
    info.format.as_ref()?;
    let text = text.trim();
    let matches_text = |value| info.format(value).eq_ignore_ascii_case(text);
    if let ParamKind::Int { min, max } = info.kind
        && max.saturating_sub(min) < 128
    {
        // Small named integer ranges include the sampler's MIDI root note.
        return (min..=max)
            .map(f64::from)
            .find(|value| matches_text(*value));
    }
    let (min, max) = info.range();
    [min, max].into_iter().find(|value| matches_text(*value))
}

fn parse_number(unit: Unit, text: &str) -> Option<f64> {
    let text = text.trim();
    let suffixes: &[(&str, f64)] = match unit {
        Unit::None => &[],
        Unit::Hz => &[("kHz", 1000.0), ("Hz", 1.0)],
        Unit::Ms => &[("ms", 1.0), ("s", 1000.0)],
        Unit::S => &[("ms", 0.001), ("s", 1.0)],
        Unit::Db | Unit::DbWidth => &[("dB", 1.0)],
        Unit::Percent => &[("%", 0.01)],
        Unit::Semitones => &[("st", 1.0)],
        Unit::Cents => &[("ct", 1.0)],
        Unit::Octaves => &[("oct", 1.0)],
        Unit::Ratio => &[(":1", 1.0)],
        Unit::Meters => &[("m", 1.0)],
    };
    let default_factor = if unit == Unit::Percent { 0.01 } else { 1.0 };
    let (number, factor) = suffixes
        .iter()
        .find_map(|(suffix, factor)| {
            text.strip_suffix(suffix)
                .map(|value| (value.trim(), *factor))
        })
        .unwrap_or((text, default_factor));
    let value = number.replace('−', "-").parse::<f64>().ok()? * factor;
    value.is_finite().then_some(value)
}

struct BoundParameter {
    info: &'static ParamInfo,
    binding: Binding,
    default_raw: f64,
}

/// Stable author bindings and model-to-CLAP scaling for a set of device parameters.
pub struct Parameters {
    bindings: Vec<BoundParameter>,
    factory_presets: &'static [Preset],
    selected_preset: Mutex<Option<&'static str>>,
}

impl Parameters {
    pub fn new(
        spec: &'static Spec,
        choice: impl FnMut(&'static ParamInfo) -> Option<Binding>,
    ) -> Self {
        let mut params = Self::from_infos(spec.params_in(Lang::En), choice);
        params.factory_presets = spec.presets();
        params
    }

    /// Bind independently described device parameters in the supplied wire order.
    pub fn from_infos(
        infos: &'static [ParamInfo],
        mut choice: impl FnMut(&'static ParamInfo) -> Option<Binding>,
    ) -> Self {
        Self {
            bindings: infos
                .iter()
                .map(|info| {
                    let binding = choice(info).unwrap_or_else(|| Binding::from_info(info));
                    let default_raw = binding.0.raw();
                    BoundParameter {
                        info,
                        binding,
                        default_raw,
                    }
                })
                .collect(),
            factory_presets: &[],
            selected_preset: Mutex::new(None),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Creates per-instance tracking for the last values applied to the DSP.
    pub fn unapplied_values(&self) -> Vec<Option<f64>> {
        vec![None; self.len()]
    }

    pub(crate) fn raw_value(&self, index: usize) -> f64 {
        self.bindings[index].binding.0.raw()
    }

    pub fn plain_value_by_id(&self, id: &str) -> Option<f64> {
        let index = self
            .bindings
            .iter()
            .position(|bound| bound.info.id.as_str() == id)?;
        self.changed_value(index, &mut None)
    }

    /// Returns the new model plain value and records the underlying wrapper value.
    pub fn changed_value(&self, index: usize, applied: &mut Option<f64>) -> Option<f64> {
        let bound = &self.bindings[index];
        let raw = bound.binding.0.raw();
        if *applied == Some(raw) {
            return None;
        }
        *applied = Some(raw);
        // Retain the exact built-in default, and only evaluate nonlinear scales on a change.
        let info = bound.info;
        Some(if raw == bound.default_raw {
            info.default
        } else {
            match info.kind {
                ParamKind::Float { steps: 0, .. } => info.to_plain(raw),
                ParamKind::Float { .. } => raw * integer_multiplier(info) + integer_offset(info),
                _ => raw,
            }
        })
    }
}

// SAFETY: Each pointer is owned by a heap-allocated binding kept alive by the Arc that nice-plug
// retains for the entire plugin instance. The vector and its bindings are immutable after setup.
unsafe impl Params for Parameters {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        self.bindings
            .iter()
            .map(|bound| {
                (
                    bound.info.id.as_str().into(),
                    bound.binding.0.ptr(),
                    String::new(),
                )
            })
            .collect()
    }

    fn serialize_fields(&self) -> BTreeMap<String, String> {
        let preset = self
            .selected_preset
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        preset
            .iter()
            .map(|name| {
                (
                    FACTORY_PRESET_FIELD.to_owned(),
                    serde_json::to_string(name).expect("preset names encode as JSON strings"),
                )
            })
            .collect()
    }

    fn deserialize_fields(&self, fields: &BTreeMap<String, String>) {
        let selected = fields
            .get(FACTORY_PRESET_FIELD)
            .and_then(|raw| serde_json::from_str::<String>(raw).ok())
            .and_then(|name| {
                self.factory_presets
                    .iter()
                    .find(|preset| preset.name == name)
            })
            .map(|preset| preset.name);
        *self
            .selected_preset
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = selected;
    }
}

#[cfg(test)]
mod tests {
    use super::{Unit, parse_number};

    #[test]
    fn percent_input_uses_display_units_with_or_without_suffix() {
        assert_eq!(parse_number(Unit::Percent, "50 %"), Some(0.5));
        assert_eq!(parse_number(Unit::Percent, "50"), Some(0.5));
        assert_eq!(parse_number(Unit::Hz, "50"), Some(50.0));
    }
}
