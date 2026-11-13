//! Pure formatting of values for the UI (ARCHITECTURE §5.4).
//!
//! Rules from `ks-engine.js` (`fmtPos`, `fmtTime`, `fmtDb`, `fmtPan`, `noteName`) and the screens.
//! Typography is part of the contract: the minus sign of numeric values (dB, pan input) is U+2212
//! (`−`), never `-`; digits are meant for tabular (monospace) display.

use crate::time::{Meter, Tick};

/// What [`fmt_pos`] shows before tick 0, during the count-in.
pub const COUNT_IN_LABEL: &str = "Vorzähler";

/// The minus sign used in all formatted values (U+2212).
pub const MINUS: char = '\u{2212}';

/// Transport position `Takt.Schlag.Tick` with 240 display ticks per beat, tick zero-padded to three
/// digits: tick 0 → `1.1.000`, `10.2.120`. Negative ticks → [`COUNT_IN_LABEL`].
pub fn fmt_pos(tick: Tick, meter: Meter) -> String {
    todo!("WP-12")
}

/// Position as bar, beat and 1/16 within the beat, all 1-based and unpadded (`9.1.1`, inspector
/// "Start", export range "1.1.1 – 17.1.1"). Negative ticks → [`COUNT_IN_LABEL`].
pub fn fmt_bar_pos(tick: Tick, meter: Meter) -> String {
    todo!("WP-12")
}

/// Length as bars, beats and display ticks, all 0-based and unpadded (`2.0.0`, `1.2.96`; inspector
/// "Länge", "Loop-Länge").
pub fn fmt_length(length: Tick, meter: Meter) -> String {
    todo!("WP-12")
}

/// Time `mm:ss.mmm` with zero-padded minutes and seconds (`00:17.902`); negative seconds show as
/// `00:00.000`.
pub fn fmt_time(seconds: f64) -> String {
    todo!("WP-12")
}

/// Level in dB with one decimal: `−∞` at or below [`crate::project::VOLUME_DB_MIN`] (and for
/// `-inf`), `+1.5` above +0.05, `−3.2` below −0.05, `0.0` in between (prototype `fmtDb`). No unit.
pub fn fmt_db(db: f32) -> String {
    todo!("WP-12")
}

/// Parses dB text entry (fader double-click): accepts an optional sign (`+`, `-` or `−`), a decimal
/// point or comma, an optional `dB` suffix, and `-inf` / `−∞` for silence (returned as
/// `f32::NEG_INFINITY`). Whitespace is ignored. `None` if unparsable; no clamping.
pub fn parse_db(text: &str) -> Option<f32> {
    todo!("WP-12")
}

/// Pan as `C` (|pan| < 0.005), `L 12`, `R 50`: side letter, space, `round(|pan| × 50)` (prototype
/// `fmtPan`, so full left is `L 50`).
pub fn fmt_pan(pan: f32) -> String {
    todo!("WP-12")
}

/// Parses pan text entry: `C`, `L n`, `R n` (n in 0 ..= 50, space optional, case-insensitive), or a
/// plain number `-50 ..= 50`. Returns `-1.0 ..= 1.0`; `None` if unparsable or out of range.
pub fn parse_pan(text: &str) -> Option<f32> {
    todo!("WP-12")
}

/// Note name with sharps and octave, C4 = 60: `C4`, `C#4`, `A-1` for 9 (prototype `noteName`; the
/// octave of pitches 0–11 is `-1` with ASCII hyphen as in the prototype).
pub fn note_name(pitch: u8) -> String {
    todo!("WP-12")
}
