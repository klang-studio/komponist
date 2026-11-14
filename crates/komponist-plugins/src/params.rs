//! Bridged plugin parameters as the parameter model: [`ipc::ParamInfo`] → [`ParamInfo`].
//!
//! The bridge reports `u32` ids (CLAP `clap_id`, VST3 `ParamID`) with a plain range; VST3
//! parameters arrive normalised (0 ..= 1) because only the plugin's controller can convert, so
//! the plugin's own value text ([`komponist_ipc::ParamValue::text`]) stays the text to show
//! whenever it exists; [`ParamInfo::format`] is the fallback. Wire id and model id correspond:
//! [`ParamId::plugin`] / [`ParamId::plugin_id`].

use komponist_ipc::control::{ParamInfo as WireInfo, param_flags};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

/// Whether a generic view lists the parameter (not hidden, not read-only, not the plugin's own
/// bypass).
pub fn is_listed(p: &WireInfo) -> bool {
    p.flags & (param_flags::HIDDEN | param_flags::READ_ONLY | param_flags::BYPASS) == 0
}

/// The model description of a bridged parameter. Nothing is modulatable (foreign plugins are not
/// modulated by komponist); `automatable` follows the plugin's flag.
pub fn param_info(p: &WireInfo) -> ParamInfo {
    let whole = |v: f64| v.fract() == 0.0 && v.abs() < f64::from(i32::MAX);
    let steps = f64::from(p.step_count);
    let kind = if p.step_count == 0 {
        ParamKind::lin(p.min, p.max)
    } else if p.min == 0.0 && p.max == 1.0 && p.step_count == 1 && p.flags & param_flags::ENUM == 0
    {
        ParamKind::Bool
    } else if whole(p.min) && whole(p.max) && p.max - p.min == steps {
        ParamKind::Int {
            min: p.min as i32,
            max: p.max as i32,
        }
    } else {
        ParamKind::Float {
            min: p.min,
            max: p.max,
            scale: Scale::Lin,
            steps: p.step_count,
        }
    };
    // A continuous 0 … 1 parameter is a normalised value (VST3, nice-plug's CLAP): show percent.
    let unit = if matches!(kind, ParamKind::Float { steps: 0, .. }) && p.min == 0.0 && p.max == 1.0
    {
        Unit::Percent
    } else {
        Unit::None
    };
    let mut info = ParamInfo::new(ParamId::plugin(p.id), p.name.clone(), kind, p.default)
        .unit(unit)
        .flags(p.flags & param_flags::AUTOMATABLE != 0, false);
    info.is_macro = p.flags & param_flags::MACRO != 0;
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(min: f64, max: f64, step_count: u32, flags: u32) -> WireInfo {
        WireInfo {
            id: 7,
            name: "Cutoff".into(),
            module: String::new(),
            min,
            max,
            default: min,
            step_count,
            flags,
        }
    }

    #[test]
    fn kinds_follow_range_and_steps() {
        let f = param_info(&wire(0.0, 1.0, 0, param_flags::AUTOMATABLE));
        assert_eq!(f.id, ParamId::plugin(7));
        assert_eq!(f.kind, ParamKind::lin(0.0, 1.0));
        assert_eq!(f.format(0.4), "40 %");
        assert!(f.automatable && !f.modulatable && !f.is_macro);
        assert_eq!(param_info(&wire(0.0, 1.0, 1, 0)).kind, ParamKind::Bool);
        assert_eq!(
            param_info(&wire(0.0, 1.0, 1, param_flags::ENUM)).kind,
            ParamKind::Int { min: 0, max: 1 }
        );
        let int = param_info(&wire(0.0, 8.0, 8, param_flags::STEPPED));
        assert_eq!(int.kind, ParamKind::Int { min: 0, max: 8 });
        assert_eq!(int.format(3.0), "3");
        // VST3: normalised, stepped in 1/n.
        let v3 = param_info(&wire(0.0, 1.0, 4, param_flags::STEPPED));
        assert_eq!(v3.steps(), 4);
        assert_eq!(v3.snap(0.3), 0.25);
        let plain = param_info(&wire(-60.0, 0.0, 0, 0));
        assert_eq!(plain.format(-12.0), "\u{2212}12.00");
        assert!(!plain.automatable);
        assert!(param_info(&wire(0.0, 1.0, 0, param_flags::MACRO)).is_macro);
    }

    #[test]
    fn hidden_read_only_and_bypass_are_not_listed() {
        assert!(is_listed(&wire(0.0, 1.0, 0, param_flags::AUTOMATABLE)));
        for flag in [
            param_flags::HIDDEN,
            param_flags::READ_ONLY,
            param_flags::BYPASS,
        ] {
            assert!(!is_listed(&wire(0.0, 1.0, 0, flag)));
        }
    }
}
