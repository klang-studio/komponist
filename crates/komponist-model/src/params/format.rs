//! The text of a value per kind and unit (device handoff §6: `420 Hz`, `1.2 kHz`, `−4.0 dB`,
//! `L 14`, `C`; minus is U+2212, `k` from 1000 Hz).

use super::{ParamInfo, ParamKind, Scale, Unit};
use crate::format::{MINUS, fmt_db, fmt_pan};

pub(super) fn value_text(info: &ParamInfo, plain: f64) -> String {
    let v = info.snap(plain);
    match &info.kind {
        ParamKind::Bool => (if v >= 0.5 {
            info.texts.on
        } else {
            info.texts.off
        })
        .to_owned(),
        ParamKind::Enum(names) => names.get(v.round() as usize).cloned().unwrap_or_default(),
        ParamKind::Int { .. } => with_unit(info, v, number(v, 0)),
        ParamKind::Float {
            scale: Scale::Pan, ..
        } => fmt_pan(v as f32),
        ParamKind::Float { .. } => float_text(info, v),
    }
}

fn float_text(info: &ParamInfo, v: f64) -> String {
    match info.unit {
        Unit::None => number(v, 2),
        Unit::Hz => hz(v),
        Unit::Ms => ms(v),
        Unit::S => seconds(v),
        Unit::Db => format!("{} dB", fmt_db(v as f32)),
        Unit::Percent => format!("{}{}", number((v * 100.0).round(), 0), info.texts.percent),
        Unit::Semitones => format!("{} st", signed_whole(v)),
        Unit::Cents => format!("{} ct", signed_whole(v)),
        Unit::Octaves => format!("{} oct", signed_whole(v)),
        Unit::Ratio => format!("{}:1", number(v, 1)),
        Unit::Meters => format!("{} m", number(v, 0)),
    }
}

/// An integer-valued kind: its number plus the unit's suffix, if it has one.
fn with_unit(info: &ParamInfo, v: f64, whole: String) -> String {
    match info.unit {
        Unit::None => whole,
        _ => float_text(info, v),
    }
}

/// `420 Hz`, `1.2 kHz`, `12 kHz`.
fn hz(v: f64) -> String {
    if v < 1000.0 {
        format!("{} Hz", number(v, 0))
    } else if v < 10_000.0 {
        format!("{} kHz", number(v / 1000.0, 1))
    } else {
        format!("{} kHz", number(v / 1000.0, 0))
    }
}

/// `8.0 ms`, `375 ms`, then seconds from 1000 ms.
fn ms(v: f64) -> String {
    if v >= 1000.0 {
        seconds(v / 1000.0)
    } else if v < 10.0 {
        format!("{} ms", number(v, 1))
    } else {
        format!("{} ms", number(v, 0))
    }
}

/// Below one second in milliseconds, then `1.20 s`, `12.0 s`.
fn seconds(v: f64) -> String {
    if v < 1.0 {
        ms(v * 1000.0)
    } else if v < 10.0 {
        format!("{} s", number(v, 2))
    } else {
        format!("{} s", number(v, 1))
    }
}

/// A number with `decimals` places and the real minus; a result that rounds to zero has no sign.
fn number(v: f64, decimals: usize) -> String {
    let text = format!("{:.*}", decimals, v.abs());
    let zero = text.bytes().all(|b| b == b'0' || b == b'.');
    if v < 0.0 && !zero {
        format!("{MINUS}{text}")
    } else {
        text
    }
}

/// Like [`number`] with a `+` for positive values (so `+7`, `−12`, `0`).
fn signed(v: f64, decimals: usize) -> String {
    let n = number(v, decimals);
    if v > 0.0 && !n.bytes().all(|b| b == b'0' || b == b'.') {
        format!("+{n}")
    } else {
        n
    }
}

/// A whole number when the value is (nearly) one, else one decimal; signed.
fn signed_whole(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        signed(v.round(), 0)
    } else {
        signed(v, 1)
    }
}
