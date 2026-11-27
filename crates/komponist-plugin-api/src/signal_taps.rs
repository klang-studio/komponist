//! Named, bounded CLAP signal streams requested by a visible graph.

use core::ffi::{CStr, c_void};

use serde::{Deserialize, Serialize};

/// Plugin extension ID. Inventory is read on the main thread at initialization.
pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.signal-taps";
/// Maximum number of streams in one instance.
pub const MAX_TAPS: usize = 4;
/// Maximum samples per stream in a process block, one per audio frame.
pub const MAX_BLOCK_SAMPLES: usize = 2048;
/// Largest ring capacity the host allocates per stream.
pub const MAX_CAPACITY: u32 = 65_536;

/// A mono audio stream which a host can transform into a spectrum.
pub const SPECTRUM_SOURCE: u32 = 1;
/// A level history stream, in dBFS.
pub const LEVEL_HISTORY: u32 = 2;
/// Gain reduction over time, in positive dB.
pub const REDUCTION: u32 = 3;
/// Position in the bound audio asset, normalized to 0–1. A value of −1 means idle.
pub const PLAYHEAD_POSITION: u32 = 4;
/// One atomic activity frame for a layout's pad bank at the end of an audio block.
pub const PAD_ACTIVITY: u32 = 5;
/// Number of counter and pad-index samples before the playhead positions in a frame.
pub const PAD_ACTIVITY_HEADER: usize = 3;
/// Buffer size that can hold a pad activity frame of the maximum permitted size.
pub const MAX_PAD_ACTIVITY_FRAME: usize = PAD_ACTIVITY_HEADER + crate::native_layout::MAX_PADS;

/// A bounded activity frame for a pad-selected panel.
#[derive(Clone, Debug, PartialEq)]
pub struct PadActivity {
    pub hit: u32,
    pub last_pad: usize,
    pub playheads: [Option<f32>; crate::native_layout::MAX_PADS],
}

impl PadActivity {
    /// Encode a frame as exact finite f32 samples, preserving all bits of the hit counter.
    pub fn write(
        out: &mut [f32],
        hit: u32,
        last_pad: usize,
        positions: impl Iterator<Item = Option<f32>>,
    ) {
        let [lo, hi, pad, positions_out @ ..] = out else {
            return;
        };
        *lo = f32::from(hit as u16);
        *hi = f32::from((hit >> 16) as u16);
        *pad = last_pad as f32;
        let mut positions = positions;
        for slot in positions_out {
            *slot = positions.next().flatten().unwrap_or(-1.0);
        }
    }

    /// Accept only complete frames whose counters, selection and positions are in range.
    pub fn decode(samples: &[f32], count: usize) -> Option<Self> {
        let [lo, hi, pad, positions @ ..] = samples else {
            return None;
        };
        let integer = |value: f32, limit: u16| {
            ((0.0..=f32::from(limit)).contains(&value) && value.fract() == 0.0)
                .then_some(value as u16)
        };
        let hit = u32::from(integer(*lo, u16::MAX)?) | (u32::from(integer(*hi, u16::MAX)?) << 16);
        if !(1..=crate::native_layout::MAX_PADS).contains(&count)
            || samples.len() != MAX_PAD_ACTIVITY_FRAME
            || positions[count..].iter().any(|&position| position != -1.0)
        {
            return None;
        }
        let last_pad = usize::from(integer(*pad, count as u16 - 1)?);
        let mut playheads = [None; crate::native_layout::MAX_PADS];
        for (entry, &position) in playheads.iter_mut().zip(positions) {
            match position {
                -1.0 => {}
                value if (0.0..=1.0).contains(&value) => *entry = Some(value),
                _ => return None,
            }
        }
        Some(Self {
            hit,
            last_pad,
            playheads,
        })
    }
}

/// One tap from the main-thread inventory. The index is stable for the instance's lifetime.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    /// Stable ASCII key with zero padding, used by the layout.
    pub name: [u8; 32],
    /// One of the stream kinds above.
    pub kind: u32,
    /// One reported sample per this many audio frames.
    pub audio_frames_per_sample: u32,
    /// If nonzero, scale the frame interval by the nearest whole ratio to this rate.
    pub reference_rate: u32,
    /// Requested capacity in history samples, including room for the writer.
    pub capacity: u32,
}

impl Descriptor {
    /// Zeroed output for the inventory callback.
    pub const EMPTY: Self = Self {
        name: [0; 32],
        kind: 0,
        audio_frames_per_sample: 0,
        reference_rate: 0,
        capacity: 0,
    };

    /// Construct an inventory entry with a fixed key; the host validates its value on load.
    pub const fn new(key: &str, kind: u32, audio_frames_per_sample: u32, capacity: u32) -> Self {
        let bytes = key.as_bytes();
        assert!(bytes.len() <= 32);
        let mut name = [0; 32];
        let mut i = 0;
        while i < bytes.len() {
            name[i] = bytes[i];
            i += 1;
        }
        Self {
            name,
            kind,
            audio_frames_per_sample,
            reference_rate: 0,
            capacity,
        }
    }

    /// Set the nominal audio clock for rate-scaled reporting intervals.
    pub const fn at_reference_rate(mut self, rate: u32) -> Self {
        self.reference_rate = rate;
        self
    }

    /// Frame interval at the actual audio rate.
    pub fn frame_interval(&self, audio_rate: f32) -> u32 {
        let multiplier = if self.reference_rate == 0 {
            1
        } else {
            (audio_rate / self.reference_rate as f32).round().max(1.0) as u32
        };
        self.audio_frames_per_sample.saturating_mul(multiplier)
    }

    /// Actual samples per second of the stream.
    pub fn sample_rate(&self, audio_rate: f32) -> f32 {
        audio_rate / self.frame_interval(audio_rate).max(1) as f32
    }

    /// A validated layout key. It starts with a lowercase letter.
    pub fn key(&self) -> Option<&str> {
        let end = self
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.name.len());
        let (name, padding) = self.name.split_at(end);
        let name = std::str::from_utf8(name).ok()?;
        (valid_key(name) && padding.iter().all(|&b| b == 0)).then_some(name)
    }

    /// Validates inventory metadata before it can affect allocation or display.
    pub fn valid(&self) -> bool {
        self.key().is_some()
            && matches!(
                self.kind,
                SPECTRUM_SOURCE | LEVEL_HISTORY | REDUCTION | PLAYHEAD_POSITION | PAD_ACTIVITY
            )
            && (1..=MAX_BLOCK_SAMPLES as u32).contains(&self.audio_frames_per_sample)
            && (self.reference_rate == 0 || (8_000..=384_000).contains(&self.reference_rate))
            && (2..=MAX_CAPACITY).contains(&self.capacity)
            && (self.kind != PAD_ACTIVITY || self.capacity >= 2 * MAX_PAD_ACTIVITY_FRAME as u32)
    }
}

/// Syntax shared by an inventory key and its layout references.
pub fn valid_key(name: &str) -> bool {
    name.len() <= 32
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

/// An inventory has one stable name for each bit in the request mask.
pub fn valid_inventory(inventory: &[Descriptor]) -> bool {
    !inventory.is_empty()
        && inventory.len() <= MAX_TAPS
        && inventory
            .iter()
            .enumerate()
            .all(|(i, d)| d.valid() && inventory[..i].iter().all(|other| other.name != d.name))
}

/// The plugin's C callback table. No callback runs on the audio thread when the request mask is zero.
#[repr(C)]
pub struct Extension {
    /// Returns the number of immutable descriptors.
    pub count: Option<unsafe extern "C" fn(plugin: *const c_void) -> u32>,
    /// Writes one descriptor and returns 1, or reports absence with 0.
    pub info: Option<
        unsafe extern "C" fn(plugin: *const c_void, index: u32, out: *mut Descriptor) -> u32,
    >,
    /// Starts a collection interval before processing. Set bits identify the requested tap indices.
    pub begin: Option<unsafe extern "C" fn(plugin: *const c_void, mask: u32)>,
    /// After processing, writes up to `capacity` finite samples in time order and returns their count.
    pub read: Option<
        unsafe extern "C" fn(
            plugin: *const c_void,
            index: u32,
            out: *mut f32,
            capacity: u32,
        ) -> u32,
    >,
}

/// Bounded readings from one completed process call. Samples beyond each count are ignored.
#[repr(C)]
pub struct BlockTaps {
    counts: [u32; MAX_TAPS],
    samples: [[f32; MAX_BLOCK_SAMPLES]; MAX_TAPS],
}

impl BlockTaps {
    /// Empty backing storage, for allocation and initialization off the audio thread.
    pub const EMPTY: Self = Self {
        counts: [0; MAX_TAPS],
        samples: [[0.0; MAX_BLOCK_SAMPLES]; MAX_TAPS],
    };

    /// Audio thread. Clears metadata without copying the sample backing storage.
    pub fn clear(&mut self) {
        self.counts.fill(0);
    }

    /// Audio thread. Let a producer fill one stream. A malformed count or sample discards the stream.
    pub fn fill(&mut self, index: usize, read: impl FnOnce(&mut [f32]) -> usize) {
        if let Some(slot) = self.samples.get_mut(index) {
            let count = read(slot);
            self.counts[index] = if slot
                .get(..count)
                .is_some_and(|samples| samples.iter().all(|s| s.is_finite()))
            {
                count as u32
            } else {
                0
            };
        }
    }

    /// A validated stream, or an empty slice when nothing was written.
    pub fn samples(&self, index: usize) -> &[f32] {
        let Some((&count, samples)) = self.counts.get(index).zip(self.samples.get(index)) else {
            return &[];
        };
        samples.get(..count as usize).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_activity_preserves_all_hit_counter_bits_and_checks_frames() {
        let mut frame = [0.0; MAX_PAD_ACTIVITY_FRAME];
        PadActivity::write(&mut frame, u32::MAX, 1, [Some(0.25), None].into_iter());
        let activity = PadActivity::decode(&frame, 2).unwrap();
        assert_eq!(activity.hit, u32::MAX);
        assert_eq!(activity.last_pad, 1);
        assert_eq!(&activity.playheads[..2], &[Some(0.25), None]);
        assert!(PadActivity::decode(&frame, 1).is_none());
        assert!(PadActivity::decode(&frame[..frame.len() - 1], 2).is_none());
        frame[1] = 65_536.0;
        assert!(PadActivity::decode(&frame, 2).is_none());
        frame[1] = 0.0;
        frame[4] = -0.5;
        assert!(PadActivity::decode(&frame, 2).is_none());
        frame[4] = -1.0;
        frame[5] = 0.5;
        assert!(PadActivity::decode(&frame, 2).is_none());
    }

    #[test]
    fn malformed_inventories_and_readings_are_ignored() {
        let mut descriptor = Descriptor::EMPTY;
        descriptor.name[..4].copy_from_slice(b"gain");
        descriptor.kind = REDUCTION;
        descriptor.audio_frames_per_sample = 32;
        descriptor.capacity = 1024;
        assert!(valid_inventory(&[descriptor]));
        assert!(!valid_inventory(&[descriptor, descriptor]));
        descriptor.capacity = MAX_CAPACITY + 1;
        assert!(!valid_inventory(&[descriptor]));

        let mut block = BlockTaps::EMPTY;
        block.fill(0, |samples| {
            samples[..3].copy_from_slice(&[1.0, 2.0, 3.0]);
            3
        });
        block.fill(1, |samples| samples.len() + 1);
        block.fill(2, |samples| {
            samples[0] = f32::INFINITY;
            1
        });
        assert_eq!(block.samples(0), [1.0, 2.0, 3.0]);
        assert!(block.samples(1).is_empty() && block.samples(2).is_empty());
        block.clear();
        assert!(block.samples(0).is_empty());
    }
}
