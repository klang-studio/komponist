//! Portable state encoding for migration to the pinned bundled CLAP wrapper.

use komponist_model::params::{ParamInfo, ParamKind};
use serde_json::{Map, Value, json};

/// Shared persisted field used by both sampler CLAPs.
pub const SAMPLE_ASSETS_FIELD: &str = "sample_assets";
/// Maximum size accepted by the sampler CLAP state readers.
pub const MAX_SAMPLE_ASSET_BYTES: usize = 256 * 1024;
/// The canonical factory preset name retained across edits and CLAP state saves.
pub const FACTORY_PRESET_FIELD: &str = "factory_preset";

/// Encode a built-in plain value in the pinned adapter's parameter representation.
/// The second result is the wrapper value used for matching an audio patch snapshot.
pub fn parameter(info: &ParamInfo, plain: f64) -> (Value, f64) {
    let plain = info.snap(plain);
    match info.kind {
        ParamKind::Float { steps: 0, .. } => {
            let value = info.to_normalized(plain) as f32;
            (json!({"f32": value}), f64::from(value))
        }
        ParamKind::Float {
            min, max, steps, ..
        } => {
            let step = (max - min) / f64::from(steps);
            let value = ((plain - min) / step).round() as i32;
            (json!({"i32": value}), f64::from(value))
        }
        ParamKind::Int { .. } | ParamKind::Enum(_) => {
            let value = plain as i32;
            (json!({"i32": value}), f64::from(value))
        }
        ParamKind::Bool => {
            let value = plain >= 0.5;
            (json!({"bool": value}), f64::from(u8::from(value)))
        }
    }
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
