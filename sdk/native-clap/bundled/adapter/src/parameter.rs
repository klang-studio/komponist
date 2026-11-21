use std::sync::Arc;

use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};
use nice_plug::prelude::*;

trait ParameterValue: Send + Sync {
    fn ptr(&self) -> ParamPtr;
    fn raw(&self) -> f64;
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
}

/// Owns one nice-plug parameter used by the stable author ID of the device spec.
pub struct Binding(Box<dyn ParameterValue>);

impl Binding {
    /// Provide a static nice-plug enum for a categorical parameter.
    pub fn choice<E: Enum + PartialEq + Send + Sync + 'static>(info: &ParamInfo) -> Self {
        let ParamKind::Enum(labels) = &info.kind else {
            panic!("{} is not a choice", info.id);
        };
        assert_eq!(labels.len(), E::variants().len(), "{}", info.id);
        assert_eq!(
            info.default,
            E::from_index(info.default as usize).to_index() as f64
        );
        let default = E::from_index(info.default as usize);
        Self(Box::new(EnumParam::new(&info.name, default)))
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
                .with_value_to_string(Arc::new(|value| {
                    format_continuous(info, info.to_plain(f64::from(value)))
                }))
                .with_string_to_value(Arc::new(move |text| {
                    let plain = if scale == Scale::Pan {
                        komponist_model::format::parse_pan(text).map(f64::from)
                    } else {
                        parse_number(info.unit, text)
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
            parse_number(info.unit, text)
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
        return info.format(value);
    }
    let (scale, decimals, suffix) = match info.unit {
        Unit::None => (1.0, 3, ""),
        Unit::Hz => (1.0, 0, " Hz"),
        Unit::Ms => (1.0, 2, " ms"),
        Unit::S => (1.0, 3, " s"),
        Unit::Db => (1.0, 2, " dB"),
        Unit::Percent => (100.0, 1, " %"),
        Unit::Semitones => (1.0, 2, " st"),
        Unit::Cents => (1.0, 2, " ct"),
        Unit::Octaves => (1.0, 2, " oct"),
        Unit::Ratio => (1.0, 2, ":1"),
        Unit::Meters => (1.0, 1, " m"),
    };
    format!("{:.decimals$}{suffix}", value * scale)
}

fn parse_number(unit: Unit, text: &str) -> Option<f64> {
    let text = text.trim();
    let suffixes: &[(&str, f64)] = match unit {
        Unit::None => &[],
        Unit::Hz => &[("kHz", 1000.0), ("Hz", 1.0)],
        Unit::Ms => &[("ms", 1.0), ("s", 1000.0)],
        Unit::S => &[("ms", 0.001), ("s", 1.0)],
        Unit::Db => &[("dB", 1.0)],
        Unit::Percent => &[("%", 0.01)],
        Unit::Semitones => &[("st", 1.0)],
        Unit::Cents => &[("ct", 1.0)],
        Unit::Octaves => &[("oct", 1.0)],
        Unit::Ratio => &[(":1", 1.0)],
        Unit::Meters => &[("m", 1.0)],
    };
    let (number, factor) = suffixes
        .iter()
        .find_map(|(suffix, factor)| {
            text.strip_suffix(suffix)
                .map(|value| (value.trim(), *factor))
        })
        .unwrap_or((text, 1.0));
    let value = number.replace('−', "-").parse::<f64>().ok()? * factor;
    value.is_finite().then_some(value)
}

struct BoundParameter {
    info: &'static ParamInfo,
    binding: Binding,
    default_raw: f64,
}

pub(crate) struct Parameters(Vec<BoundParameter>);

impl Parameters {
    pub(crate) fn new(
        spec: &'static Spec,
        mut choice: impl FnMut(&'static ParamInfo) -> Option<Binding>,
    ) -> Self {
        Self(
            spec.params_in(Lang::En)
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
        )
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn raw_value(&self, index: usize) -> f64 {
        self.0[index].binding.0.raw()
    }

    pub(crate) fn changed_value(&self, index: usize, applied: &mut Option<f64>) -> Option<f64> {
        let bound = &self.0[index];
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
        self.0
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
}
