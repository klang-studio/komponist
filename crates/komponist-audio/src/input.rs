//! Timestamped audio capture, independent of the engine. Device and synthetic inputs share
//! the same bounded SPSC bridge. All allocation happens before either callback starts.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};

use crate::{AudioError, DeviceId, HostKind};

/// Maximum channels accepted from a capture device (bounds callback memory and work).
pub const MAX_INPUT_CHANNELS: usize = 64;
/// Maximum queue size; enough for four of the largest supported output buffers.
const MAX_QUEUE_FRAMES: usize = 65_536;
/// The clock follower corrects separate device clocks by at most 0.2 percent.
const MAX_CLOCK_CORRECTION: f64 = 0.002;

/// An enabled input device. Optional fields choose the host default; `None` for the whole
/// configuration disables capture. Old settings therefore never open a microphone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {
    /// Display name, used if the saved stable id is unavailable.
    pub device: Option<String>,
    /// Stable backend identity.
    pub device_id: Option<DeviceId>,
}

/// Capture device metadata, enumerated without opening a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDeviceInfo {
    /// Stable device identity, if provided by the driver.
    pub id: Option<DeviceId>,
    /// Device name.
    pub name: String,
    /// Channels in the default input configuration.
    pub channels: u16,
    /// Whether the host selects this device by default.
    pub is_default: bool,
    /// Default native rate. Capture is converted to the output clock by the bridge.
    pub sample_rate: u32,
}

/// Borrowed, interleaved input at the output sample rate, for one processing callback.
#[derive(Debug, Clone, Copy)]
pub struct InputBuffer<'a> {
    data: &'a [f32],
    channels: usize,
    latency: Duration,
    discontinuous: bool,
}

impl<'a> InputBuffer<'a> {
    /// Wraps samples. Invalid dimensions expose an empty input instead of panicking on RT.
    pub fn new(data: &'a [f32], channels: usize, latency: Duration, discontinuous: bool) -> Self {
        if channels == 0 || channels > MAX_INPUT_CHANNELS || !data.len().is_multiple_of(channels) {
            return Self::empty();
        }
        Self {
            data,
            channels,
            latency,
            discontinuous,
        }
    }
    /// No configured capture input.
    pub const fn empty() -> Self {
        Self {
            data: &[],
            channels: 0,
            latency: Duration::ZERO,
            discontinuous: false,
        }
    }
    /// Number of channels; zero means disabled or unavailable.
    pub fn channels(&self) -> usize {
        self.channels
    }
    /// Number of input frames.
    pub fn frames(&self) -> usize {
        self.data.len().checked_div(self.channels).unwrap_or(0)
    }
    /// One sample, with silence for an unavailable channel/frame.
    pub fn sample(&self, channel: usize, frame: usize) -> f32 {
        if channel >= self.channels {
            return 0.0;
        }
        frame
            .checked_mul(self.channels)
            .and_then(|i| i.checked_add(channel))
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0.0)
    }
    /// Interleaved samples at the output sample rate.
    pub fn data(&self) -> &[f32] {
        self.data
    }
    /// Time from capture of the first sample to processing, including bridge buffering.
    pub fn latency(&self) -> Duration {
        self.latency
    }
    /// Frames were lost or unavailable. A recorder must fail explicitly rather than silently
    /// concatenating samples across this gap.
    pub fn discontinuous(&self) -> bool {
        self.discontinuous
    }
}

struct Frame {
    samples: [f32; MAX_INPUT_CHANNELS],
    at: Instant,
    sequence: u64,
}

#[derive(Default)]
struct Shared {
    dropped: AtomicU64,
    lost: AtomicBool,
}

/// Producer used by the device callback and deterministic synthetic-input tests.
pub struct TestAudioSource {
    tx: rtrb::Producer<Frame>,
    channels: usize,
    rate: u32,
    sequence: u64,
    shared: Arc<Shared>,
}

impl TestAudioSource {
    /// RT. Queues interleaved input whose first frame was captured at `at`. Returns false if
    /// any frame was dropped; the consumer also sees the discontinuity.
    pub fn push(&mut self, samples: &[f32], at: Instant) -> bool {
        self.push_typed(samples, at)
    }
    fn push_typed<T>(&mut self, samples: &[T], at: Instant) -> bool
    where
        T: cpal::Sample + Copy,
        f32: cpal::FromSample<T>,
    {
        let mut complete = true;
        for (index, source) in samples.chunks_exact(self.channels).enumerate() {
            let mut frame = Frame {
                samples: [0.0; MAX_INPUT_CHANNELS],
                at: at
                    .checked_add(crate::frames_to_duration(index as u64, self.rate))
                    .unwrap_or(at),
                sequence: self.sequence,
            };
            self.sequence = self.sequence.wrapping_add(1);
            for (out, &value) in frame.samples.iter_mut().zip(source) {
                let sample = <f32 as cpal::FromSample<T>>::from_sample_(value);
                *out = if sample.is_finite() { sample } else { 0.0 };
            }
            if self.tx.push(frame).is_err() {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                complete = false;
            }
        }
        complete
    }
    /// Total source frames that did not fit in the bounded queue.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

/// Consumer owned by the output callback. Linear interpolation converts the input rate;
/// a bounded queue-depth follower compensates the clocks of separate devices.
pub struct InputReader {
    rx: rtrb::Consumer<Frame>,
    channels: usize,
    rate: u32,
    left: Option<Frame>,
    right: Option<Frame>,
    phase: f64,
    shared: Arc<Shared>,
    dropped: u64,
    target: usize,
    primed: bool,
}

/// Creates a synthetic input bridge, without enumerating or connecting any hardware.
pub fn test_input(
    channels: u16,
    rate: u32,
    capacity: usize,
) -> Result<(TestAudioSource, InputReader), AudioError> {
    let channels = usize::from(channels);
    if channels == 0
        || channels > MAX_INPUT_CHANNELS
        || !crate::backend::valid_sample_rate(rate)
        || !(2..=MAX_QUEUE_FRAMES).contains(&capacity)
    {
        return Err(AudioError::UnsupportedConfig(
            "invalid capture dimensions".into(),
        ));
    }
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    let shared = Arc::new(Shared::default());
    Ok((
        TestAudioSource {
            tx,
            channels,
            rate,
            sequence: 0,
            shared: shared.clone(),
        },
        InputReader {
            rx,
            channels,
            rate,
            left: None,
            right: None,
            phase: 0.0,
            shared,
            dropped: 0,
            target: (capacity / 4).max(1),
            primed: false,
        },
    ))
}

impl InputReader {
    /// Input channels of this bridge.
    pub fn channels(&self) -> usize {
        self.channels
    }
    /// The capture device reported an unrecoverable stream error.
    pub fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }
    /// RT. Converts as many frames as fit into `out` to the output clock. `out` must contain
    /// complete interleaved frames. The returned view borrows only the caller's scratch space.
    pub fn read<'a>(
        &mut self,
        out: &'a mut [f32],
        output_rate: u32,
        now: Instant,
    ) -> InputBuffer<'a> {
        out.fill(0.0);
        if output_rate == 0 || !out.len().is_multiple_of(self.channels) {
            return InputBuffer::empty();
        }
        let dropped = self.shared.dropped.load(Ordering::Acquire);
        let overflow = dropped != self.dropped;
        let mut gap = overflow || self.lost();
        self.dropped = dropped;
        // A large input callback can legitimately deliver several output periods at once.
        // Only shorten the queue after confirmed loss to bring monitoring latency back down.
        if overflow {
            for _ in 0..self.rx.slots().saturating_sub(self.target) {
                let _ = self.rx.pop();
            }
            self.reset_interpolator();
        }
        // Build a small lead once instead of repeatedly consuming a partially filled device
        // callback and underflowing. Neither callback waits for the other.
        if !self.primed {
            if self.rx.slots() < self.target {
                return InputBuffer::new(out, self.channels, Duration::ZERO, true);
            }
            self.primed = true;
        }
        let depth = self.rx.slots() as f64;
        let correction = ((depth - self.target as f64) / self.target as f64).clamp(-1.0, 1.0)
            * MAX_CLOCK_CORRECTION;
        let step = f64::from(self.rate) / f64::from(output_rate) * (1.0 + correction);
        let mut first_at = None;
        for frame in out.chunks_exact_mut(self.channels) {
            if self.left.is_none() {
                self.left = self.rx.pop().ok();
            }
            // Advance the source only when the next output frame is needed. The last frame of
            // an output callback need not have its successor waiting in this callback.
            while self.phase >= 1.0
                && let Some(sequence) = self.left.as_ref().map(|frame| frame.sequence)
            {
                self.left = self.right.take().or_else(|| self.rx.pop().ok());
                if let Some(next) = &self.left {
                    gap |= next.sequence != sequence.wrapping_add(1);
                }
                self.phase -= 1.0;
            }
            let right = if self.phase == 0.0 {
                self.left.as_ref()
            } else {
                if self.left.is_some() && self.right.is_none() {
                    self.right = self.rx.pop().ok();
                }
                self.right.as_ref()
            };
            let (Some(left), Some(right)) = (self.left.as_ref(), right) else {
                gap = true;
                self.reset_interpolator();
                break;
            };
            gap |= self.phase != 0.0 && right.sequence != left.sequence.wrapping_add(1);
            first_at.get_or_insert(left.at);
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = left.samples[channel]
                    + (right.samples[channel] - left.samples[channel]) * self.phase as f32;
            }
            self.phase += step;
        }
        let latency = first_at.map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
        InputBuffer::new(out, self.channels, latency, gap)
    }

    fn reset_interpolator(&mut self) {
        self.left = None;
        self.right = None;
        self.phase = 0.0;
        self.primed = false;
    }
}

/// Lists input devices without starting capture. Null intentionally exposes no hardware.
pub fn input_devices(kind: HostKind) -> Result<Vec<InputDeviceInfo>, AudioError> {
    if kind == HostKind::Null {
        return Ok(Vec::new());
    }
    if kind == HostKind::Asio && crate::backend::asio_open() {
        return Ok(ASIO_INPUTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone());
    }
    let host = crate::backend::host(kind)?;
    let default = host.default_input_device();
    let mut entries = Vec::new();
    for device in host.input_devices().map_err(crate::backend::map_err)? {
        let Ok(config) = device.default_input_config() else {
            continue;
        };
        if usize::from(config.channels()) > MAX_INPUT_CHANNELS {
            continue;
        }
        let Ok(description) = device.description() else {
            continue;
        };
        entries.push(InputDeviceInfo {
            id: device.id().ok().map(|id| DeviceId(id.to_string())),
            name: description.name().to_owned(),
            channels: config.channels(),
            is_default: default.as_ref() == Some(&device),
            sample_rate: config.sample_rate(),
        });
    }
    Ok(entries)
}

static ASIO_INPUTS: Mutex<Vec<InputDeviceInfo>> = Mutex::new(Vec::new());

/// Called while the output enumeration already has this ASIO driver loaded, so opening the
/// input chooser never enumerates other drivers while an ASIO stream is running.
pub(crate) fn remember_asio(device: &cpal::Device) {
    let Ok(config) = device.default_input_config() else {
        return;
    };
    let Ok(description) = device.description() else {
        return;
    };
    let info = InputDeviceInfo {
        id: device.id().ok().map(|id| DeviceId(id.to_string())),
        name: description.name().to_owned(),
        channels: config.channels(),
        is_default: false,
        sample_rate: config.sample_rate(),
    };
    let mut cache = ASIO_INPUTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(old) = cache
        .iter_mut()
        .find(|old| old.id == info.id && old.name == info.name)
    {
        *old = info;
    } else {
        cache.push(info);
    }
}

pub(crate) struct CaptureStream {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
}

impl CaptureStream {
    pub(crate) fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }
    pub(crate) fn start(
        kind: HostKind,
        selection: &InputConfig,
        output_rate: u32,
        buffer: u32,
        duplex_device: Option<&cpal::Device>,
    ) -> Result<(Self, InputReader), AudioError> {
        let host = crate::backend::host(kind)?;
        let mut found = None;
        let devices = if kind == HostKind::Asio {
            // ASIO can load only one driver: capture must use the already selected output.
            let device =
                duplex_device.ok_or_else(|| AudioError::NoDevice("ASIO output missing".into()))?;
            let id_matches = selection
                .device_id
                .as_ref()
                .is_none_or(|id| device.id().is_ok_and(|own| own.to_string() == id.0));
            let name_matches = selection
                .device
                .as_ref()
                .is_none_or(|name| device.description().is_ok_and(|d| d.name() == name));
            if !id_matches || !name_matches {
                return Err(AudioError::UnsupportedConfig(
                    "ASIO input and output must use the same driver".into(),
                ));
            }
            found = Some(device.clone());
            Vec::new()
        } else {
            host.input_devices()
                .map_err(crate::backend::map_err)?
                .collect::<Vec<_>>()
        };
        for device in devices {
            let same_id = selection
                .device_id
                .as_ref()
                .is_some_and(|id| device.id().is_ok_and(|own| own.to_string() == id.0));
            let same_name = selection
                .device
                .as_ref()
                .is_some_and(|name| device.description().is_ok_and(|d| d.name() == name));
            if same_id || (found.is_none() && same_name) {
                found = Some(device);
                if same_id {
                    break;
                }
            }
        }
        let device = found
            .or_else(|| {
                (selection.device.is_none() && selection.device_id.is_none())
                    .then(|| host.default_input_device())
                    .flatten()
            })
            .ok_or_else(|| AudioError::NoDevice("capture input unavailable".into()))?;
        let native = device
            .default_input_config()
            .map_err(crate::backend::map_err)?;
        let formats = [
            cpal::SampleFormat::F32,
            cpal::SampleFormat::I32,
            cpal::SampleFormat::I16,
        ];
        let ranges: Vec<_> = device
            .supported_input_configs()
            .map_err(crate::backend::map_err)?
            .collect();
        let config = formats
            .iter()
            .find_map(|format| {
                ranges
                    .iter()
                    .find(|r| {
                        r.sample_format() == *format
                            && r.channels() == native.channels()
                            && r.min_sample_rate() <= output_rate
                            && r.max_sample_rate() >= output_rate
                    })
                    .map(|r| r.with_sample_rate(output_rate))
            })
            .unwrap_or(native);
        let rate = config.sample_rate();
        let channels = config.channels();
        let capacity = ((buffer as usize).saturating_mul(8)).clamp(4096, MAX_QUEUE_FRAMES);
        let (source, mut reader) = test_input(channels, rate, capacity)?;
        reader.target = ((buffer as usize).saturating_mul(2)).clamp(1, capacity / 2);
        let shared = reader.shared.clone();
        let format = config.sample_format();
        let stream_config = config.config();
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, stream_config, source, shared.clone()),
            cpal::SampleFormat::I32 => build::<i32>(&device, stream_config, source, shared.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, stream_config, source, shared.clone()),
            _ => {
                return Err(AudioError::UnsupportedConfig(format!(
                    "capture sample format {format}"
                )));
            }
        }?;
        stream.play().map_err(crate::backend::map_err)?;
        Ok((
            Self {
                _stream: stream,
                shared,
            },
            reader,
        ))
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut source: TestAudioSource,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, AudioError>
where
    T: cpal::SizedSample + Copy,
    f32: cpal::FromSample<T>,
{
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, info| {
                let timestamp = info.timestamp();
                let age = timestamp
                    .callback
                    .saturating_duration_since(timestamp.capture);
                let now = Instant::now();
                let at = now.checked_sub(age).unwrap_or(now);
                source.push_typed(data, at);
            },
            move |error| {
                if !matches!(
                    error.kind(),
                    cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::Xrun
                ) {
                    shared.lost.store(true, Ordering::Release);
                }
            },
            Some(Duration::from_secs(3)),
        )
        .map_err(crate::backend::map_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn synthetic_capture_is_interleaved_bounded_and_rt_safe() {
        let (mut source, mut reader) = test_input(2, 48_000, 32).unwrap();
        let at = Instant::now();
        let samples: Vec<_> = (0..16).flat_map(|i| [i as f32 / 16.0, -0.5]).collect();
        let mut out = [0.0; 16];
        assert_no_alloc(|| {
            assert!(source.push(&samples, at));
            let input = reader.read(&mut out, 48_000, at + Duration::from_millis(1));
            assert_eq!(input.channels(), 2);
            assert_eq!(input.frames(), 8);
            assert_eq!(input.sample(1, 0), -0.5);
            assert!(input.latency() >= Duration::from_millis(1));
            assert!(!input.discontinuous());
            assert_eq!(input.sample(99, usize::MAX), 0.0);
        });
    }
    #[test]
    fn null_stream_delivers_synthetic_capture_without_opening_devices() {
        use crate::{
            AudioProcess, AudioSystem, CallbackInfo, MidiIn, OutputBuffer, StreamConfig,
            StreamParams,
        };
        use std::sync::atomic::AtomicU32;
        struct Probe(Arc<AtomicU32>);
        impl AudioProcess for Probe {
            fn prepare(&mut self, _: &StreamParams) {}
            fn process(
                &mut self,
                out: &mut OutputBuffer<'_>,
                _: &CallbackInfo,
                _: &mut MidiIn<'_>,
            ) {
                out.silence();
            }
            fn process_input(
                &mut self,
                out: &mut OutputBuffer<'_>,
                _: &CallbackInfo,
                _: &mut MidiIn<'_>,
                input: &InputBuffer<'_>,
            ) {
                if input.sample(0, 0) > 0.0 {
                    self.0
                        .store(input.sample(0, 0).to_bits(), Ordering::Relaxed);
                }
                out.silence();
            }
        }
        let seen = Arc::new(AtomicU32::new(0));
        let (_, midi) = crate::midi::queue(4);
        let mut audio = AudioSystem::new(Box::new(Probe(seen.clone())), midi);
        audio
            .start(&StreamConfig {
                host: HostKind::Null,
                buffer_frames: 64,
                ..StreamConfig::default()
            })
            .unwrap();
        let mut source = audio.set_test_input(1, 48_000).unwrap();
        assert_eq!(audio.input_channels(), 1);
        assert!(source.push(&vec![0.75; 2048], Instant::now()));
        let deadline = Instant::now() + Duration::from_secs(1);
        while seen.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        audio.stop();
        assert_eq!(f32::from_bits(seen.load(Ordering::Relaxed)), 0.75);
        assert!(input_devices(HostKind::Null).unwrap().is_empty());
    }

    #[test]
    fn overflow_and_underflow_are_reported_and_never_concatenate_silently() {
        let (mut source, mut reader) = test_input(1, 48_000, 4).unwrap();
        assert!(!source.push(&[0.5; 10], Instant::now()));
        assert_eq!(source.dropped(), 6);
        let mut out = [1.0; 16];
        assert_no_alloc(|| {
            assert!(
                reader
                    .read(&mut out, 48_000, Instant::now())
                    .discontinuous()
            )
        });
        assert_eq!(out[15], 0.0);
    }
    #[test]
    fn larger_input_periods_do_not_interrupt_small_output_periods() {
        let (mut source, mut reader) = test_input(2, 48_000, 512).unwrap();
        let at = Instant::now();
        let samples: Vec<_> = (0..1280).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let capture_at = |frame| at + crate::frames_to_duration(frame, 48_000);
        let mut out = [0.0; 128];
        assert_no_alloc(|| {
            assert!(source.push(&samples[..256], at));
            assert!(
                !reader
                    .read(&mut out, 48_000, capture_at(128))
                    .discontinuous()
            );

            // An input callback delivers six output periods at once, without overflowing the
            // ring. Each of those periods must preserve the ordered samples already queued.
            for source_start in [128..512, 512..896, 896..1280] {
                assert!(source.push(
                    &samples[2 * source_start.start..2 * source_start.end],
                    capture_at(source_start.start as u64)
                ));
                for period in 0..6 {
                    let expected = source_start.start - 64 + period * 64;
                    let input = reader.read(
                        &mut out,
                        48_000,
                        capture_at((source_start.end + period * 64) as u64),
                    );
                    assert!(!input.discontinuous(), "at output frame {expected}");
                    assert!((input.sample(0, 0) - expected as f32).abs() < 3.0);
                    assert_eq!(input.sample(1, 0), -input.sample(0, 0));
                    assert!(
                        input
                            .data()
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|f| f[0])
                            .is_sorted()
                    );
                }
            }
            assert_eq!(source.dropped(), 0);
            assert!(!reader.lost());
        });
    }

    #[test]
    fn smaller_input_periods_accumulate_for_large_output_periods() {
        let (mut source, mut reader) = test_input(1, 48_000, 512).unwrap();
        let at = Instant::now();
        let capture_at = |frame| at + crate::frames_to_duration(frame, 48_000);
        let samples: Vec<f32> = (0..1280).map(|frame| frame as f32).collect();
        let mut out = [0.0; 384];
        assert!(source.push(&samples[..128], at));
        for output_frame in [0, 384, 768] {
            for start in (output_frame + 128..output_frame + 512).step_by(64) {
                assert!(source.push(&samples[start..start + 64], capture_at(start as u64)));
            }
            let input = reader.read(&mut out, 48_000, capture_at((output_frame + 512) as u64));
            assert!(!input.discontinuous());
            assert!((input.sample(0, 0) - output_frame as f32).abs() < 3.0);
            assert!(input.data().windows(2).all(|pair| pair[0] < pair[1]));
        }
        assert_eq!(source.dropped(), 0);
    }

    #[test]
    fn capture_primes_without_discarding_and_reports_a_real_underrun() {
        let (mut source, mut reader) = test_input(1, 48_000, 16).unwrap();
        let at = Instant::now();
        let capture_at = |frame| at + crate::frames_to_duration(frame, 48_000);
        let mut out = [0.0; 4];
        assert!(source.push(&[1.0, 2.0, 3.0], at));
        let input = reader.read(&mut out, 48_000, capture_at(3));
        assert!(input.discontinuous());
        assert_eq!(input.data(), &[0.0; 4]);

        assert!(source.push(&[4.0], capture_at(3)));
        let input = reader.read(&mut out, 48_000, capture_at(4));
        assert!(!input.discontinuous());
        assert_eq!(input.data(), &[1.0, 2.0, 3.0, 4.0]);

        // The empty queue matters only when the next callback actually needs samples.
        let input = reader.read(&mut out, 48_000, capture_at(8));
        assert!(input.discontinuous());
        assert_eq!(input.data(), &[0.0; 4]);

        assert!(source.push(&[9.0, 10.0, 11.0, 12.0], capture_at(8)));
        let input = reader.read(&mut out, 48_000, capture_at(12));
        assert!(!input.discontinuous());
        assert_eq!(input.data(), &[9.0, 10.0, 11.0, 12.0]);
        assert_eq!(input.latency(), capture_at(12) - capture_at(8));

        reader.shared.lost.store(true, Ordering::Release);
        assert!(source.push(&[13.0, 14.0, 15.0, 16.0], capture_at(12)));
        let input = reader.read(&mut out, 48_000, capture_at(16));
        assert!(input.discontinuous());
        assert_eq!(input.data(), &[13.0, 14.0, 15.0, 16.0]);
    }

    #[test]
    fn overflow_reduces_monitoring_backlog_and_detects_the_sequence_gap() {
        let (mut source, mut reader) = test_input(1, 48_000, 16).unwrap();
        let at = Instant::now();
        let capture_at = |frame| at + crate::frames_to_duration(frame, 48_000);
        let samples: Vec<f32> = (0..28).map(|frame| frame as f32).collect();
        let mut out = [0.0; 2];
        assert!(source.push(&samples[..4], at));
        assert!(!reader.read(&mut out, 48_000, capture_at(4)).discontinuous());

        // Two frames remain after the first read. Another eighteen fill the ring and
        // drop four frames. Monitoring resumes at the freshest retained four.
        assert!(!source.push(&samples[4..22], capture_at(4)));
        assert_eq!(source.dropped(), 4);
        let input = reader.read(&mut out, 48_000, capture_at(22));
        assert!(input.discontinuous());
        assert_eq!(input.data(), &[14.0, 15.0]);
        assert_eq!(input.latency(), capture_at(22) - capture_at(14));

        // Detect the dropped interval even if it arrives after the overflow flag was read.
        assert!(source.push(&samples[22..], capture_at(22)));
        let input = reader.read(&mut out, 48_000, capture_at(24));
        assert!(input.discontinuous());
        let input = reader.read(&mut out, 48_000, capture_at(26));
        assert!((input.sample(0, 0) - 22.0).abs() < 0.1);
        let input = reader.read(&mut out, 48_000, capture_at(28));
        assert!(!input.discontinuous());
        assert!((input.sample(0, 0) - 24.0).abs() < 0.1);
        assert_eq!(
            input.latency(),
            capture_at(28) - capture_at(22) - crate::frames_to_duration(2, 48_000)
        );
    }

    #[test]
    fn clock_follower_keeps_capture_continuous_within_its_correction_range() {
        // One extra frame every sixteen 64-frame periods is about 0.1% drift.
        // One every four periods is about 0.4% and outruns the 0.2% follower.
        for (extra_every, expect_overflow) in [(16, false), (4, true)] {
            let (mut source, mut reader) = test_input(1, 48_000, 256).unwrap();
            let at = Instant::now();
            let samples = [0.25; 65];
            let mut queued_until = 64;
            let mut out = [0.0; 64];
            assert!(source.push(&samples[..64], at));
            for period in 1..=2_000 {
                let frames = 64 + usize::from(period % extra_every == 0);
                let complete = source.push(
                    &samples[..frames],
                    at + crate::frames_to_duration(queued_until, 48_000),
                );
                queued_until += frames as u64;
                let now = at + crate::frames_to_duration(queued_until, 48_000);
                let input = reader.read(&mut out, 48_000, now);
                assert_eq!(input.discontinuous(), !complete, "period {period}");
                if !complete {
                    break;
                }
            }
            assert_eq!(source.dropped() > 0, expect_overflow);
        }
    }

    #[test]
    fn another_input_rate_is_converted_without_allocation() {
        let (mut source, mut reader) = test_input(1, 24_000, 64).unwrap();
        let samples: Vec<f32> = (0..32).map(|i| i as f32).collect();
        assert!(source.push(&samples, Instant::now()));
        let mut out = [0.0; 32];
        assert_no_alloc(|| {
            reader.read(&mut out, 48_000, Instant::now());
        });
        assert!((out[16] - 8.0).abs() < 0.1);
        assert!(out.windows(2).all(|v| v[1] > v[0]));
    }
}
