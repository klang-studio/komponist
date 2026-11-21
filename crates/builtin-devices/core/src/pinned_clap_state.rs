//! Parameter mapping and portable state for the pinned bundled CLAP wrapper.

use komponist_model::params::{ParamInfo, ParamKind};
use serde_json::{Map, Value, json};

/// Shared persisted field used by both sampler CLAPs.
pub const SAMPLE_ASSETS_FIELD: &str = "sample_assets";
/// Maximum size accepted by the sampler CLAP state readers.
pub const MAX_SAMPLE_ASSET_BYTES: usize = 256 * 1024;
/// The canonical factory preset name retained across edits and CLAP state saves.
pub const FACTORY_PRESET_FIELD: &str = "factory_preset";

/// The CLAP plain value of a built-in parameter, using the adapter's published range.
pub fn to_clap(info: &ParamInfo, plain: f64) -> f64 {
    let plain = info.snap(plain);
    match info.kind {
        ParamKind::Float { steps: 0, .. } => info.to_normalized(plain),
        ParamKind::Float {
            min, max, steps, ..
        } => ((plain - min) / ((max - min) / f64::from(steps))).round(),
        _ => plain,
    }
}

/// Convert a CLAP plain value back to the built-in parameter's display domain.
pub fn from_clap(info: &ParamInfo, raw: f64) -> f64 {
    match info.kind {
        ParamKind::Float { steps: 0, .. } => info.to_plain(raw),
        ParamKind::Float {
            min, max, steps, ..
        } => info.snap(min + raw * ((max - min) / f64::from(steps))),
        _ => info.snap(raw),
    }
}

/// The inclusive CLAP plain-value range published by the bundled adapter.
pub fn clap_range(info: &ParamInfo) -> (f64, f64) {
    match info.kind {
        ParamKind::Float { steps, .. } => (0.0, f64::from(steps.max(1))),
        _ => info.range(),
    }
}

/// Encode a built-in plain value in the pinned adapter's parameter representation.
/// The second result is the wrapper value used for matching an audio patch snapshot.
pub fn parameter(info: &ParamInfo, plain: f64) -> (Value, f64) {
    let raw = to_clap(info, plain);
    match info.kind {
        ParamKind::Float { steps: 0, .. } => {
            let value = raw as f32;
            (json!({"f32": value}), f64::from(value))
        }
        ParamKind::Bool => (json!({"bool": raw >= 0.5}), raw),
        _ => (json!({"i32": raw as i32}), raw),
    }
}

/// Read a complete uncompressed state document from the pinned wrapper.
pub fn decode(state: &[u8]) -> Option<Value> {
    let (length, payload) = state.split_first_chunk::<8>()?;
    if u64::from_le_bytes(*length) != payload.len() as u64 {
        return None;
    }
    serde_json::from_slice(payload).ok()
}

/// Write the pinned wrapper's uncompressed CLAP state stream.
/// Persisted fields contain JSON strings, as required by the wrapper.
pub fn encode(version: &str, params: Map<String, Value>, fields: Map<String, Value>) -> Vec<u8> {
    let state = json!({"version": version, "params": params, "fields": fields});
    let payload = serde_json::to_vec(&state).expect("wrapper state JSON value encodes");
    let mut result = Vec::with_capacity(8 + payload.len());
    result.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    result.extend_from_slice(&payload);
    result
}
