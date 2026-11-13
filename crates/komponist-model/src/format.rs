//! Pure formatting of values for the UI (ARCHITECTURE §5.4).
//!
//! Rules from `ks-engine.js` (`fmtPos`, `fmtTime`, `fmtDb`, `fmtPan`, `noteName`) and the screens.
//! Typography is part of the contract: the minus sign of numeric values (dB, pan input) is U+2212
//! (`−`), never `-`; digits are meant for tabular (monospace) display.

use crate::project::VOLUME_DB_MIN;
use crate::time::{DISPLAY_TICKS_PER_BEAT, Meter, TICKS_PER_STEP, Tempo, TempoMap, Tick};

/// What [`fmt_pos`] shows before tick 0, during the count-in.
pub const COUNT_IN_LABEL: &str = "Vorzähler";

/// The minus sign used in all formatted values (U+2212).
pub const MINUS: char = '\u{2212}';

/// Transport position `Takt.Schlag.Tick` with 240 display ticks per beat, tick zero-padded to three
/// digits: tick 0 → `1.1.000`, `10.2.120`. Negative ticks → [`COUNT_IN_LABEL`].
pub fn fmt_pos(tick: Tick, meter: Meter) -> String {
    if tick < Tick::ZERO {
        return COUNT_IN_LABEL.to_owned();
    }
    let bbt = map(meter).bbt(tick);
    format!("{}.{}.{:03}", bbt.bar, bbt.beat, bbt.tick)
}

/// Position as bar, beat and 1/16 within the beat, all 1-based and unpadded (`9.1.1`, inspector
/// "Start", export range "1.1.1 – 17.1.1"). Negative ticks → [`COUNT_IN_LABEL`].
pub fn fmt_bar_pos(tick: Tick, meter: Meter) -> String {
    if tick < Tick::ZERO {
        return COUNT_IN_LABEL.to_owned();
    }
    let per_beat = meter.ticks_per_beat();
    let bbt = map(meter).bbt(tick);
    let sixteenth = tick.0.rem_euclid(per_beat) / TICKS_PER_STEP + 1;
    format!("{}.{}.{}", bbt.bar, bbt.beat, sixteenth)
}

/// Length as bars, beats and display ticks, all 0-based and unpadded (`2.0.0`, `1.2.96`; inspector
/// "Länge", "Loop-Länge").
pub fn fmt_length(length: Tick, meter: Meter) -> String {
    if length < Tick::ZERO {
        return format!("{MINUS}{}", fmt_length(-length, meter));
    }
    let (per_bar, per_beat) = (meter.ticks_per_bar(), meter.ticks_per_beat());
    let in_bar = length.0 % per_bar;
    format!(
        "{}.{}.{}",
        length.0 / per_bar,
        in_bar / per_beat,
        (in_bar % per_beat) * DISPLAY_TICKS_PER_BEAT / per_beat
    )
}

/// Time `mm:ss.mmm` with zero-padded minutes and seconds (`00:17.902`); negative seconds show as
/// `00:00.000`.
pub fn fmt_time(seconds: f64) -> String {
    // Whole milliseconds first, so 59.9996 s carries into the minute (`01:00.000`).
    let ms = if seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    };
    format!("{:02}:{:02}.{:03}", ms / 60_000, ms / 1000 % 60, ms % 1000)
}

/// Level in dB with one decimal: `−∞` at or below [`crate::project::VOLUME_DB_MIN`] (and for
/// `-inf`), `+1.5` above +0.05, `−3.2` below −0.05, `0.0` in between (prototype `fmtDb`). No unit.
pub fn fmt_db(db: f32) -> String {
    if db.is_nan() || db <= VOLUME_DB_MIN {
        return "\u{2212}\u{221e}".to_owned();
    }
    let sign = if db > 0.05 {
        "+"
    } else if db < -0.05 {
        "\u{2212}"
    } else {
        return "0.0".to_owned();
    };
    // Round half away from zero like JavaScript's `toFixed(1)` on the prototype's values.
    let tenths = (f64::from(db.abs()) * 10.0).round();
    format!("{sign}{:.1}", tenths / 10.0)
}

/// Parses dB text entry (fader double-click): accepts an optional sign (`+`, `-` or `−`), a decimal
/// point or comma, an optional `dB` suffix, and `-inf` / `−∞` for silence (returned as
/// `f32::NEG_INFINITY`). Whitespace is ignored. `None` if unparsable; no clamping.
pub fn parse_db(text: &str) -> Option<f32> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let text = match text.get(text.len().saturating_sub(2)..) {
        Some(unit) if unit.eq_ignore_ascii_case("db") => &text[..text.len() - 2],
        _ => &text,
    };
    let (negative, body) = split_sign(text);
    if body == "\u{221e}" || body.eq_ignore_ascii_case("inf") {
        return negative.then_some(f32::NEG_INFINITY);
    }
    let value = parse_decimal(body)?;
    Some(if negative { -value } else { value })
}

/// Pan as `C` (|pan| < 0.005), `L 12`, `R 50`: side letter, space, `round(|pan| × 50)` (prototype
/// `fmtPan`, so full left is `L 50`).
pub fn fmt_pan(pan: f32) -> String {
    if pan.is_nan() || pan.abs() < 0.005 {
        return "C".to_owned();
    }
    let side = if pan < 0.0 { 'L' } else { 'R' };
    format!("{side} {}", (pan.abs() * 50.0).round() as i32)
}

/// Parses pan text entry: `C`, `L n`, `R n` (n in 0 ..= 50, space optional, case-insensitive), or a
/// plain number `-50 ..= 50`. Returns `-1.0 ..= 1.0`; `None` if unparsable or out of range.
pub fn parse_pan(text: &str) -> Option<f32> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut chars = text.chars();
    let side = match chars.next()?.to_ascii_uppercase() {
        'C' if chars.as_str().is_empty() => return Some(0.0),
        'L' => -1.0,
        'R' => 1.0,
        _ => {
            let (negative, body) = split_sign(&text);
            let value = parse_decimal(body)?;
            let value = if negative { -value } else { value };
            return (-50.0..=50.0).contains(&value).then_some(value / 50.0);
        }
    };
    let amount = parse_decimal(chars.as_str())?;
    (amount <= 50.0).then_some(side * amount / 50.0)
}

/// Note name with sharps and octave, C4 = 60: `C4`, `C#4`, `A-1` for 9 (prototype `noteName`; the
/// octave of pitches 0–11 is `-1` with ASCII hyphen as in the prototype).
pub fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[usize::from(pitch % 12)],
        i32::from(pitch / 12) - 1
    )
}

/// A tempo map for display math (tempo does not matter for bars and beats).
fn map(meter: Meter) -> TempoMap {
    TempoMap::constant(Tempo::DEFAULT, meter)
}

/// Splits an optional leading sign (`+`, `-`, `−`) off `text`; returns whether it was negative.
fn split_sign(text: &str) -> (bool, &str) {
    if let Some(rest) = text.strip_prefix('-').or_else(|| text.strip_prefix(MINUS)) {
        (true, rest)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    }
}

/// Parses unsigned decimal text: digits with at most one `.` or `,`, at least one digit. Rejects
/// what `f32::from_str` would also accept (exponents, `inf`, `NaN`, signs).
fn parse_decimal(text: &str) -> Option<f32> {
    let mut seen_point = false;
    let mut seen_digit = false;
    for c in text.chars() {
        match c {
            '0'..='9' => seen_digit = true,
            '.' | ',' if !seen_point => seen_point = true,
            _ => return None,
        }
    }
    if !seen_digit {
        return None;
    }
    text.replace(',', ".").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Meter = Meter::FOUR_FOUR;

    #[test]
    fn positions_match_the_prototype() {
        // Prototype `fmtPos` takes 1/16 steps; `Tick::from_steps` converts.
        let cases: [(i64, &str); 6] = [
            (0, "1.1.000"),
            (1, "1.1.060"),
            (16, "2.1.000"),
            (150, "10.2.120"),
            (128, "9.1.000"),
            (-1, "Vorzähler"),
        ];
        for (steps, text) in cases {
            assert_eq!(fmt_pos(Tick::from_steps(steps), M), text, "{steps} steps");
        }
        // Display ticks floor to 4 ticks.
        assert_eq!(fmt_pos(Tick(9 * 3840 + 2 * 960 + 387), M), "10.3.096");
        assert_eq!(fmt_pos(Tick(3839), M), "1.4.239");
        assert_eq!(
            fmt_pos(Tick(2 * 2880 + 480), Meter { num: 6, den: 8 }),
            "3.2.000"
        );
    }

    #[test]
    fn bar_positions_match_the_screens() {
        // S01/S03 inspector "Start 9.1.1", S08 export range "1.1.1 – 17.1.1".
        assert_eq!(fmt_bar_pos(Tick(8 * 3840), M), "9.1.1");
        assert_eq!(fmt_bar_pos(Tick::ZERO, M), "1.1.1");
        assert_eq!(fmt_bar_pos(Tick(16 * 3840), M), "17.1.1");
        // S02 inspector "Start 13.1.1".
        assert_eq!(fmt_bar_pos(Tick(12 * 3840), M), "13.1.1");
        assert_eq!(fmt_bar_pos(Tick(960 + 3 * 240 + 239), M), "1.2.4");
        assert_eq!(fmt_bar_pos(Tick(-1), M), COUNT_IN_LABEL);
    }

    #[test]
    fn lengths_match_the_screens() {
        // S01 "Länge 2.0.0", S03 "Länge 4.0.0", S02 "Länge 1.2.96" (1.6 bars).
        assert_eq!(fmt_length(Tick(2 * 3840), M), "2.0.0");
        assert_eq!(fmt_length(Tick(4 * 3840), M), "4.0.0");
        assert_eq!(fmt_length(Tick(3840 * 16 / 10), M), "1.2.96");
        assert_eq!(fmt_length(Tick(240), M), "0.0.60");
        assert_eq!(fmt_length(Tick::ZERO, M), "0.0.0");
        assert_eq!(fmt_length(Tick(-960), M), "\u{2212}0.1.0");
    }

    #[test]
    fn times_match_the_prototype() {
        let cases: [(f64, &str); 9] = [
            (0.0, "00:00.000"),
            (-1.0, "00:00.000"),
            // S01: 37 beats at 124 bpm.
            (37.0 * 60.0 / 124.0, "00:17.903"),
            // S02 shows "00:26.322" for 14.3.96 (54.4 beats); the prototype rounds to .323.
            (54.4 * 60.0 / 124.0, "00:26.323"),
            (61.5, "01:01.500"),
            // The prototype rounds the seconds alone and shows "00:60.000" here.
            (59.9996, "01:00.000"),
            (0.0004, "00:00.000"),
            (3599.999, "59:59.999"),
            (6000.0, "100:00.000"),
        ];
        for (seconds, text) in cases {
            assert_eq!(fmt_time(seconds), text, "{seconds} s");
        }
        assert_eq!(fmt_time(f64::NAN), "00:00.000");
    }

    #[test]
    fn levels_match_the_prototype() {
        let cases: [(f32, &str); 12] = [
            (f32::NEG_INFINITY, "−∞"),
            (-70.0, "−∞"),
            (-80.0, "−∞"),
            (-69.94, "−69.9"),
            (1.5, "+1.5"),
            (-3.2, "−3.2"),
            (0.0, "0.0"),
            (0.04, "0.0"),
            // The prototype shows an unsigned "0.1" at exactly ±0.05; the contract says "0.0".
            (-0.05, "0.0"),
            (0.06, "+0.1"),
            (-6.0, "−6.0"),
            (6.0, "+6.0"),
        ];
        for (db, text) in cases {
            assert_eq!(fmt_db(db), text, "{db} dB");
        }
        assert_eq!(fmt_db(-1.25), "−1.3");
        assert_eq!(fmt_db(f32::NAN), "−∞");
        assert!(fmt_db(-3.2).starts_with(MINUS));
    }

    #[test]
    fn level_text_entry() {
        assert_eq!(parse_db("-3.2"), Some(-3.2));
        assert_eq!(parse_db("−3,2 dB"), Some(-3.2));
        assert_eq!(parse_db("+1.5"), Some(1.5));
        assert_eq!(parse_db(" 6 db "), Some(6.0));
        assert_eq!(parse_db("0"), Some(0.0));
        assert_eq!(parse_db(".5"), Some(0.5));
        assert_eq!(parse_db("-inf"), Some(f32::NEG_INFINITY));
        assert_eq!(parse_db("−∞"), Some(f32::NEG_INFINITY));
        assert_eq!(parse_db("−∞ dB"), Some(f32::NEG_INFINITY));
        assert_eq!(parse_db("-120"), Some(-120.0));
        for bad in [
            "", "dB", "-", "inf", "∞", "1e3", "NaN", "1.2.3", "abc", "--3", "3-",
        ] {
            assert_eq!(parse_db(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn pans_match_the_prototype() {
        let cases: [(f32, &str); 8] = [
            (0.0, "C"),
            (0.004, "C"),
            (-0.004, "C"),
            (-0.24, "L 12"),
            (1.0, "R 50"),
            (-1.0, "L 50"),
            (0.2, "R 10"),
            (-0.1, "L 5"),
        ];
        for (pan, text) in cases {
            assert_eq!(fmt_pan(pan), text, "{pan}");
        }
        assert_eq!(fmt_pan(f32::NAN), "C");
    }

    #[test]
    fn pan_text_entry() {
        assert_eq!(parse_pan("C"), Some(0.0));
        assert_eq!(parse_pan(" c "), Some(0.0));
        assert_eq!(parse_pan("L 12"), Some(-0.24));
        assert_eq!(parse_pan("l12"), Some(-0.24));
        assert_eq!(parse_pan("R 50"), Some(1.0));
        assert_eq!(parse_pan("R0"), Some(0.0));
        assert_eq!(parse_pan("-50"), Some(-1.0));
        assert_eq!(parse_pan("−25"), Some(-0.5));
        assert_eq!(parse_pan("+10"), Some(0.2));
        assert_eq!(parse_pan("12,5"), Some(0.25));
        for bad in ["", "L", "R 51", "L -5", "51", "-50.5", "C5", "X 3", "LR 3"] {
            assert_eq!(parse_pan(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn note_names_match_the_prototype() {
        let cases: [(u8, &str); 7] = [
            (60, "C4"),
            (61, "C#4"),
            (69, "A4"),
            (9, "A-1"),
            (0, "C-1"),
            (127, "G9"),
            (36, "C2"),
        ];
        for (pitch, text) in cases {
            assert_eq!(note_name(pitch), text);
        }
    }

    #[test]
    fn formatted_values_round_trip_through_parsing() {
        for pan in [-1.0f32, -0.5, -0.24, 0.0, 0.3, 1.0] {
            assert_eq!(parse_pan(&fmt_pan(pan)), Some(pan));
        }
        for db in [-60.0f32, -3.2, 0.0, 1.5, 6.0] {
            assert_eq!(parse_db(&fmt_db(db)), Some(db));
        }
    }
}
