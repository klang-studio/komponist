//! A common display history for direct and bridged plugin signal streams.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use komponist_ipc::signal_taps::{
    BlockTaps, Descriptor, MAX_PAD_ACTIVITY_FRAME, PAD_ACTIVITY, PadActivity, REDUCTION,
    valid_inventory,
};
use komponist_processor::{SampleTap, StereoIo};

/// A named plugin signal displayed by a native layout.
#[derive(Debug)]
pub struct SignalTap {
    descriptor: Descriptor,
    samples: Arc<SampleTap>,
    audio_rate: AtomicU32,
    // For a reduction meter, the main-bus input and output peaks from the same render path.
    audio_peaks: [AtomicU32; 2],
}

impl SignalTap {
    /// The validated descriptor associated with these samples.
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// The sample rate and lock-free history readers can inspect.
    pub fn samples(&self) -> &Arc<SampleTap> {
        &self.samples
    }

    /// Refresh the lease when the graph is visible.
    pub fn want(&self) {
        self.samples
            .want_at_audio_rate(f32::from_bits(self.audio_rate.load(Ordering::Relaxed)));
    }

    /// Take and reset this reduction meter's main-bus input and output peaks.
    pub fn take_audio_peaks(&self) -> [f32; 2] {
        self.audio_peaks
            .each_ref()
            .map(|peak| f32::from_bits(peak.swap(0, Ordering::Relaxed)))
    }

    /// Renew interest and read the most recent complete pad-activity frame.
    pub fn pad_activity(&self, count: usize) -> Option<(u64, PadActivity)> {
        if self.descriptor.kind != PAD_ACTIVITY {
            return None;
        }
        self.want();
        let mut frame = [0.0; MAX_PAD_ACTIVITY_FRAME];
        let cursor = self.samples.read_latest(&mut frame)?;
        Some((cursor, PadActivity::decode(&frame, count)?))
    }

    fn set_rate(&self, rate: f32) {
        self.audio_rate.store(rate.to_bits(), Ordering::Relaxed);
        self.samples.set_rate(self.descriptor.sample_rate(rate));
    }
}

#[derive(Debug)]
pub(crate) struct SignalTaps(Vec<SignalTap>);

impl SignalTaps {
    pub(crate) fn new(inventory: &[Descriptor], rate: f32) -> Option<Self> {
        if !valid_inventory(inventory) {
            return None;
        }
        let taps = Self(
            inventory
                .iter()
                .map(|&descriptor| SignalTap {
                    descriptor,
                    samples: Arc::new(SampleTap::new(descriptor.capacity as usize)),
                    audio_rate: AtomicU32::new(0),
                    audio_peaks: Default::default(),
                })
                .collect(),
        );
        taps.set_rate(rate);
        Some(taps)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&SignalTap> {
        self.0.iter().find(|tap| tap.descriptor.key() == Some(key))
    }

    pub(crate) fn set_rate(&self, rate: f32) {
        for tap in &self.0 {
            tap.set_rate(rate);
        }
    }

    /// A zero mask requires neither a plugin call nor a block-data copy.
    pub(crate) fn begin(&self, frames: usize) -> u32 {
        self.0.iter().enumerate().fold(0, |mask, (i, tap)| {
            mask | (u32::from(tap.samples.begin(frames)) << i)
        })
    }

    pub(crate) fn record(&self, payload: &BlockTaps, mask: u32, io: Option<&StereoIo<'_>>) {
        let mut peaks = None;
        for (index, tap) in self.0.iter().enumerate() {
            if mask & (1 << index) == 0 {
                continue;
            }
            if tap.descriptor.kind == REDUCTION
                && let Some(io) = io
            {
                let levels = peaks.get_or_insert_with(|| {
                    [
                        stereo_peak(io.input),
                        stereo_peak([io.output[0], io.output[1]]),
                    ]
                });
                for (peak, level) in tap.audio_peaks.iter().zip(levels) {
                    peak.fetch_max(level.to_bits(), Ordering::Relaxed);
                }
            }
            let samples = payload.samples(index);
            if !samples.is_empty()
                && (tap.descriptor.kind != PAD_ACTIVITY || samples.len() == MAX_PAD_ACTIVITY_FRAME)
            {
                tap.samples.push(samples);
            }
        }
    }
}

fn stereo_peak(channels: [&[f32]; 2]) -> f32 {
    channels
        .into_iter()
        .flatten()
        .map(|sample| sample.abs())
        .filter(|sample| sample.is_finite())
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;
    use komponist_ipc::signal_taps::{LEVEL_HISTORY, SPECTRUM_SOURCE};

    #[cfg(debug_assertions)]
    #[global_allocator]
    static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

    #[test]
    fn visibility_leases_select_only_one_named_stream_without_allocating() {
        let descriptor = |key: &[u8], kind, frames| {
            let mut d = Descriptor::EMPTY;
            d.name[..key.len()].copy_from_slice(key);
            d.kind = kind;
            d.capacity = 1_024;
            d.audio_frames_per_sample = frames;
            d
        };
        let taps = SignalTaps::new(
            &[
                descriptor(b"output", SPECTRUM_SOURCE, 1),
                descriptor(b"detector", LEVEL_HISTORY, 32),
            ],
            48_000.0,
        )
        .unwrap();
        let output = taps.get("output").unwrap();
        let detector = taps.get("detector").unwrap();
        let mut block = BlockTaps::EMPTY;
        assert_no_alloc(|| {
            assert_eq!(taps.begin(64), 0);
            detector.want();
            assert_eq!(taps.begin(64), 0b10);
            block.fill(1, |out| {
                out[..2].copy_from_slice(&[-3.0, -4.0]);
                2
            });
            taps.record(&block, 0b10, None);
            assert_eq!(taps.begin(24_000), 0b10);
            assert_eq!(taps.begin(64), 0);
        });
        assert_eq!(output.samples().written(), 0);
        assert_eq!(detector.samples().rate(), 1_500.0);
        let mut out = [0.0; 2];
        assert_eq!(detector.samples().read_latest(&mut out), Some(2));
        assert_eq!(out, [-3.0, -4.0]);
    }
    #[test]
    fn a_leased_reduction_tap_also_reports_the_main_bus_peaks() {
        let taps = SignalTaps::new(
            &[
                Descriptor::new("reduction", REDUCTION, 32, 256),
                Descriptor::new("other-reduction", REDUCTION, 32, 256),
            ],
            48_000.0,
        )
        .unwrap();
        let reduction = taps.get("reduction").unwrap();
        let other = taps.get("other-reduction").unwrap();
        let mut left = [0.25, f32::NAN];
        let mut right = [0.5, f32::INFINITY];
        let io = StereoIo {
            input: [&[-0.75, 0.125], &[1.0, 0.25]],
            output: [&mut left, &mut right],
        };
        let block = BlockTaps::EMPTY;
        assert_no_alloc(|| {
            taps.record(&block, taps.begin(2), Some(&io));
            assert_eq!(reduction.take_audio_peaks(), [0.0, 0.0]);
            reduction.want();
            taps.record(&block, taps.begin(2), Some(&io));
            assert_eq!(reduction.take_audio_peaks(), [1.0, 0.5]);
            assert_eq!(reduction.take_audio_peaks(), [0.0, 0.0]);
            assert_eq!(other.take_audio_peaks(), [0.0, 0.0]);
        });
    }
}
