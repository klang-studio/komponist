//! Timestamped audio capture, independent of the engine. Device and synthetic inputs share
//! the same bounded SPSC bridge. All allocation happens before either callback starts.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};

use crate::{AudioError, DeviceId, HostKind, StreamStats};

/// Maximum channels accepted from a capture device (bounds callback memory and work).
pub const MAX_INPUT_CHANNELS: usize = 64;
/// Maximum queue size; enough for four of the largest supported output buffers.
const MAX_QUEUE_FRAMES: usize = 65_536;
/// The clock follower corrects separate device clocks by at most 0.2 percent.
const MAX_CLOCK_CORRECTION: f64 = 0.002;
/// Marks an interval for which the producer has not published a complete burst.
const NO_BURST: u64 = u64::MAX;
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

/// Per-port timing for a synchronous capture backend.
#[derive(Debug, Clone, Copy)]
pub struct RoutedInput<'a> {
    /// Each slot's capture latency in output-rate frames.
    pub latencies: &'a [u32],
    /// Slots carrying valid input, without compacting missing slots.
    pub available: u64,
    /// Slots with a capture gap at the start of this buffer.
    pub discontinuous: u64,
    /// Interleaved previous samples retained for stereo leg alignment.
    pub history: &'a [f32],
    /// The next ring frame to write.
    pub history_cursor: usize,
    /// Continuous prior frames per slot, excluding frames before its latest discontinuity.
    pub history_valid: &'a [u32],
}

/// Borrowed, interleaved input at the output sample rate, for one processing callback.
#[derive(Debug, Clone, Copy)]
pub struct InputBuffer<'a> {
    data: &'a [f32],
    channels: usize,
    latency: Duration,
    discontinuous: bool,
    routes: Option<RoutedInput<'a>>,
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
            routes: None,
        }
    }
    /// No configured capture input.
    pub const fn empty() -> Self {
        Self {
            data: &[],
            channels: 0,
            latency: Duration::ZERO,
            discontinuous: false,
            routes: None,
        }
    }
    /// Attaches slot timing and availability without changing the interleaved layout.
    #[cfg(target_os = "linux")]
    pub(crate) fn with_routes(mut self, routes: RoutedInput<'a>) -> Self {
        self.routes = Some(routes);
        self
    }
    /// Slot-level timing on native duplex hosts.
    pub fn routes(&self) -> Option<RoutedInput<'a>> {
        self.routes
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
    /// Overruns the capture device reported (cpal `Xrun`). The producer turns each new one into a
    /// sequence break, so the reader marks a discontinuity where the lost frames were.
    xruns: AtomicU64,
    lost: AtomicBool,
    enabled: AtomicBool,
    burst_start: AtomicU64,
    burst_end: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            dropped: AtomicU64::new(0),
            xruns: AtomicU64::new(0),
            lost: AtomicBool::new(false),
            enabled: AtomicBool::new(false),
            burst_start: AtomicU64::new(NO_BURST),
            burst_end: AtomicU64::new(0),
        }
    }
}

impl Shared {
    fn publish_burst(&self, first_frame: u64, end_frame: u64) {
        self.burst_end.store(end_frame, Ordering::Relaxed);
        self.burst_start.fetch_min(first_frame, Ordering::Release);
    }

    fn take_burst(&self) -> Option<(u64, u64)> {
        let first_frame = self.burst_start.swap(NO_BURST, Ordering::Acquire);
        if first_frame == NO_BURST {
            return None;
        }
        // The marker publishes the end of its completed burst. If another burst completes
        // before this load, include it in the size estimate for this batch.
        Some((first_frame, self.burst_end.load(Ordering::Relaxed)))
    }
}

/// Producer used by the device callback and deterministic synthetic-input tests.
pub struct TestAudioSource {
    tx: rtrb::Producer<Frame>,
    channels: usize,
    rate: u32,
    sequence: u64,
    queued_frames: u64,
    /// The device xrun count already turned into a sequence break.
    seen_xruns: u64,
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
        // Frames the device lost in an overrun never reach the queue. Skip a sequence number so
        // the reader sees the break between the last frame before it and the first one after.
        let xruns = self.shared.xruns.load(Ordering::Acquire);
        if xruns != self.seen_xruns {
            self.seen_xruns = xruns;
            self.sequence = self.sequence.wrapping_add(1);
        }
        let first_frame = self.queued_frames;
        let complete = self.enqueue(samples, at);
        self.shared.publish_burst(first_frame, self.queued_frames);
        complete
    }

    fn enqueue<T>(&mut self, samples: &[T], at: Instant) -> bool
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
            } else {
                self.queued_frames = self.queued_frames.wrapping_add(1);
            }
        }
        complete
    }
    /// Total source frames that did not fit in the bounded queue.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Any thread. The device reported an overrun, as the capture stream's error callback does
    /// for cpal `Xrun`: the next pushed frame starts after a discontinuity.
    pub fn report_xrun(&self) {
        self.shared.xruns.fetch_add(1, Ordering::AcqRel);
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
    correction_frames: usize,
    dequeued_frames: u64,
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
            queued_frames: 0,
            seen_xruns: 0,
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
            correction_frames: 0,
            dequeued_frames: 0,
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
        let burst = self.shared.take_burst();
        let dropped = self.shared.dropped.load(Ordering::Acquire);
        let overflow = dropped != self.dropped;
        let mut gap = overflow;
        self.dropped = dropped;
        // Only discard queued audio after confirmed loss to bring monitoring latency back down.
        if overflow {
            // A producer can fill the ring after the reserve measurement was taken.
            for _ in 0..self.rx.slots().saturating_sub(self.target) {
                let _ = self.pop_frame();
            }
            self.reset_interpolator();
        } else if let Some((first_frame, end_frame)) = burst {
            let queued = self.rx.slots();
            // Compare completed pushes with completed pops, excluding dropped frames from
            // both counters. This cannot count reserve that was read while a burst arrived.
            self.lead = first_frame
                .saturating_sub(self.dequeued_frames)
                .min(queued as u64) as usize;
            // Use the completed burst extent even when some of it was read before publication.
            let completed = end_frame
                .saturating_sub(first_frame)
                .min(self.rx.buffer().capacity() as u64) as usize;
            self.burst = self
                .burst
                .max(completed.max(queued.saturating_sub(self.lead)));
            self.correction_frames = self.burst;
        }
        let rate_ratio = f64::from(self.rate) / f64::from(output_rate);
        let planned = ((out.len() / self.channels) as f64 * rate_ratio).ceil() as usize;
        let correction = if self.primed {
            let spare = self.rx.buffer().capacity().saturating_sub(self.burst);
            let reserve = self.target.min(spare / 2);
            // A measurement can cover several reads in the same input period. Stop correcting
            // once that period's worth of source frames is exhausted without a fresh burst.
            let weight = self.correction_frames.min(planned) as f64 / planned.max(1) as f64;
            ((self.lead as f64 - reserve as f64) / self.target as f64).clamp(-1.0, 1.0)
                * MAX_CLOCK_CORRECTION
                * weight
        } else {
            if self.rx.slots() < self.target {
                return InputBuffer::new(out, self.channels, Duration::ZERO, true);
            }
            self.primed = true;
            0.0
        };
        self.correction_frames = self.correction_frames.saturating_sub(planned);
        let step = rate_ratio * (1.0 + correction);
        let mut first_at = None;
        for frame in out.chunks_exact_mut(self.channels) {
            if self.left.is_none() {
                self.left = self.pop_frame();
            }
            // Advance the source only when the next output frame is needed. The last frame of
            // an output callback need not have its successor waiting in this callback.
            while self.phase >= 1.0
                && let Some(sequence) = self.left.as_ref().map(|frame| frame.sequence)
            {
                self.left = self.right.take().or_else(|| self.pop_frame());
                if let Some(next) = &self.left {
                    gap |= next.sequence != sequence.wrapping_add(1);
                }
                self.phase -= 1.0;
            }
            let right = if self.phase == 0.0 {
                self.left.as_ref()
            } else {
                if self.left.is_some() && self.right.is_none() {
                    self.right = self.pop_frame();
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

    fn pop_frame(&mut self) -> Option<Frame> {
        let next = self.rx.pop().ok()?;
        self.dequeued_frames = self.dequeued_frames.wrapping_add(1);
        Some(next)
    }

    fn reset_interpolator(&mut self) {
        self.left = None;
        self.right = None;
        self.phase = 0.0;
        self.primed = false;
        self.lead = 0;
        self.burst = 0;
        self.correction_frames = 0;
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
    capture_default(
        device
            .default_input_config()
            .map_err(crate::backend::map_err),
        || {
            device
                .supported_input_configs()
                .map_err(crate::backend::map_err)
        },
    )
}

fn capture_default<R>(
    native: Result<cpal::SupportedStreamConfig, AudioError>,
    supported_ranges: impl FnOnce() -> Result<R, AudioError>,
) -> Result<cpal::SupportedStreamConfig, AudioError>
where
    R: IntoIterator<Item = cpal::SupportedStreamConfigRange>,
{
    let (native, native_error) = match native {
        Ok(config)
            if capture_dimensions_valid(config.channels(), config.sample_rate())
                && CAPTURE_FORMATS.contains(&config.sample_format()) =>
        {
            return Ok(config);
        }
        Ok(config) => (Some(config), None),
        Err(error) => (None, Some(error)),
    };
    fallback_capture_config(native.as_ref(), supported_ranges()?).ok_or_else(|| {
        native_error
            .unwrap_or_else(|| AudioError::UnsupportedConfig("no supported capture format".into()))
    })
}

fn fallback_capture_config(
    native: Option<&cpal::SupportedStreamConfig>,
    ranges: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
) -> Option<cpal::SupportedStreamConfig> {
    // Keep the native channel layout whenever possible. Without a default, prefer the largest
    // usable layout and a rate near 48 kHz. The layout is independent of the output rate.
    let desired = native
        .map(cpal::SupportedStreamConfig::sample_rate)
        .filter(|&rate| crate::backend::valid_sample_rate(rate))
        .unwrap_or(48_000);
    ranges
        .into_iter()
        .filter_map(|range| {
            let first = range.min_sample_rate().max(1);
            if first > range.max_sample_rate() || !capture_dimensions_valid(range.channels(), first)
            {
                return None;
            }
            let format = CAPTURE_FORMATS
                .iter()
                .position(|format| range.sample_format() == *format)?;
            let rate = desired.clamp(first, range.max_sample_rate());
            let channels = range.channels();
            let fewer_channels = u16::MAX - channels;
            let layout_distance = native.map_or(0, |config| channels.abs_diff(config.channels()));
            let preference = (
                layout_distance,
                fewer_channels,
                format,
                desired.abs_diff(rate),
                rate,
            );
            Some((preference, range.try_with_sample_rate(rate)?))
        })
        .min_by_key(|(preference, _)| *preference)
        .map(|(_, config)| config)
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

/// Reopening an identified input pins its backend ID. The default routes deliberately follow the
/// backend, including default aliases that older settings files have stored as device IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureTarget {
    Exact(DeviceId),
    SystemDefault,
    BackendAlias(DeviceId),
}

impl CaptureTarget {
    fn is_backend_alias(id: &DeviceId) -> bool {
        matches!(
            id.0.as_str(),
            "pipewire:input_default"
                | "pipewire:sink_default"
                | "alsa:default"
                | "alsa:sysdefault"
                | "alsa:pipewire"
                | "alsa:pulse"
                | "jack:input"
        )
    }

    fn opened(kind: HostKind, selection: &InputConfig, id: Option<DeviceId>) -> Option<Self> {
        if selection.device.is_none()
            && selection.device_id.is_none()
            && kind != HostKind::Asio
            && id.as_ref().is_none_or(Self::is_backend_alias)
        {
            return Some(Self::SystemDefault);
        }
        id.map(|id| {
            if Self::is_backend_alias(&id) {
                Self::BackendAlias(id)
            } else {
                Self::Exact(id)
            }
        })
    }

    pub(crate) fn missing_at_start(selection: &InputConfig, error: &AudioError) -> Option<Self> {
        let id = selection.device_id.as_ref()?;
        (matches!(error, AudioError::NoDevice(_)) && !Self::is_backend_alias(id))
            .then(|| Self::Exact(id.clone()))
    }

    pub(crate) fn selection(&self) -> InputConfig {
        match self {
            Self::Exact(id) | Self::BackendAlias(id) => InputConfig {
                device: None,
                device_id: Some(id.clone()),
            },
            Self::SystemDefault => InputConfig::default(),
        }
    }
}

pub(crate) struct CaptureStream {
    stream: cpal::Stream,
    shared: Arc<Shared>,
    target: Option<CaptureTarget>,
}

impl CaptureStream {
    pub(crate) fn lost(&self) -> bool {
        self.shared.lost.load(Ordering::Acquire)
    }

    pub(crate) fn target(&self) -> Option<&CaptureTarget> {
        self.target.as_ref()
    }

    pub(crate) fn play(&self) -> Result<(), AudioError> {
        self.stream.play().map_err(crate::backend::map_err)
    }

    /// The device may run on its own thread before the control thread accepts a reconnect.
    /// Keep its callbacks from queueing audio until the reader belongs to the current output.
    pub(crate) fn enable(&self) {
        self.shared.enabled.store(true, Ordering::Release);
    }

    pub(crate) fn start(
        kind: HostKind,
        selection: &InputConfig,
        output_rate: u32,
        buffer: u32,
        duplex_device: Option<&cpal::Device>,
        stats: Arc<StreamStats>,
    ) -> Result<(Self, InputReader), AudioError> {
        let (capture, reader) =
            Self::prepare(kind, selection, output_rate, buffer, duplex_device, stats)?;
        capture.enable();
        capture.play()?;
        Ok((capture, reader))
    }

    pub(crate) fn prepare(
        kind: HostKind,
        selection: &InputConfig,
        output_rate: u32,
        buffer: u32,
        duplex_device: Option<&cpal::Device>,
        stats: Arc<StreamStats>,
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
        let callback = CaptureCallback { source };
        let errors = (shared.clone(), stats);
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, stream_config, callback, errors),
            cpal::SampleFormat::I32 => build::<i32>(&device, stream_config, callback, errors),
            cpal::SampleFormat::I16 => build::<i16>(&device, stream_config, callback, errors),
            _ => {
                return Err(AudioError::UnsupportedConfig(format!(
                    "capture sample format {format}"
                )));
            }
        }?;
        Ok((
            Self {
                stream,
                shared,
                target: CaptureTarget::opened(kind, selection, device_id),
            },
            reader,
        ))
    }
}

/// The capture device's data callback.
struct CaptureCallback {
    source: TestAudioSource,
}

impl CaptureCallback {
    /// RT. Queues one device buffer whose first frame is `age` old at callback entry. Runs inside
    /// `assert_no_alloc`, which reports allocations in debug builds of binaries that install its
    /// allocator.
    fn run<T>(&mut self, data: &[T], age: Duration)
    where
        T: cpal::Sample + Copy,
        f32: cpal::FromSample<T>,
    {
        assert_no_alloc::assert_no_alloc(|| {
            if !self.source.shared.enabled.load(Ordering::Acquire) {
                return;
            }
            let now = Instant::now();
            let at = now.checked_sub(age).unwrap_or(now);
            self.source.push_typed(data, at);
        });
    }
}

/// The capture stream's error callback. Overruns and refused real-time promotion may arrive on the
/// audio thread: atomics only. An overrun lost frames: it is counted and becomes a discontinuity
/// in the captured stream. Every other error ends the capture.
fn on_capture_error(shared: &Shared, stats: &StreamStats, error: &cpal::Error) {
    match error.kind() {
        cpal::ErrorKind::Xrun => {
            stats.record_input_xrun();
            shared.xruns.fetch_add(1, Ordering::AcqRel);
        }
        cpal::ErrorKind::RealtimeDenied => {}
        _ => shared.lost.store(true, Ordering::Release),
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut callback: CaptureCallback,
    (shared, stats): (Arc<Shared>, Arc<StreamStats>),
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
                callback.run(
                    data,
                    timestamp
                        .callback
                        .saturating_duration_since(timestamp.capture),
                );
            },
            move |error| on_capture_error(&shared, &stats, &error),
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
    /// A device overrun loses frames before they reach the queue. The take must not be
    /// concatenated across that gap (ARCHITECTURE §7), and the overrun is counted.
    #[test]
    fn an_input_xrun_marks_a_discontinuity_where_frames_were_lost() {
        let stats = StreamStats::new();
        let (mut source, mut reader) = test_input(1, 48_000, 64).unwrap();
        let at = Instant::now();
        let ramp: Vec<f32> = (0..32).map(|i| i as f32).collect();
        assert!(source.push(&ramp, at));
        let mut out = [0.0; 8];
        assert!(!reader.read(&mut out, 48_000, at).discontinuous());

        // The device callback runs again after the overrun with the frames that followed it.
        on_capture_error(
            &source.shared,
            &stats,
            &cpal::Error::new(cpal::ErrorKind::Xrun),
        );
        assert_eq!(stats.input_xruns(), 1);
        assert_eq!(stats.xruns(), 0, "the output did not run late");
        let later: Vec<f32> = (100..116).map(|i| i as f32).collect();
        assert!(source.push(&later, at));
        let mut breaks = Vec::new();
        let mut out = [0.0; 8];
        for _ in 0..4 {
            let input = assert_no_alloc(|| reader.read(&mut out, 48_000, at));
            breaks.push(input.discontinuous());
        }
        // Frames 8 .. 24 lie well before the overrun. The read that would interpolate from
        // frame 31 to frame 100 (the clock follower may look one frame ahead) reports the gap.
        assert_eq!(breaks[..2], [false, false]);
        assert!(breaks[2..].contains(&true), "{breaks:?}");
        assert!(out[7] >= 100.0, "{out:?}");
        assert!(!reader.lost());

        // Refused real-time scheduling is neither an overrun nor a loss; other errors end capture.
        on_capture_error(
            &source.shared,
            &stats,
            &cpal::Error::new(cpal::ErrorKind::RealtimeDenied),
        );
        assert_eq!(stats.input_xruns(), 1);
        assert!(!reader.lost());
        on_capture_error(
            &source.shared,
            &stats,
            &cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable),
        );
        assert!(reader.lost());
    }

    #[test]
    fn a_reported_xrun_breaks_the_sequence_only_once() {
        let (mut source, mut reader) = test_input(1, 48_000, 64).unwrap();
        let at = Instant::now();
        source.report_xrun();
        source.report_xrun();
        assert!(source.push(&[0.5; 16], at));
        assert!(source.push(&[0.5; 16], at));
        let mut out = [0.0; 32];
        // Both overruns happened before the first frame: nothing earlier to break from.
        assert!(!reader.read(&mut out, 48_000, at).discontinuous());
        assert_eq!(out, [0.5; 32]);
    }

    /// The device callback itself runs inside `assert_no_alloc` (as the output and JACK
    /// callbacks do), converts the device format and stays silent until enabled.
    #[test]
    fn the_capture_callback_is_rt_checked_and_waits_for_enable() {
        let (source, mut reader) = test_input(2, 48_000, 64).unwrap();
        let shared = Arc::clone(&source.shared);
        let mut callback = CaptureCallback { source };
        let device = [i16::MAX, i16::MIN].repeat(16);
        callback.run(&device, Duration::from_millis(2));
        assert_eq!(reader.rx.slots(), 0, "not enabled yet");
        shared.enabled.store(true, Ordering::Release);
        callback.run(&device, Duration::from_millis(2));
        assert_eq!(reader.rx.slots(), 16);
        let mut out = [0.0; 32];
        let input = reader.read(&mut out, 48_000, Instant::now());
        assert!(!input.discontinuous());
        assert!(input.latency() >= Duration::from_millis(2));
        assert!((input.sample(0, 0) - 1.0).abs() < 1e-3);
        assert_eq!(input.sample(1, 0), -1.0);
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
    fn a_delayed_burst_uses_the_reserve_remaining_at_the_consumer_cursor() {
        let (mut source, mut reader) = test_input(1, 48_000, 16).unwrap();
        let at = Instant::now();
        assert!(source.push(&[0.25; 12], at));
        assert!(!reader.read(&mut [0.0; 4], 48_000, at).discontinuous());
        let boundary = source.queued_frames;

        // The producer starts measuring while eight queued frames remain. The read drains them
        // before the producer has finished publishing its next burst.
        assert!(!reader.read(&mut [0.0; 8], 48_000, at).discontinuous());
        assert!(source.enqueue(&[0.25; 4], at));
        source.shared.publish_burst(boundary, source.queued_frames);
        let mut out = [0.0; 4];
        assert!(!reader.read(&mut out, 48_000, at).discontinuous());
        assert_eq!(reader.lead, 0);
        assert_eq!(out, [0.25; 4]);
    }

    #[test]
    fn sustained_overlap_keeps_capture_continuous_with_equal_and_nearby_clocks() {
        for (capacity, target, periods, drift) in [
            (4096, 128, 32_000, 0),
            (256, 64, 6_000, -1),
            (256, 64, 6_000, 1),
        ] {
            let (mut source, mut reader) = test_input(1, 48_000, capacity).unwrap();
            reader.target = target;
            let at = Instant::now();
            assert!(source.push(&[0.25; 128], at));
            let mut out = [0.0; 64];
            let samples = [0.25; 65];
            assert_no_alloc(|| {
                for period in 0..periods {
                    // Model a producer that starts before each read and publishes only once
                    // the consumer has already drained that output period.
                    let boundary = source.queued_frames;
                    let time = at + crate::frames_to_duration(period * 64, 48_000);
                    assert!(
                        !reader.read(&mut out, 48_000, time).discontinuous(),
                        "period {period}, drift direction {drift}"
                    );
                    assert_eq!(out, [0.25; 64]);
                    let frames = if period % 16 == 15 { 64 + drift } else { 64 };
                    assert!(source.enqueue(&samples[..frames as usize], time));
                    source.shared.publish_burst(boundary, source.queued_frames);
                }
            });
            assert_eq!(source.dropped(), 0);
        }
    }

    #[test]
    fn a_full_burst_includes_frames_read_before_its_marker() {
        let (mut source, mut reader) = test_input(1, 48_000, 4096).unwrap();
        reader.target = 128;
        let at = Instant::now();
        let mut out = [0.0; 64];
        let samples = [0.25; 4096];

        assert_no_alloc(|| {
            for _ in 0..4 {
                let boundary = source.queued_frames;
                assert!(source.enqueue(&samples, at));
                assert!(!reader.read(&mut out, 48_000, at).discontinuous());
                source.shared.publish_burst(boundary, source.queued_frames);
                for _ in 1..64 {
                    assert!(!reader.read(&mut out, 48_000, at).discontinuous());
                    assert_eq!(out, [0.25; 64]);
                }
                assert_eq!(reader.burst, 4096);
                assert_eq!(reader.rx.slots(), 0);
            }
        });
        assert_eq!(source.dropped(), 0);
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
        assert!(fallback_capture_config(Some(&native), [unsupported, invalid]).is_none());

        let mono = config(1, 48_000, 48_000, SampleFormat::F32);
        let stereo = config(2, 44_100, 44_100, SampleFormat::I16);
        let fallback = fallback_capture_config(Some(&native), [unsupported, mono, stereo]).unwrap();
        assert_eq!(fallback.channels(), 2);
        assert_eq!(fallback.sample_rate(), 44_100);
        assert_eq!(fallback.sample_format(), SampleFormat::I16);
    }

    #[test]
    fn failed_default_query_can_use_supported_ranges() {
        use cpal::{SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};
        let range = |channels, rate, sample_format| {
            SupportedStreamConfigRange::new(
                channels,
                rate,
                rate,
                SupportedBufferSize::Unknown,
                sample_format,
            )
        };
        let fallback = capture_default(
            Err(AudioError::UnsupportedConfig(
                "native format is not supported".into(),
            )),
            || {
                Ok([
                    range(2, 44_100, SampleFormat::F32),
                    range(2, 48_000, SampleFormat::F32),
                    range(1, 48_000, SampleFormat::F32),
                    range(96, 48_000, SampleFormat::F32),
                ])
            },
        )
        .unwrap();
        assert_eq!(fallback.channels(), 2);
        assert_eq!(fallback.sample_rate(), 48_000);
        assert_eq!(fallback.sample_format(), SampleFormat::F32);
    }

    #[test]
    fn reconnect_distinguishes_the_host_default_backend_aliases_and_device_ids() {
        let alias = DeviceId("pipewire:input_default".into());
        let exact = DeviceId("pipewire:alsa_input.usb-Hotone.analog-stereo".into());
        let config = |id| InputConfig {
            device: Some("Shared label".into()),
            device_id: id,
        };
        let missing = AudioError::NoDevice("gone".into());
        let disabled = AudioError::UnsupportedConfig("permanent failure".into());

        let target = CaptureTarget::opened(
            HostKind::PipeWire,
            &InputConfig::default(),
            Some(alias.clone()),
        );
        assert_eq!(target, Some(CaptureTarget::SystemDefault));
        assert_eq!(target.unwrap().selection(), InputConfig::default());
        assert_eq!(
            CaptureTarget::opened(
                HostKind::CoreAudio,
                &InputConfig::default(),
                Some(exact.clone())
            ),
            Some(CaptureTarget::Exact(exact.clone()))
        );
        assert_eq!(
            CaptureTarget::opened(
                HostKind::PipeWire,
                &config(Some(alias.clone())),
                Some(alias.clone())
            ),
            Some(CaptureTarget::BackendAlias(alias.clone()))
        );
        assert_eq!(
            CaptureTarget::missing_at_start(&config(Some(alias)), &missing),
            None
        );
        assert_eq!(
            CaptureTarget::missing_at_start(&config(None), &missing),
            None
        );
        assert_eq!(
            CaptureTarget::missing_at_start(&config(Some(exact.clone())), &disabled),
            None
        );
        let target =
            CaptureTarget::missing_at_start(&config(Some(exact.clone())), &missing).unwrap();
        assert_eq!(target, CaptureTarget::Exact(exact.clone()));
        assert_eq!(
            target.selection(),
            InputConfig {
                device: None,
                device_id: Some(exact)
            }
        );
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
