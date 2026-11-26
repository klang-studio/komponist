//! Bounded control-thread sampler UI data. Audio and sample ownership stay in the plugin.
use std::ffi::CStr;

use serde::{Deserialize, Serialize};

pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.sampler-ui";
pub const MAX_BYTES: usize = 32 * 1024;
pub const MAX_PADS: usize = 8;
pub const MAX_PEAKS: usize = 512;
pub const MAX_TRANSIENTS: usize = 256;
pub const MAX_TEXT_BYTES: usize = 256;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Snapshot {
    pub pads: Vec<Pad>,
    pub last_hit: (u32, u8),
    pub transients: Vec<f64>,
    pub sync: Option<Sync>,
    pub source_rate: Option<u32>,
    pub idle: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Pad {
    pub status: Status,
    pub name: String,
    pub length: f64,
    pub peaks: Vec<f32>,
    pub playhead: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Status {
    Empty,
    Loading,
    Ready,
    Missing,
    Failed(String),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum Sync {
    Ready,
    Preparing,
    Unavailable,
}

impl Snapshot {
    fn valid(&self) -> bool {
        (1..=MAX_PADS).contains(&self.pads.len())
            && (self.last_hit.1 as usize) < self.pads.len()
            && self.transients.len() <= MAX_TRANSIENTS
            && self
                .transients
                .iter()
                .all(|time| time.is_finite() && *time >= 0.0)
            && self.source_rate.is_none_or(|rate| rate != 0)
            && self.pads.iter().all(|pad| {
                pad.length.is_finite()
                    && pad.length >= 0.0
                    && pad.name.len() <= MAX_TEXT_BYTES
                    && pad.peaks.len() <= MAX_PEAKS
                    && pad
                        .peaks
                        .iter()
                        .all(|peak| peak.is_finite() && (0.0..=1.0).contains(peak))
                    && pad
                        .playhead
                        .is_none_or(|p| p.is_finite() && (0.0..=1.0).contains(&p))
                    && match &pad.status {
                        Status::Failed(reason) => reason.len() <= MAX_TEXT_BYTES,
                        _ => true,
                    }
            })
    }

    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        if !self.valid() {
            return None;
        }
        let size = out.len().min(MAX_BYTES);
        postcard::to_slice(self, &mut out[..size])
            .ok()
            .map(|bytes| bytes.len())
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_BYTES {
            return None;
        }
        let (snapshot, remaining) = postcard::take_from_bytes::<Self>(bytes).ok()?;
        (remaining.is_empty() && snapshot.valid()).then_some(snapshot)
    }
}
