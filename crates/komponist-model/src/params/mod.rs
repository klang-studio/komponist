//! The parameter model of internal devices **and** bridged CLAP/VST3 plugins (device handoff §6).
//!
//! One description, [`ParamInfo`], for everything a device card, the plugin frame's grid, the
//! inspector, the context menu and (later) automation and MIDI learn address:
//!
//! - [`ParamId`]: the stable name a project saves (`"op1_attack"`, `"p42"` for plugin id 42).
//! - [`ParamKind`]: `Float { min, max, scale, steps }`, `Int`, `Enum`, `Bool`; plain values are
//!   `f64` in the kind's own range (Hz, seconds, dB, a step index).
//! - Normalised 0–1 values and the mapping per [`Scale`]: [`ParamInfo::to_normalized`],
//!   [`ParamInfo::to_plain`] (the latter snaps stepped kinds).
//! - Texts per [`Unit`]: [`ParamInfo::format`] (`420 Hz`, `1.2 kHz`, `−4.0 dB`, `L 14`, `C`).
//! - `automatable` / `modulatable`: only carried, nothing uses them yet (owner decision
//!   2026-11-14: automation, MIDI learn and modulation are prepared, not built).
//! - [`layout`]: the auto-layout of foreign plugins (macros or the first 12, 4 columns).
//! - [`context`]: placeholders such as `op{env_op}_attack` and their resolver.
//!
//! Why here: `komponist-model` is the one crate every other crate already depends on (engine,
//! builtin devices, plugin manager, app, UI) and it holds the display formatting
//! ([`crate::format`]) and the fader taper the `Db` scale shares. It depends on no other studio
//! crate, so the adapters from foreign types live where those types are: bridged plugins in
//! `komponist-plugins`, pisynth's macros in `komponist-builtin`.
//!
//! Plain values are what travels to devices (`Command::SetParam`, `PluginAction::SetParam`); the
//! normalised value is for knobs, automation lanes and modulation.

pub mod context;
pub mod eq_curve;
mod format;
pub mod layout;
mod scale;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use context::{ParamContext, ResolveError, placeholders, resolve};
pub use layout::{AUTO_COLUMNS, AUTO_MAX, auto_layout};
pub use scale::{DBT, SILENCE_POSITION, db_to_p, p_to_db};

/// The stable id of a parameter, saved in projects (automation, mappings later). Internal devices
/// use readable keys (`"op1_attack"`), bridged plugins `"p<id>"` ([`ParamId::plugin`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamId(String);

impl ParamId {
    /// An id from its key.
    pub fn new(key: impl Into<String>) -> Self {
        ParamId(key.into())
    }

    /// The id of a bridged plugin's parameter (CLAP `clap_id`, VST3 `ParamID`): `p42`.
    pub fn plugin(id: u32) -> Self {
        ParamId(format!("p{id}"))
    }

    /// The plugin parameter id of `p42`, `None` for any other key.
    pub fn plugin_id(&self) -> Option<u32> {
        let digits = self.0.strip_prefix('p')?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }

    /// The key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ParamId {
    fn from(key: &str) -> Self {
        ParamId::new(key)
    }
}

/// How a float's plain range maps onto 0–1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scale {
    /// Linear.
    Lin,
    /// Logarithmic, for frequency and time (needs `min > 0`, else linear).
    Log,
    /// Decibels on the fader taper ([`DBT`]); needs `−70 ≤ min < max ≤ 6`, else linear.
    Db,
    /// Linear and centred: −1 (left) … +1 (right), shown as `L 14` / `C` / `R 14`.
    Pan,
}

/// What a parameter holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParamKind {
    /// A number; `steps > 0` makes it stepped (`steps` equal steps from `min` to `max`).
    Float {
        /// Lowest plain value.
        min: f64,
        /// Highest plain value.
        max: f64,
        /// Mapping to 0–1.
        scale: Scale,
        /// Steps between `min` and `max`, 0 = continuous.
        steps: u32,
    },
    /// A whole number `min ..= max`.
    Int {
        /// Lowest value.
        min: i32,
        /// Highest value.
        max: i32,
    },
    /// One of the named choices; the plain value is the index.
    Enum(Vec<String>),
    /// Off (0) or on (1).
    Bool,
}

impl ParamKind {
    /// A continuous linear float.
    pub fn lin(min: f64, max: f64) -> Self {
        ParamKind::Float {
            min,
            max,
            scale: Scale::Lin,
            steps: 0,
        }
    }

    /// A continuous float with `scale`.
    pub fn scaled(min: f64, max: f64, scale: Scale) -> Self {
        ParamKind::Float {
            min,
            max,
            scale,
            steps: 0,
        }
    }
}

/// The unit a value is shown in (see [`ParamInfo::format`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit {
    /// No unit: plain number, integer, choice name.
    None,
    /// Hertz (`420 Hz`, `1.2 kHz` from 1000).
    Hz,
    /// Milliseconds (`8.0 ms`, `375 ms`, `1.20 s` from 1000).
    Ms,
    /// Seconds (`4 ms` below 1 s, `1.20 s`, `12.0 s`).
    S,
    /// Decibels (`−4.0 dB`, `+1.5 dB`, `0.0 dB`).
    Db,
    /// A fraction 0–1 shown as percent (`40 %`).
    Percent,
    /// Semitones (`+7 st`).
    Semitones,
    /// Cents (`−12 ct`).
    Cents,
    /// Octaves (`−1 oct`).
    Octaves,
    /// A ratio (`4.0:1`).
    Ratio,
    /// Metres (`28 m`).
    Meters,
}

/// A custom text for a plain value, for devices whose values need their own words ("Frei").
pub type FormatFn = fn(f64) -> String;

/// A [`FormatFn`] that compares by address (two parameters with the same function are equal; a
/// duplicated function counting as different only costs a redundant redraw).
#[derive(Clone, Copy)]
pub struct Format(pub FormatFn);

impl PartialEq for Format {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.0, other.0)
    }
}

impl fmt::Debug for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Format(..)")
    }
}

/// Everything about one parameter except its current value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    /// Stable id.
    pub id: ParamId,
    /// Name under the knob.
    pub name: String,
    /// Kind and range.
    pub kind: ParamKind,
    /// Default plain value (Alt-click, "Auf Standard").
    pub default: f64,
    /// Unit of the text.
    pub unit: Unit,
    /// Own text for a plain value instead of the unit's; not saved.
    #[serde(skip)]
    pub format: Option<Format>,
    /// May be automated (carried only).
    pub automatable: bool,
    /// May be modulated (carried only).
    pub modulatable: bool,
    /// The plugin reports it as a macro: the auto-layout shows these first.
    pub is_macro: bool,
}

impl ParamInfo {
    /// A parameter with unit [`Unit::None`], automatable, not modulatable, no macro.
    pub fn new(
        id: impl Into<ParamId>,
        name: impl Into<String>,
        kind: ParamKind,
        default: f64,
    ) -> Self {
        ParamInfo {
            id: id.into(),
            name: name.into(),
            kind,
            default,
            unit: Unit::None,
            format: None,
            automatable: true,
            modulatable: false,
            is_macro: false,
        }
    }

    /// Sets the unit.
    #[must_use]
    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    /// Sets an own text function.
    #[must_use]
    pub fn formatted(mut self, format: FormatFn) -> Self {
        self.format = Some(Format(format));
        self
    }

    /// Sets the flags.
    #[must_use]
    pub fn flags(mut self, automatable: bool, modulatable: bool) -> Self {
        self.automatable = automatable;
        self.modulatable = modulatable;
        self
    }

    /// Lowest and highest plain value.
    pub fn range(&self) -> (f64, f64) {
        match &self.kind {
            ParamKind::Float { min, max, .. } => (*min, *max),
            ParamKind::Int { min, max } => (f64::from(*min), f64::from(*max)),
            ParamKind::Enum(names) => (0.0, names.len().saturating_sub(1) as f64),
            ParamKind::Bool => (0.0, 1.0),
        }
    }

    /// Steps between lowest and highest value, 0 = continuous.
    pub fn steps(&self) -> u32 {
        match &self.kind {
            ParamKind::Float { steps, .. } => *steps,
            ParamKind::Int { min, max } => max.saturating_sub(*min).max(0) as u32,
            ParamKind::Enum(names) => names.len().saturating_sub(1) as u32,
            ParamKind::Bool => 1,
        }
    }

    /// Whether the value takes discrete steps.
    pub fn is_stepped(&self) -> bool {
        self.steps() > 0
    }

    /// `plain` clamped to the range and, for stepped kinds, on a step. NaN gives the default.
    pub fn snap(&self, plain: f64) -> f64 {
        if plain.is_nan() {
            return self.default;
        }
        let (min, max) = self.range();
        let v = plain.clamp(min.min(max), max.max(min));
        let steps = self.steps();
        if steps == 0 || max <= min {
            return v;
        }
        let step = (max - min) / f64::from(steps);
        (min + ((v - min) / step).round() * step).clamp(min, max)
    }

    /// The 0–1 position of `plain` (clamped); stepped kinds are not snapped.
    pub fn to_normalized(&self, plain: f64) -> f64 {
        let (min, max) = self.range();
        if plain.is_nan() || max <= min {
            return 0.0;
        }
        let v = plain.clamp(min, max);
        match &self.kind {
            ParamKind::Float { scale, .. } => scale::to_normalized(*scale, min, max, v),
            _ => (v - min) / (max - min),
        }
    }

    /// The plain value at 0–1 position `normalized` (clamped), snapped for stepped kinds.
    pub fn to_plain(&self, normalized: f64) -> f64 {
        let (min, max) = self.range();
        if normalized.is_nan() || max <= min {
            return min;
        }
        let n = normalized.clamp(0.0, 1.0);
        let plain = match &self.kind {
            ParamKind::Float { scale, .. } => scale::to_plain(*scale, min, max, n),
            _ => min + n * (max - min),
        };
        self.snap(plain)
    }

    /// The default as a 0–1 position.
    pub fn default_normalized(&self) -> f64 {
        self.to_normalized(self.default)
    }

    /// The text of a plain value: the own [`ParamInfo::format`] if set, else per kind and unit
    /// (`420 Hz`, `1.2 kHz`, `−4.0 dB`, `L 14`, `C`).
    pub fn format(&self, plain: f64) -> String {
        match self.format {
            Some(Format(f)) => f(plain),
            None => format::value_text(self, plain),
        }
    }
}

#[cfg(test)]
mod tests;
