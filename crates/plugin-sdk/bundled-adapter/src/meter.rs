//! Gain reduction and optional detector data produced by the effect DSP.

use core::ffi::{CStr, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use komponist_engine::SampleTap;
use komponist_native_clap_sdk::effect_meter::{
    BlockMeter, DETECTOR_FRAMES, EXTENSION_ID, Extension,
};
use komponist_native_clap_sdk::signal_taps;
use nice_plug::wrapper::clap::{Wrapper, clap_plugin};

use super::{AudioDefinition, Effect};

pub(super) struct Meter {
    enabled: AtomicBool,
    reduction: AtomicU32,
    tap: Option<Arc<SampleTap>>,
    detector_start: AtomicU64,
    named_start: AtomicU64,
    named_mask: AtomicU32,
    named_frames: AtomicU32,
    named_reduction: AtomicU32,
    rate_bits: AtomicU32,
    reduction_phases: [AtomicU32; signal_taps::MAX_TAPS],
}

impl Meter {
    pub(super) fn new(tap: Option<Arc<SampleTap>>) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            reduction: AtomicU32::new(0),
            tap,
            detector_start: AtomicU64::new(u64::MAX),
            named_start: AtomicU64::new(u64::MAX),
            named_mask: AtomicU32::new(0),
            named_frames: AtomicU32::new(0),
            named_reduction: AtomicU32::new(0),
            rate_bits: AtomicU32::new(48_000.0_f32.to_bits()),
            reduction_phases: [const { AtomicU32::new(0) }; signal_taps::MAX_TAPS],
        }
    }

    pub(super) fn set_rate(&self, rate: f32) {
        self.rate_bits.store(rate.to_bits(), Ordering::Relaxed);
        for phase in &self.reduction_phases {
            phase.store(0, Ordering::Relaxed);
        }
    }

    fn begin_named(&self, mask: u32) {
        self.named_mask.store(mask, Ordering::Relaxed);
        self.named_reduction.store(0, Ordering::Relaxed);
        self.named_frames.store(0, Ordering::Relaxed);
        if let Some(tap) = &self.tap {
            self.named_start.store(tap.written(), Ordering::Relaxed);
            tap.want_frames(signal_taps::MAX_BLOCK_SAMPLES as u32);
        }
    }

    pub(super) fn record_named_reduction(&self, frames: u32, reduction: f32) {
        // A DSP tap already publishes its per-interval peaks. Without one, hold the block's
        // gain reduction across that block's sample intervals. A failed callback that never
        // reaches read is still bounded to the maximum number of audio frames.
        if self.tap.is_none()
            && self.named_mask.load(Ordering::Relaxed) != 0
            && self.named_frames.load(Ordering::Relaxed) < signal_taps::MAX_BLOCK_SAMPLES as u32
        {
            self.named_frames.fetch_add(frames, Ordering::Relaxed);
            if reduction.is_finite() && reduction > 0.0 {
                self.named_reduction
                    .fetch_max(reduction.to_bits(), Ordering::Relaxed);
            }
        }
    }

    fn read_named(&self, index: u32, descriptor: signal_taps::Descriptor, out: &mut [f32]) -> u32 {
        let bit = 1 << index;
        let pending = self.named_mask.fetch_and(!bit, Ordering::Relaxed);
        if pending & bit == 0 {
            return 0;
        }
        if let Some(tap) = &self.tap {
            if pending == bit {
                tap.want_frames(0);
            }
            let start = self.named_start.load(Ordering::Relaxed);
            let available = tap.written().saturating_sub(start) as usize;
            if descriptor.kind == signal_taps::SPECTRUM_SOURCE {
                let count = available.min(out.len()).min(tap.capacity() / 2);
                return if count > 0 && tap.read_latest(&mut out[..count]).is_some() {
                    count as u32
                } else {
                    0
                };
            }
            let mut pairs = [[0.0; 2]; DETECTOR_FRAMES];
            let count = (available / 2).min(pairs.len()).min(out.len());
            if count == 0 || tap.read_latest(pairs[..count].as_flattened_mut()).is_none() {
                return 0;
            }
            let channel = usize::from(descriptor.kind == signal_taps::REDUCTION);
            for (sample, pair) in out.iter_mut().zip(&pairs[..count]) {
                *sample = pair[channel];
            }
            return count as u32;
        }
        if descriptor.kind != signal_taps::REDUCTION {
            return 0;
        }
        let rate = f32::from_bits(self.rate_bits.load(Ordering::Relaxed));
        let interval = descriptor.frame_interval(rate).max(1);
        let phase = &self.reduction_phases[index as usize];
        let frames = u64::from(self.named_frames.load(Ordering::Relaxed))
            + u64::from(phase.load(Ordering::Relaxed));
        let interval = u64::from(interval);
        phase.store((frames % interval) as u32, Ordering::Relaxed);
        let count = ((frames / interval) as usize).min(out.len());
        out[..count].fill(f32::from_bits(self.named_reduction.load(Ordering::Relaxed)));
        count as u32
    }

    fn begin(&self, detector: bool) {
        self.reduction.store(0, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Relaxed);
        let start = self
            .tap
            .as_ref()
            .filter(|_| detector)
            .map_or(u64::MAX, |tap| {
                tap.want();
                tap.written()
            });
        self.detector_start.store(start, Ordering::Relaxed);
    }

    pub(super) fn record(&self, reduction: f32) {
        if self.enabled.load(Ordering::Relaxed) && reduction.is_finite() && reduction > 0.0 {
            self.reduction
                .fetch_max(reduction.to_bits(), Ordering::Relaxed);
        }
    }

    fn read(&self, out: &mut BlockMeter) {
        self.enabled.store(false, Ordering::Relaxed);
        out.reduction_db = f32::from_bits(self.reduction.load(Ordering::Relaxed));
        out.detector_frames = 0;
        let start = self.detector_start.swap(u64::MAX, Ordering::Relaxed);
        if let Some(tap) = &self.tap
            && start != u64::MAX
        {
            let frames = (tap.written().saturating_sub(start) / 2).min(DETECTOR_FRAMES as u64);
            if frames != 0 {
                let samples = &mut out.detector.as_flattened_mut()[..2 * frames as usize];
                if tap.read_latest(samples).is_some() {
                    out.detector_frames = frames as u32;
                }
            }
        }
    }
}

struct Table<T>(std::marker::PhantomData<T>);

impl<T: AudioDefinition> Table<T> {
    const VALUE: Extension = Extension {
        begin: Some(begin::<T>),
        read: Some(read::<T>),
    };
    const NAMED: signal_taps::Extension = signal_taps::Extension {
        count: Some(named_count::<T>),
        info: Some(named_info::<T>),
        begin: Some(named_begin::<T>),
        read: Some(named_read::<T>),
    };
}

pub(super) fn signal_tap_extension<T: AudioDefinition>(id: &CStr) -> *const c_void {
    if id == signal_taps::EXTENSION_ID && signal_taps::valid_inventory(T::SIGNAL_TAPS) {
        (&Table::<T>::NAMED as *const signal_taps::Extension).cast()
    } else {
        core::ptr::null()
    }
}

unsafe extern "C" fn named_count<T: AudioDefinition>(_: *const c_void) -> u32 {
    T::SIGNAL_TAPS.len() as u32
}

unsafe extern "C" fn named_info<T: AudioDefinition>(
    _: *const c_void,
    index: u32,
    out: *mut signal_taps::Descriptor,
) -> u32 {
    let Some(descriptor) = T::SIGNAL_TAPS
        .get(index as usize)
        .copied()
        .filter(|_| !out.is_null())
    else {
        return 0;
    };
    // SAFETY: A non-null host output points to one writable descriptor.
    unsafe { out.write(descriptor) };
    1
}

unsafe extern "C" fn named_begin<T: AudioDefinition>(plugin: *const c_void, mask: u32) {
    let mask = mask & ((1 << T::SIGNAL_TAPS.len()) - 1);
    if mask == 0 {
        return;
    }
    // SAFETY: This callback is invoked on the live originating Effect<T> audio instance.
    unsafe {
        Wrapper::<Effect<T>>::with_custom_extension_data::<Meter, _>(
            plugin.cast::<clap_plugin>(),
            |meter| meter.begin_named(mask),
        )
    };
}

unsafe extern "C" fn named_read<T: AudioDefinition>(
    plugin: *const c_void,
    index: u32,
    out: *mut f32,
    capacity: u32,
) -> u32 {
    let Some(&descriptor) = T::SIGNAL_TAPS.get(index as usize) else {
        return 0;
    };
    if out.is_null() || capacity == 0 {
        return 0;
    }
    let capacity = (capacity as usize).min(signal_taps::MAX_BLOCK_SAMPLES);
    // SAFETY: A live host supplies this instance's audio handle and unique output samples.
    unsafe {
        let out = core::slice::from_raw_parts_mut(out, capacity);
        Wrapper::<Effect<T>>::with_custom_extension_data::<Meter, _>(
            plugin.cast::<clap_plugin>(),
            |meter| meter.read_named(index, descriptor, out),
        )
    }
    .unwrap_or_default()
}

pub(super) fn extension<T: AudioDefinition>(id: &CStr) -> *const c_void {
    if T::REPORTS_REDUCTION && id == EXTENSION_ID {
        (&Table::<T>::VALUE as *const Extension).cast()
    } else {
        core::ptr::null()
    }
}

unsafe extern "C" fn begin<T: AudioDefinition>(plugin: *const c_void, detector: u32) {
    // SAFETY: The extension is only queried from Effect<T> and its instance outlives the call.
    unsafe {
        Wrapper::<Effect<T>>::with_custom_extension_data::<Meter, _>(
            plugin.cast::<clap_plugin>(),
            |meter| meter.begin(detector != 0),
        )
    };
}

unsafe extern "C" fn read<T: AudioDefinition>(plugin: *const c_void, data: *mut BlockMeter) -> u32 {
    // SAFETY: The host supplies a writable, unique pointer to one BlockMeter or null.
    let Some(data) = (unsafe { data.as_mut() }) else {
        return 0;
    };
    // SAFETY: The extension is only queried from Effect<T> and its instance outlives the call.
    u32::from(
        unsafe {
            Wrapper::<Effect<T>>::with_custom_extension_data::<Meter, _>(
                plugin.cast::<clap_plugin>(),
                |meter| meter.read(data),
            )
        }
        .is_some(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spectrum_request_consumes_only_the_current_block() {
        let tap = Arc::new(SampleTap::new(256));
        let meter = Meter::new(Some(Arc::clone(&tap)));
        let mut out = [0.0; 4];
        assert!(!tap.begin(64));
        tap.push(&[7.0; 8]);
        let spectrum =
            signal_taps::Descriptor::new("spectrum", signal_taps::SPECTRUM_SOURCE, 1, 256);
        meter.begin_named(1);
        assert!(tap.begin(8));
        tap.push(&[0.5, 0.25, 0.0, -0.25]);
        assert_eq!(meter.read_named(0, spectrum, &mut out), 4);
        assert_eq!(out, [0.5, 0.25, 0.0, -0.25]);
        assert!(!tap.begin(64));
        assert_eq!(meter.read_named(0, spectrum, &mut out), 0);
    }

    #[test]
    fn a_detector_pair_can_feed_two_selected_named_streams() {
        let tap = Arc::new(SampleTap::new(256));
        let meter = Meter::new(Some(Arc::clone(&tap)));
        let level = signal_taps::Descriptor::new("detector", signal_taps::LEVEL_HISTORY, 32, 256)
            .at_reference_rate(48_000);
        let gr = signal_taps::Descriptor::new("reduction", signal_taps::REDUCTION, 32, 256)
            .at_reference_rate(48_000);
        assert_eq!(level.sample_rate(48_000.0), level.sample_rate(96_000.0));
        meter.begin_named(0b11);
        tap.push(&[-4.0, 1.0, -2.0, 3.0]);
        let mut out = [0.0; 2];
        assert_eq!(meter.read_named(0, level, &mut out), 2);
        assert_eq!(out, [-4.0, -2.0]);
        assert_eq!(meter.read_named(1, gr, &mut out), 2);
        assert_eq!(out, [1.0, 3.0]);
        assert!(!tap.is_wanted());
    }

    #[test]
    fn a_reduction_stream_holds_the_block_peak_at_the_declared_cadence() {
        let meter = Meter::new(None);
        let gr = signal_taps::Descriptor::new("reduction", signal_taps::REDUCTION, 32, 256)
            .at_reference_rate(48_000);
        meter.set_rate(96_000.0);
        meter.begin_named(1);
        meter.record_named_reduction(80, 7.0);
        let mut out = [0.0; 2];
        assert_eq!(meter.read_named(0, gr, &mut out), 1);
        assert_eq!(out[0], 7.0);
        meter.begin_named(1);
        meter.record_named_reduction(48, 3.0);
        assert_eq!(meter.read_named(0, gr, &mut out), 1);
        assert_eq!(out[0], 3.0);
    }

    #[test]
    fn reduction_streams_keep_independent_phase() {
        let meter = Meter::new(None);
        let first = signal_taps::Descriptor::new("first", signal_taps::REDUCTION, 32, 256);
        let second = signal_taps::Descriptor::new("second", signal_taps::REDUCTION, 32, 256);
        let mut out = [0.0; 2];
        meter.begin_named(0b11);
        meter.record_named_reduction(48, 5.0);
        assert_eq!(meter.read_named(0, first, &mut out), 1);
        assert_eq!(meter.read_named(1, second, &mut out), 1);
        meter.begin_named(0b11);
        meter.record_named_reduction(48, 6.0);
        assert_eq!(meter.read_named(1, second, &mut out), 2);
        assert_eq!(out, [6.0, 6.0]);
        assert_eq!(meter.read_named(0, first, &mut out), 2);
    }

    #[test]
    fn one_interval_collects_its_deepest_reduction_and_only_its_detector_data() {
        let tap = Arc::new(SampleTap::new(256));
        let meter = Meter::new(Some(Arc::clone(&tap)));
        tap.push(&[-10.0, 10.0]);
        meter.begin(true);
        assert!(tap.is_wanted());
        meter.record(2.0);
        meter.record(7.0);
        meter.record(1.0);
        tap.push(&[-2.0, 7.0, -3.0, 1.0]);
        let mut data = BlockMeter::EMPTY;
        meter.read(&mut data);
        assert_eq!(data.reduction_db, 7.0);
        assert_eq!(data.detector_frames, 2);
        assert_eq!(data.detector[..2], [[-2.0, 7.0], [-3.0, 1.0]]);

        meter.begin(false);
        tap.push(&[-1.0, 1.0]);
        meter.read(&mut data);
        assert_eq!(data.reduction_db, 0.0);
        assert_eq!(data.detector_frames, 0);
    }
}
