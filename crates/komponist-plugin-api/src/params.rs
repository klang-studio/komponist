//! Parameter metadata and values shared by hosts and layout validators.

use serde::{Deserialize, Serialize};

/// Parameter flag bits ([`ParamInfo::flags`]).
pub mod param_flags {
    /// Integer steps (`step_count` > 0).
    pub const STEPPED: u32 = 1;
    /// The plugin's bypass parameter.
    pub const BYPASS: u32 = 2;
    /// Not shown in generic views.
    pub const HIDDEN: u32 = 4;
    /// Read-only (meters, outputs).
    pub const READ_ONLY: u32 = 8;
    /// Automatable.
    pub const AUTOMATABLE: u32 = 16;
    /// Enumerated values: show [`super::ParamValue::text`].
    pub const ENUM: u32 = 32;
    /// The plugin reports the parameter as a macro: the auto-layout shows these first. No host
    /// sets it yet (neither CLAP nor VST3 has the concept; reserved for plugins that tell).
    pub const MACRO: u32 = 64;
}

/// A plugin parameter as the generic parameter view needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    /// Stable parameter id (CLAP `clap_id`, VST3 `ParamID`).
    pub id: u32,
    /// Display name ("Cutoff").
    pub name: String,
    /// Group path ("Filter/Env"), empty if none.
    pub module: String,
    /// Lowest plain value. VST3 parameters are exposed normalized (0..1, stepped = `step_count`
    /// steps of 1/n), because only the controller can convert to plain values; use the plugin's
    /// value text for display.
    pub min: f64,
    /// Highest plain value (1.0 for VST3, see `min`).
    pub max: f64,
    /// Default plain value (Alt-click).
    pub default: f64,
    /// Number of steps for stepped parameters (0 = continuous).
    pub step_count: u32,
    /// [`param_flags`] bits.
    pub flags: u32,
}

/// A parameter value with the plugin's own text for it ("80 Hz", "−1.5 dB").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamValue {
    /// Parameter id.
    pub id: u32,
    /// Plain value.
    pub value: f64,
    /// Text from the plugin (CLAP `value_to_text`, VST3 `getParamStringByValue`); empty if none.
    pub text: String,
}
