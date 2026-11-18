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
/// Low bits hold the minimum pre-burst depth. High bits identify the current output read interval.
const NO_BURST_DEPTH: u64 = u32::MAX as u64;
const BURST_EPOCH: u64 = NO_BURST_DEPTH + 1;
const CAPTURE_FORMATS: [cpal::SampleFormat; 3] = [
    cpal::SampleFormat::F32,
    cpal::SampleFormat::I32,
    cpal::SampleFormat::I16,
];

fn capture_dimensions_valid(channels: u16, sample_rate: u32) -> bool {
    (1..=MAX_INPUT_CHANNELS).contains(&usize::from(channels))
        && crate::backend::valid_sample_rate(sample_rate)
}

/// An enabled input device. Optional fields choose the host default; `None` for the whole
/// configuration disables capture. Old settings therefore never open a microphone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputConfig {
    /// Display name, used as a fallback when it identifies a unique compatible device.
    pub device: Option<String>,
    /// Stable backend identity.
    pub device_id: Option<DeviceId>,
}

impl InputConfig {
    /// Resolves the same stored selection used to open the capture stream.
    pub fn device_index(&self, devices: &[InputDeviceInfo]) -> Option<usize> {
        self.match_device(devices.iter())
    }

    fn match_device<'a>(
        &self,
        devices: impl IntoIterator<Item = &'a InputDeviceInfo>,
    ) -> Option<usize> {
        let mut candidates = devices
            .into_iter()
            .enumerate()
            .filter(|(_, device)| device.is_usable());
        if self.device.is_none() && self.device_id.is_none() {
            return candidates.find_map(|(index, device)| device.is_default.then_some(index));
        }
        let expected_role = self.device_id.as_ref().and_then(pipewire_input_role);
        let mut fallback = None;
        let mut ambiguous = false;
        for (index, device) in candidates {
            if self.device_id.is_some() && self.device_id == device.id {
                return Some(index);
            }
            if self.device.as_deref() == Some(device.name.as_str())
                && expected_role.is_none_or(|role| device.role() == Some(role))
            {
                ambiguous |= fallback.replace(index).is_some();
            }
        }
        // A saved name alone cannot distinguish devices with the same display label.
        fallback.filter(|_| !ambiguous)
    }
}

/// Capture roles identifiable from a PipeWire ALSA node's stable name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDeviceRole {
    /// Audio from a hardware input.
    Input,
    /// Audio playing through a hardware output (loopback).
    OutputMonitor,
}

fn pipewire_input_role(id: &DeviceId) -> Option<InputDeviceRole> {
    if id.0.starts_with("pipewire:alsa_input.") {
        Some(InputDeviceRole::Input)
    } else if id.0.starts_with("pipewire:alsa_output.") {
        Some(InputDeviceRole::OutputMonitor)
    } else {
        None
    }
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
    /// Preferred native capture rate. The bridge converts to the output clock.
    pub sample_rate: u32,
}

impl InputDeviceInfo {
    /// The identifiable PipeWire ALSA node role, if present.
    pub fn role(&self) -> Option<InputDeviceRole> {
        self.id.as_ref().and_then(pipewire_input_role)
    }

    fn is_usable(&self) -> bool {
        capture_dimensions_valid(self.channels, self.sample_rate)
    }
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

struct Shared {
    dropped: AtomicU64,
    lost: AtomicBool,
    burst_depth: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            dropped: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            burst_depth: AtomicU64::new(NO_BURST_DEPTH),
        }
    }
}

impl Shared {
    fn measure_read_epoch(&self) -> u64 {
        self.burst_depth.load(Ordering::Acquire) & !NO_BURST_DEPTH
    }

    fn publish_burst_depth(&self, epoch: u64, depth: usize) {
        // The low bit of the epoch is set while an output read can drain the queue.
        if epoch & BURST_EPOCH != 0 {
            return;
        }
        let _ = self
            .burst_depth
            .try_update(Ordering::Release, Ordering::Relaxed, |state| {
                (state & !NO_BURST_DEPTH == epoch)
                    .then_some(epoch | (state & NO_BURST_DEPTH).min(depth as u64))
            });
    }

    fn begin_read(&self) -> Option<usize> {
        let old = self
            .burst_depth
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                Some(state.wrapping_add(BURST_EPOCH) | NO_BURST_DEPTH)
            })
            .unwrap_or_else(|state| state);
        let depth = old & NO_BURST_DEPTH;
        (depth != NO_BURST_DEPTH).then_some(depth as usize)
    }

    fn finish_read(&self) {
        self.burst_depth.fetch_add(BURST_EPOCH, Ordering::Release);
    }
}

struct BurstMeasurement {
    epoch: u64,
    depth: usize,
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
        if samples.len() < self.channels {
            return true;
        }
        let measurement = self.measure_burst();
        self.push_measured(samples, at, measurement)
    }

    fn measure_burst(&self) -> BurstMeasurement {
        let epoch = self.shared.measure_read_epoch();
        BurstMeasurement {
            epoch,
            depth: self.tx.buffer().capacity() - self.tx.slots(),
        }
    }

    fn push_measured<T>(&mut self, samples: &[T], at: Instant, measured: BurstMeasurement) -> bool
    where
        T: cpal::Sample + Copy,
        f32: cpal::FromSample<T>,
    {
        let frames = samples.chunks_exact(self.channels);
        let mut complete = true;
        for (index, source) in frames.enumerate() {
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
        self.shared
            .publish_burst_depth(measured.epoch, measured.depth);
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
    lead: usize,
    burst: usize,
}

/// Creates a synthetic input bridge, without enumerating or connecting any hardware.
pub fn test_input(
    channels: u16,
    rate: u32,
    capacity: usize,
) -> Result<(TestAudioSource, InputReader), AudioError> {
    if !capture_dimensions_valid(channels, rate) || !(2..=MAX_QUEUE_FRAMES).contains(&capacity) {
        return Err(AudioError::UnsupportedConfig(
            "invalid capture dimensions".into(),
        ));
    }
    let channels = usize::from(channels);
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
            lead: 0,
            burst: 0,
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
        if self.lost() {
            return InputBuffer::new(out, self.channels, Duration::ZERO, true);
        }
        let lead = self.shared.begin_read();
        let input = self.read_queued(out, output_rate, now, lead);
        self.shared.finish_read();
        input
    }

    fn read_queued<'a>(
        &mut self,
        out: &'a mut [f32],
        output_rate: u32,
        now: Instant,
        lead: Option<usize>,
    ) -> InputBuffer<'a> {
        let dropped = self.shared.dropped.load(Ordering::Acquire);
        let overflow = dropped != self.dropped;
        let mut gap = overflow;
        self.dropped = dropped;
        // Only discard queued audio after confirmed loss to bring monitoring latency back down.
        if overflow {
            // A producer can fill the ring after the reserve measurement was taken.
            for _ in 0..self.rx.slots().saturating_sub(self.target) {
                let _ = self.rx.pop();
            }
            self.reset_interpolator();
        } else if let Some(lead) = lead {
            self.lead = lead;
            self.burst = self.burst.max(self.rx.slots().saturating_sub(lead));
        }
        // Start at the nominal rate once enough data is queued. During playback follow the
        // reserve before input arrived, excluding the sawtooth of input/output callback batching.
        // Leave room for the largest observed batch and for normal scheduling variation.
        let correction = if self.primed {
            let spare = self.rx.buffer().capacity().saturating_sub(self.burst);
            let reserve = self.target.min(spare / 2);
            ((self.lead as f64 - reserve as f64) / self.target as f64).clamp(-1.0, 1.0)
                * MAX_CLOCK_CORRECTION
        } else {
            if self.rx.slots() < self.target {
                return InputBuffer::new(out, self.channels, Duration::ZERO, true);
            }
            self.primed = true;
            0.0
        };
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
            // Hold the last real sample up to the sequence break without blending across it.
            // Keep the successor so the next integral advance can resume there.
            let right = if self.phase != 0.0 && right.sequence != left.sequence.wrapping_add(1) {
                gap = true;
                left
            } else {
                right
            };
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
        self.lead = 0;
        self.burst = 0;
    }
}

/// Capture-reader storage sized on the control thread for the output callback.
pub(crate) type CallbackInput = (InputReader, Vec<f32>);

pub(crate) fn callback_input(reader: InputReader, output_frames: u32) -> CallbackInput {
    let scratch = vec![0.0; output_frames as usize * reader.channels()];
    (reader, scratch)
}

/// Control side of a bounded capture-reader handoff.
pub(crate) struct CaptureTransfer {
    pending: rtrb::Producer<CallbackInput>,
    retired: rtrb::Consumer<CallbackInput>,
}

/// Callback side of a bounded capture-reader handoff. The control side frees retired readers.
pub(crate) struct CaptureUpdates {
    pending: rtrb::Consumer<CallbackInput>,
    retired: rtrb::Producer<CallbackInput>,
}

pub(crate) fn capture_transfer() -> (CaptureTransfer, CaptureUpdates) {
    let (send, receive) = rtrb::RingBuffer::new(1);
    let (retire, reclaim) = rtrb::RingBuffer::new(1);
    (
        CaptureTransfer {
            pending: send,
            retired: reclaim,
        },
        CaptureUpdates {
            pending: receive,
            retired: retire,
        },
    )
}

impl CaptureTransfer {
    pub(crate) fn ready(&mut self) -> bool {
        let _ = self.retired.pop();
        !self.pending.is_full()
    }

    pub(crate) fn install(&mut self, reader: InputReader, frames: u32) -> Result<(), AudioError> {
        self.pending
            .push(callback_input(reader, frames))
            .map_err(|_| AudioError::Backend("capture reader update is still pending".into()))
    }
}

impl CaptureUpdates {
    pub(crate) fn install_pending(&mut self, active: &mut Option<CallbackInput>) {
        if !self.retired.is_full()
            && let Ok(next) = self.pending.pop()
            && let Some(previous) = active.replace(next)
        {
            // There is a slot and only this callback produces retired readers.
            let retired = self.retired.push(previous);
            debug_assert!(retired.is_ok());
        }
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
    let entries = host
        .input_devices()
        .map_err(crate::backend::map_err)?
        .filter_map(|device| {
            let (mut info, _) = describe_input(&device)?;
            info.is_default = default.as_ref() == Some(&device);
            info.is_usable().then_some(info)
        })
        .collect();
    Ok(entries)
}

fn describe_input(device: &cpal::Device) -> Option<(InputDeviceInfo, cpal::SupportedStreamConfig)> {
    let config = default_capture_config(device).ok()?;
    let description = device.description().ok()?;
    let info = InputDeviceInfo {
        id: device.id().ok().map(|id| DeviceId(id.to_string())),
        name: description.name().to_owned(),
        channels: config.channels(),
        is_default: false,
        sample_rate: config.sample_rate(),
    };
    Some((info, config))
}

fn default_capture_config(
    device: &cpal::Device,
) -> Result<cpal::SupportedStreamConfig, AudioError> {
    let native = device
        .default_input_config()
        .map_err(crate::backend::map_err)?;
    if capture_dimensions_valid(native.channels(), native.sample_rate())
        && CAPTURE_FORMATS.contains(&native.sample_format())
    {
        return Ok(native);
    }
    let ranges = device
        .supported_input_configs()
        .map_err(crate::backend::map_err)?;
    fallback_capture_config(&native, ranges)
        .ok_or_else(|| AudioError::UnsupportedConfig("no supported capture format".into()))
}

fn fallback_capture_config(
    native: &cpal::SupportedStreamConfig,
    ranges: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
) -> Option<cpal::SupportedStreamConfig> {
    // Keep the native channel layout whenever possible. Choose it independently of the output
    // rate so an input's channel count is the same in the menu and in the capture bridge.
    let best = ranges
        .into_iter()
        .filter_map(|range| {
            let first = range.min_sample_rate().max(1);
            if first > range.max_sample_rate() || !capture_dimensions_valid(range.channels(), first)
            {
                return None;
            }
            let preference = CAPTURE_FORMATS
                .iter()
                .position(|format| range.sample_format() == *format)?;
            Some((range, first, preference))
        })
        .min_by_key(|(range, _, preference)| {
            (range.channels().abs_diff(native.channels()), *preference)
        })?;
    let (range, first, _) = best;
    let desired = if crate::backend::valid_sample_rate(native.sample_rate()) {
        native.sample_rate().clamp(first, range.max_sample_rate())
    } else {
        first
    };
    range.try_with_sample_rate(desired)
}

fn capture_at_output_rate(
    device: &cpal::Device,
    native: cpal::SupportedStreamConfig,
    output_rate: u32,
) -> Result<cpal::SupportedStreamConfig, AudioError> {
    let ranges: Vec<_> = device
        .supported_input_configs()
        .map_err(crate::backend::map_err)?
        .collect();
    Ok(CAPTURE_FORMATS
        .iter()
        .find_map(|format| {
            ranges
                .iter()
                .filter(|range| {
                    range.sample_format() == *format && range.channels() == native.channels()
                })
                .find_map(|range| range.try_with_sample_rate(output_rate))
        })
        .unwrap_or(native))
}

static ASIO_INPUTS: Mutex<Vec<InputDeviceInfo>> = Mutex::new(Vec::new());

/// Called while the output enumeration already has this ASIO driver loaded, so opening the
/// input chooser never enumerates other drivers while an ASIO stream is running.
pub(crate) fn remember_asio(device: &cpal::Device) {
    let Some((info, _)) = describe_input(device) else {
        return;
    };
    if !info.is_usable() {
        return;
    }
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
    device_id: Option<DeviceId>,
}

impl CaptureStream {
    pub(crate) fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }

    pub(crate) fn device_id(&self) -> Option<&DeviceId> {
        self.device_id.as_ref()
    }

    pub(crate) fn start(
        kind: HostKind,
        selection: &InputConfig,
        output_rate: u32,
        buffer: u32,
        duplex_device: Option<&cpal::Device>,
    ) -> Result<(Self, InputReader), AudioError> {
        let host = crate::backend::host(kind)?;
        let open_default = |device: cpal::Device| -> Result<_, AudioError> {
            let config = default_capture_config(&device)?;
            Ok((device, config))
        };
        let (device, native) = if kind == HostKind::Asio {
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
            open_default(device.clone())?
        } else if selection.device.is_none() && selection.device_id.is_none() {
            open_default(
                host.default_input_device()
                    .ok_or_else(|| AudioError::NoDevice("no default capture input".into()))?,
            )?
        } else {
            let devices: Vec<_> = host
                .input_devices()
                .map_err(crate::backend::map_err)?
                .filter_map(|device| {
                    let (info, native) = describe_input(&device)?;
                    Some((device, info, native))
                })
                .collect();
            selection
                .match_device(devices.iter().map(|(_, info, _)| info))
                .and_then(|index| devices.into_iter().nth(index))
                .map(|(device, _, native)| (device, native))
                .ok_or_else(|| AudioError::NoDevice("capture input unavailable".into()))?
        };
        let config = capture_at_output_rate(&device, native, output_rate)?;
        let device_id = device.id().ok().map(|id| DeviceId(id.to_string()));
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
                device_id,
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
    fn a_returning_input_replaces_capture_at_the_next_callback_without_rt_drops() {
        let (_, original) = test_input(1, 48_000, 16).unwrap();
        let (mut transfer, mut updates) = capture_transfer();
        let mut active = Some(callback_input(original, 4));
        let (mut source, restored) = test_input(2, 48_000, 16).unwrap();
        let at = Instant::now();
        assert!(source.push(&[0.25; 8], at));
        assert!(transfer.ready());
        transfer.install(restored, 4).unwrap();
        assert_no_alloc(|| {
            updates.install_pending(&mut active);
            let (reader, buffer) = active.as_mut().unwrap();
            let input = reader.read(buffer, 48_000, at);
            assert_eq!(input.channels(), 2);
            assert_eq!(input.data(), &[0.25; 8]);
        });
        assert_eq!(transfer.retired.slots(), 1);
        assert!(transfer.ready());
        assert_eq!(transfer.retired.slots(), 0);
    }

    #[test]
    fn a_read_between_burst_measurement_and_enqueue_invalidates_the_measurement() {
        let (mut source, mut reader) = test_input(1, 48_000, 16).unwrap();
        let at = Instant::now();
        assert!(source.push(&[0.25; 12], at));
        assert!(!reader.read(&mut [0.0; 4], 48_000, at).discontinuous());
        let delayed = source.measure_burst();
        assert_eq!(delayed.depth, 8);

        assert!(!reader.read(&mut [0.0; 8], 48_000, at).discontinuous());
        assert!(source.push_measured(&[0.25; 4], at, delayed));
        let mut out = [0.0; 4];
        let input = reader.read(&mut out, 48_000, at);
        assert!(!input.discontinuous());
        assert_eq!(input.data(), &[0.25; 4]);
        assert_eq!(reader.lead, 0);
        assert_eq!(source.dropped(), 0);

        assert!(source.push(&[0.25; 4], at));
        assert!(source.push(&[0.25; 4], at));
        let lead = reader.rx.slots() - 8;
        assert!(!reader.read(&mut out, 48_000, at).discontinuous());
        assert_eq!(reader.lead, lead);
    }

    #[test]
    fn equal_clock_batches_need_no_extra_preload() {
        for (input_frames, output_frames) in [(256, 64), (64, 256), (512, 64), (64, 512)] {
            let (mut source, mut reader) = test_input(1, 48_000, 512).unwrap();
            let at = Instant::now();
            let capture_at = |frame| at + crate::frames_to_duration(frame, 48_000);
            let samples = [0.25; 512];
            let mut out = [0.0; 512];
            let batch_frames = input_frames.max(output_frames);
            assert_no_alloc(|| {
                for batch in 0..2000 {
                    for i in (0..batch_frames).step_by(input_frames) {
                        assert!(source.push(
                            &samples[..input_frames],
                            capture_at((batch * batch_frames + i) as u64)
                        ));
                    }
                    for i in (0..batch_frames).step_by(output_frames) {
                        let input = reader.read(
                            &mut out[..output_frames],
                            48_000,
                            capture_at((batch * batch_frames + i + output_frames) as u64),
                        );
                        assert!(
                            !input.discontinuous(),
                            "{input_frames}→{output_frames}, batch {batch}, frame {i}"
                        );
                        assert!(input.data().iter().all(|&sample| sample == 0.25));
                    }
                }
            });
            assert_eq!(source.dropped(), 0);
        }
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
        assert_eq!(input.data(), &[0.0; 4]);
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
        assert!(!input.discontinuous());
        let input = reader.read(&mut out, 48_000, capture_at(26));
        assert!(input.discontinuous());
        assert_eq!(input.sample(0, 0), 17.0);
        assert!((input.sample(0, 1) - 23.0).abs() < 0.1);
        let input = reader.read(&mut out, 48_000, capture_at(28));
        assert!(!input.discontinuous());
        assert!((input.sample(0, 0) - 24.0).abs() < 0.1);
        assert_eq!(
            input.latency(),
            capture_at(28) - capture_at(22) - crate::frames_to_duration(1, 48_000)
        );
    }

    #[test]
    fn sequence_gap_never_blends_samples_even_across_output_blocks() {
        let (mut source, mut reader) = test_input(1, 48_000, 16).unwrap();
        let at = Instant::now();
        let mut first = [0.0; 5];
        let mut out = [0.0; 2];
        assert!(source.push(&[0.25; 2], at));
        source.sequence += 4;
        assert!(source.push(&[-0.75; 5], at));
        let input = reader.read(&mut first, 192_000, at);
        assert!(!input.discontinuous());
        assert_eq!(input.data(), &[0.25; 5]);
        for _ in 0..2 {
            let input = reader.read(&mut out, 192_000, at);
            assert!(input.discontinuous());
            assert_eq!(input.data(), &[0.25; 2]);
        }
        let input = reader.read(&mut out, 192_000, at);
        assert!(input.discontinuous());
        assert_eq!(input.data(), &[-0.75; 2]);
        let input = reader.read(&mut out, 192_000, at);
        assert!(!input.discontinuous());
        assert_eq!(input.data(), &[-0.75; 2]);
    }

    #[test]
    fn unsupported_default_format_can_use_a_compatible_alternate() {
        use cpal::{
            SampleFormat, SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange,
        };

        let native =
            SupportedStreamConfig::new(2, 48_000, SupportedBufferSize::Unknown, SampleFormat::U16);
        let config = |channels, min, max, format| {
            SupportedStreamConfigRange::new(
                channels,
                min,
                max,
                SupportedBufferSize::Unknown,
                format,
            )
        };
        let unsupported = config(2, 44_100, 48_000, SampleFormat::U16);
        let invalid = config(96, 44_100, 48_000, SampleFormat::F32);
        assert!(fallback_capture_config(&native, [unsupported, invalid]).is_none());

        let mono = config(1, 48_000, 48_000, SampleFormat::F32);
        let stereo = config(2, 44_100, 44_100, SampleFormat::I16);
        let fallback = fallback_capture_config(&native, [unsupported, mono, stereo]).unwrap();
        assert_eq!(fallback.channels(), 2);
        assert_eq!(fallback.sample_rate(), 44_100);
        assert_eq!(fallback.sample_format(), SampleFormat::I16);
    }

    #[test]
    fn only_usable_devices_contribute_to_name_ambiguity() {
        let device = |id: &str, channels, sample_rate| InputDeviceInfo {
            id: Some(DeviceId(id.into())),
            name: "Same label".into(),
            channels,
            is_default: false,
            sample_rate,
        };
        let legacy = InputConfig {
            device: Some("Same label".into()),
            device_id: None,
        };
        let available = [
            device("no-channels", 0, 48_000),
            device("usable", 2, 48_000),
            device("too-many-channels", 96, 48_000),
            device("invalid-rate", 2, 0),
        ];
        assert_eq!(legacy.device_index(&available), Some(1));
        let mut selected = legacy.clone();
        selected.device_id = available[1].id.clone();
        let two_usable = [device("other-usable", 2, 48_000), available[1].clone()];
        assert_eq!(legacy.device_index(&two_usable), None);
        assert_eq!(selected.device_index(&two_usable), Some(1));
    }

    #[test]
    fn clock_follower_keeps_capture_continuous_within_its_correction_range() {
        // One added or missing frame every sixteen 64-frame periods is about 0.1% drift.
        // One every four periods is about 0.4% and outruns the 0.2% follower.
        for (direction, every) in [(1, 16), (1, 4), (-1, 16), (-1, 4)] {
            let (mut source, mut reader) = test_input(1, 48_000, 256).unwrap();
            let at = Instant::now();
            let samples = [0.25; 65];
            let mut queued_until = 64;
            let mut out = [0.0; 64];
            assert!(source.push(&samples[..64], at));
            let mut discontinuous = false;
            for period in 1..=2_000 {
                let extra = if period % every == 0 { direction } else { 0 };
                let frames = (64 + extra) as usize;
                let complete = source.push(
                    &samples[..frames],
                    at + crate::frames_to_duration(queued_until, 48_000),
                );
                queued_until += frames as u64;
                let now = at + crate::frames_to_duration(queued_until, 48_000);
                let input = reader.read(&mut out, 48_000, now);
                discontinuous = input.discontinuous();
                if direction > 0 {
                    assert_eq!(discontinuous, !complete, "period {period}");
                }
                if discontinuous {
                    break;
                }
            }
            assert_eq!(discontinuous, every == 4, "drift direction {direction}");
            assert_eq!(source.dropped() > 0, direction > 0 && every == 4);
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
