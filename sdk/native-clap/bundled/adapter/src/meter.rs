//! Gain reduction and optional detector data produced by the effect DSP.

use core::ffi::{CStr, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use komponist_engine::SampleTap;
use komponist_native_clap_sdk::effect_meter::{
    BlockMeter, DETECTOR_FRAMES, EXTENSION_ID, Extension,
};
use nice_plug::wrapper::clap::{Wrapper, clap_plugin};

use super::{Effect, EffectDefinition};

pub(super) struct Meter {
    enabled: AtomicBool,
    reduction: AtomicU32,
    tap: Option<Arc<SampleTap>>,
    detector_start: AtomicU64,
}

impl Meter {
    pub(super) fn new(tap: Option<Arc<SampleTap>>) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            reduction: AtomicU32::new(0),
            tap,
            detector_start: AtomicU64::new(u64::MAX),
        }
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

impl<T: EffectDefinition> Table<T> {
    const VALUE: Extension = Extension {
        begin: Some(begin::<T>),
        read: Some(read::<T>),
    };
}

pub(super) fn extension<T: EffectDefinition>(id: &CStr) -> *const c_void {
    if T::REPORTS_REDUCTION && id == EXTENSION_ID {
        (&Table::<T>::VALUE as *const Extension).cast()
    } else {
        core::ptr::null()
    }
}

unsafe extern "C" fn begin<T: EffectDefinition>(plugin: *const c_void, detector: u32) {
    // SAFETY: The extension is only queried from Effect<T> and its instance outlives the call.
    unsafe {
        Wrapper::<Effect<T>>::with_custom_extension_data::<Meter, _>(
            plugin.cast::<clap_plugin>(),
            |meter| meter.begin(detector != 0),
        )
    };
}

unsafe extern "C" fn read<T: EffectDefinition>(
    plugin: *const c_void,
    data: *mut BlockMeter,
) -> u32 {
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
