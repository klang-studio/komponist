//! A common display history for direct and bridged plugin signal streams.

use std::sync::atomic::{AtomicU32, Ordering};

use komponist_engine::SampleTap;
use komponist_ipc::signal_taps::{BlockTaps, Descriptor, valid_inventory};

/// A named plugin signal displayed by a native layout.
#[derive(Debug)]
pub struct SignalTap {
    descriptor: Descriptor,
    samples: SampleTap,
    audio_rate: AtomicU32,
}

impl SignalTap {
    /// The validated descriptor associated with these samples.
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// The sample rate and lock-free history readers can inspect.
    pub fn samples(&self) -> &SampleTap {
        &self.samples
    }

    /// Refresh the lease when the graph is visible.
    pub fn want(&self) {
        self.samples
            .want_at_audio_rate(f32::from_bits(self.audio_rate.load(Ordering::Relaxed)));
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
                    samples: SampleTap::new(descriptor.capacity as usize),
                    audio_rate: AtomicU32::new(0),
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

    pub(crate) fn record(&self, payload: &BlockTaps) {
        for (index, tap) in self.0.iter().enumerate() {
            let samples = payload.samples(index);
            if !samples.is_empty() {
                tap.samples.push(samples);
            }
        }
    }
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
            taps.record(&block);
            assert_eq!(taps.begin(24_000), 0b10);
            assert_eq!(taps.begin(64), 0);
        });
        assert_eq!(output.samples().written(), 0);
        assert_eq!(detector.samples().rate(), 1_500.0);
        let mut out = [0.0; 2];
        assert_eq!(detector.samples().read_latest(&mut out), Some(2));
        assert_eq!(out, [-3.0, -4.0]);
    }
}
