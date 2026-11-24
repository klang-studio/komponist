//! Signal feeds for the editors of verified packaged effects.

use std::sync::Arc;

use komponist_engine::{DeviceMeter, SampleTap, StereoIo};
use komponist_ipc::effect_meter::BlockMeter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Spectrum,
    Detector,
    Reduction,
}

#[derive(Debug)]
pub(crate) struct Meter {
    pub(crate) view: DeviceMeter,
    source: Source,
}

impl Meter {
    /// Compatibility meter for authenticated bundles predating the named-tap extension.
    /// Features are the standard CLAP descriptor tags, independent of the device's identity.
    pub(crate) fn new(features: &[String], rate: f32) -> Option<Arc<Self>> {
        let source = features.iter().find_map(|feature| match feature.as_str() {
            "equalizer" => Some(Source::Spectrum),
            "compressor" => Some(Source::Detector),
            "limiter" => Some(Source::Reduction),
            _ => None,
        })?;
        let view = DeviceMeter::new();
        let capacity = match source {
            Source::Spectrum => komponist_engine::tap::DEFAULT_CAPACITY,
            // Room for four seconds of detector pairs at 1500 Hz and the largest in-flight block.
            Source::Detector => 32_768,
            Source::Reduction => 0,
        };
        if capacity > 0 {
            view.set_tap(Arc::new(SampleTap::new(capacity)));
        } else {
            view.set_gr_range_db(12.0);
        }
        let meter = Arc::new(Self { view, source });
        meter.set_rate(rate);
        Some(meter)
    }

    pub(crate) fn set_rate(&self, rate: f32) {
        if let Some(tap) = self.view.tap() {
            tap.set_rate(rate);
        }
    }

    pub(crate) fn requests_reduction(&self) -> bool {
        self.source != Source::Spectrum
    }

    pub(crate) fn requests_detector(&self, frames: usize) -> bool {
        self.source == Source::Detector && self.view.tap().is_some_and(|tap| tap.begin(frames))
    }

    pub(crate) fn record(&self, io: &StereoIo<'_>, readings: Option<BlockMeter>) {
        if self.source == Source::Spectrum {
            if let Some(tap) = self.view.tap()
                && tap.begin(io.frames())
            {
                tap.push_mono(io.output[0], io.output[1]);
            }
            return;
        }
        let Some(readings) = readings else {
            return;
        };
        let peak = |left: &[f32], right: &[f32]| {
            left.iter()
                .chain(right)
                .fold(0.0_f32, |peak, x| peak.max(x.abs()))
        };
        self.view.record(
            peak(io.input[0], io.input[1]),
            readings.reduction_db,
            peak(io.output[0], io.output[1]),
        );
        if let Some(tap) = self.view.tap() {
            // Both host routes validate count and finiteness before exposing the readings.
            let samples = &readings.detector[..readings.detector_frames as usize];
            if !samples.is_empty() {
                tap.push(samples.as_flattened());
            }
        }
    }
}
