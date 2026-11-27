use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use crate::SampleTap;

/// Levels of one device since the UI last read them (WP-89): the input peak, the largest
/// gain reduction and the output peak.
///
/// The device's processor owns an `Arc` of it and calls [`DeviceMeter::record`] once per block
/// (RT-safe: relaxed `fetch_max` on `f32` bit patterns, no allocation, no lock); the UI takes the
/// values every frame with [`DeviceMeter::take`]. Peaks are linear sample magnitudes
/// (1.0 = 0 dBFS), the reduction is in dB (≥ 0, 6.0 = the signal was turned down by 6 dB).
#[derive(Debug, Default)]
pub struct DeviceMeter {
    input: AtomicU32,
    reduction: AtomicU32,
    output: AtomicU32,
    /// The processor's current latency in frames (WP-91).
    latency: AtomicU32,
    /// Full scale of the gain-reduction bar in dB (f32 bits; 0 = [`DeviceMeter::DEFAULT_GR_RANGE_DB`]).
    gr_range: AtomicU32,
    /// The sample tap of a device that shows its signal (WP-92), set once when it is built.
    tap: OnceLock<Arc<SampleTap>>,
}

impl DeviceMeter {
    /// The gain-reduction range shown when the device sets none.
    pub const DEFAULT_GR_RANGE_DB: f32 = 24.0;

    /// All values 0.
    pub const fn new() -> DeviceMeter {
        DeviceMeter {
            input: AtomicU32::new(0),
            reduction: AtomicU32::new(0),
            output: AtomicU32::new(0),
            latency: AtomicU32::new(0),
            gr_range: AtomicU32::new(0),
            tap: OnceLock::new(),
        }
    }

    /// Attaches the device's sample tap (once, UI thread, when the processor is built; later calls
    /// are ignored).
    pub fn set_tap(&self, tap: Arc<SampleTap>) {
        let _ = self.tap.set(tap);
    }

    /// UI. The device's sample tap, if it has one (the EQ's output for the live spectrum).
    pub fn tap(&self) -> Option<&Arc<SampleTap>> {
        self.tap.get()
    }

    /// Sets the reduction (in dB) at which the gain-reduction bar is full; a limiter with typical
    /// reductions of a few dB wants a small range. Non-positive or non-finite values are ignored.
    pub fn set_gr_range_db(&self, db: f32) {
        if db.is_finite() && db > 0.0 {
            self.gr_range.store(db.to_bits(), Ordering::Relaxed);
        }
    }

    /// UI. The reduction at which the bar is full ([`DeviceMeter::DEFAULT_GR_RANGE_DB`] unless set).
    pub fn gr_range_db(&self) -> f32 {
        match self.gr_range.load(Ordering::Relaxed) {
            0 => Self::DEFAULT_GR_RANGE_DB,
            bits => f32::from_bits(bits),
        }
    }

    /// RT. Publishes the processor's current latency in frames. A processor whose latency depends
    /// on a parameter calls it after every change, so the host can recompile the delay
    /// compensation ([`DeviceMeter::latency`]).
    pub fn set_latency(&self, frames: u32) {
        self.latency.store(frames, Ordering::Relaxed);
    }

    /// UI. The latency last published with [`DeviceMeter::set_latency`] (0 before any).
    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    /// RT. Raises the stored values to at least `input`, `reduction_db` and `output` (all ≥ 0 and
    /// finite; anything else is ignored, so a NaN cannot poison the maximum).
    pub fn record(&self, input: f32, reduction_db: f32, output: f32) {
        for (slot, v) in [
            (&self.input, input),
            (&self.reduction, reduction_db),
            (&self.output, output),
        ] {
            if v.is_finite() && v > 0.0 {
                slot.fetch_max(v.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// UI. Returns `(input peak, reduction in dB, output peak)` since the last call and resets
    /// them to 0.
    pub fn take(&self) -> (f32, f32, f32) {
        (
            f32::from_bits(self.input.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.reduction.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.output.swap(0, Ordering::Relaxed)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_meter_keeps_the_maximum_until_taken_and_ignores_bad_values() {
        let m = DeviceMeter::new();
        m.record(0.25, 1.0, 0.5);
        m.record(0.75, f32::NAN, -1.0);
        m.record(0.5, 3.0, f32::INFINITY);
        assert_eq!(m.take(), (0.75, 3.0, 0.5));
        assert_eq!(m.take(), (0.0, 0.0, 0.0));
    }
}
