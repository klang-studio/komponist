//! Stream backends: cpal device streams, the null backend (a timer thread), and the callback both
//! drive.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{BufferSize, SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

use crate::process::{AudioProcess, CallbackInfo, OutputBuffer, StreamParams};
use crate::slot::EngineSlot;
use crate::system::{AudioError, DeviceId, DeviceInfo, HostKind, StreamConfig, StreamStats};

/// Sample rates offered in S07.
const STANDARD_RATES: [u32; 5] = [44_100, 48_000, 88_200, 96_000, 192_000];

/// Device sample formats the callback converts to, in order of preference.
const FORMATS: [SampleFormat; 3] = [SampleFormat::F32, SampleFormat::I32, SampleFormat::I16];

/// Largest accepted buffer size in frames.
const MAX_BUFFER_FRAMES: u32 = 16_384;

/// Largest accepted sample rate in Hz.
const MAX_SAMPLE_RATE: u32 = 768_000;

/// How long cpal may take to open a stream.
const BUILD_TIMEOUT: Duration = Duration::from_secs(3);

/// The null backend resynchronises instead of catching up when it falls this many periods behind.
const NULL_MAX_BEHIND: u32 = 4;

/// Name of the device the null host lists.
const NULL_DEVICE: &str = "null";

/// Whether a backend can run at `sample_rate` Hz.
pub(crate) fn valid_sample_rate(sample_rate: u32) -> bool {
    (1..=MAX_SAMPLE_RATE).contains(&sample_rate)
}

/// Whether a backend can run with buffers of `frames` frames.
pub(crate) fn valid_buffer_frames(frames: u32) -> bool {
    (1..=MAX_BUFFER_FRAMES).contains(&frames)
}

/// Rejects configurations no backend can run.
pub(crate) fn validate(config: &StreamConfig) -> Result<(), AudioError> {
    if !valid_sample_rate(config.sample_rate) {
        return Err(AudioError::UnsupportedConfig(format!(
            "sample rate {} Hz",
            config.sample_rate
        )));
    }
    if !valid_buffer_frames(config.buffer_frames) {
        return Err(AudioError::UnsupportedConfig(format!(
            "buffer size {} frames",
            config.buffer_frames
        )));
    }
    Ok(())
}

/// What a stream shares with the UI thread.
#[derive(Debug, Default)]
pub(crate) struct StreamShared {
    /// The backend reported the device gone or the stream invalid.
    lost: AtomicBool,
    /// The backend's message for `lost`.
    reason: Mutex<Option<String>>,
    /// Output latency of the last callback.
    latency_nanos: AtomicU64,
}

impl StreamShared {
    /// Error callback: the stream is unusable.
    fn mark_lost(&self, reason: String) {
        // Never block in the error callback (it may run on the audio thread).
        if let Ok(mut slot) = self.reason.try_lock() {
            slot.get_or_insert(reason);
        }
        self.lost.store(true, Ordering::Release);
    }

    /// The reason, once the stream is lost.
    pub(crate) fn lost(&self) -> Option<String> {
        if !self.lost.load(Ordering::Acquire) {
            return None;
        }
        let reason = self.reason.lock().unwrap_or_else(PoisonError::into_inner);
        Some(
            reason
                .clone()
                .unwrap_or_else(|| "audio device lost".to_owned()),
        )
    }

    /// Output latency reported by the latest callback.
    pub(crate) fn latency(&self) -> Duration {
        Duration::from_nanos(self.latency_nanos.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) fn simulate_loss(&self, reason: &str) {
        self.mark_lost(reason.to_owned());
    }
}

/// The stream callback: cuts the device buffer into pieces of at most the prepared size, runs the
/// slot on an `f32` scratch buffer and converts into the device format.
pub(crate) struct StreamCallback<P: AudioProcess> {
    slot: EngineSlot<P>,
    params: StreamParams,
    /// Interleaved, `max_frames × channels`.
    scratch: Vec<f32>,
    shared: Arc<StreamShared>,
}

impl<P: AudioProcess> StreamCallback<P> {
    /// Non-RT.
    pub(crate) fn new(
        slot: EngineSlot<P>,
        params: StreamParams,
        shared: Arc<StreamShared>,
    ) -> StreamCallback<P> {
        let scratch = vec![0.0; params.max_frames as usize * usize::from(params.channels)];
        StreamCallback {
            slot,
            params,
            scratch,
            shared,
        }
    }

    /// RT. Fills `data` (interleaved, `params.channels` channels). Runs inside `assert_no_alloc`,
    /// which reports allocations in debug builds of binaries that install its allocator.
    pub(crate) fn run<T>(&mut self, data: &mut [T], output_latency: Duration)
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        assert_no_alloc::assert_no_alloc(|| self.render(data, output_latency));
    }

    fn render<T>(&mut self, data: &mut [T], output_latency: Duration)
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let now = Instant::now();
        let StreamParams {
            sample_rate,
            max_frames,
            channels,
            ..
        } = self.params;
        let channels = usize::from(channels);
        self.shared
            .latency_nanos
            .store(output_latency.as_nanos() as u64, Ordering::Relaxed);

        let total = data.len() / channels;
        let mut done = 0;
        for chunk in data.chunks_mut(max_frames as usize * channels) {
            let frames = chunk.len() / channels;
            let offset = done;
            done += frames;
            // The callback covers the period before `now`; each piece covers its share of it.
            let remaining = crate::frames_to_duration((total - done) as u64, sample_rate);
            let info = CallbackInfo {
                sample_rate,
                frames: frames as u32,
                now: now.checked_sub(remaining).unwrap_or(now),
                output_latency: output_latency
                    + crate::frames_to_duration(offset as u64, sample_rate),
            };
            let scratch = &mut self.scratch[..chunk.len()];
            self.slot
                .process(&mut OutputBuffer::new(scratch, channels), &info);
            for (out, &sample) in chunk.iter_mut().zip(scratch.iter()) {
                *out = T::from_sample(sample);
            }
        }
    }
}

/// The null backend: a thread calls the callback once per buffer period and discards the output.
pub(crate) struct NullStream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl NullStream {
    /// Starts the timer thread.
    pub(crate) fn start<P: AudioProcess>(
        mut callback: StreamCallback<P>,
    ) -> Result<NullStream, AudioError> {
        let StreamParams {
            sample_rate,
            max_frames,
            channels,
            ..
        } = callback.params;
        let period = crate::frames_to_duration(u64::from(max_frames), sample_rate);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("komponist-null-audio".to_owned())
            .spawn(move || {
                let mut buffer = vec![0.0f32; max_frames as usize * usize::from(channels)];
                let mut next = Instant::now();
                while !stop_thread.load(Ordering::Acquire) {
                    let now = Instant::now();
                    if now < next {
                        thread::park_timeout(next - now);
                        continue;
                    }
                    callback.run(&mut buffer, Duration::ZERO);
                    next += period;
                    let after = Instant::now();
                    if after > next + period * NULL_MAX_BEHIND {
                        next = after;
                    }
                }
                // `callback` drops here, on this thread: the slot hands the engine back.
            })
            .map_err(|e| AudioError::Backend(format!("null audio thread: {e}")))?;
        Ok(NullStream {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for NullStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// cpal's id of a host; `None` for the null host and for hosts this system does not have (cpal
/// only has the PipeWire, JACK and ALSA ids on Linux, and CoreAudio on macOS).
fn host_id(kind: HostKind) -> Option<cpal::HostId> {
    match kind {
        #[cfg(target_os = "macos")]
        HostKind::CoreAudio => Some(cpal::HostId::CoreAudio),
        #[cfg(target_os = "linux")]
        HostKind::PipeWire => Some(cpal::HostId::PipeWire),
        #[cfg(target_os = "linux")]
        HostKind::Jack => Some(cpal::HostId::Jack),
        #[cfg(target_os = "linux")]
        HostKind::Alsa => Some(cpal::HostId::Alsa),
        _ => None,
    }
}

/// Opens a cpal host.
pub(crate) fn host(kind: HostKind) -> Result<cpal::Host, AudioError> {
    let id = host_id(kind).ok_or(AudioError::HostUnavailable(kind))?;
    if !cpal::available_hosts().contains(&id) {
        return Err(AudioError::HostUnavailable(kind));
    }
    cpal::host_from_id(id).map_err(|_| AudioError::HostUnavailable(kind))
}

/// Hosts available on this machine, in preference order, `Null` last.
pub(crate) fn available_hosts() -> Vec<HostKind> {
    let mut hosts: Vec<HostKind> = [
        HostKind::CoreAudio,
        HostKind::PipeWire,
        HostKind::Jack,
        HostKind::Alsa,
    ]
    .into_iter()
    .filter(|&kind| match host(kind) {
        // cpal always lists JACK; it is usable only when a server answers.
        Ok(host) if kind == HostKind::Jack => host.default_output_device().is_some(),
        Ok(_) => true,
        Err(_) => false,
    })
    .collect();
    hosts.push(HostKind::Null);
    hosts
}

/// Maps a cpal error onto the contract's error type.
pub(crate) fn map_err(err: cpal::Error) -> AudioError {
    use cpal::ErrorKind as K;
    let message = err.to_string();
    match err.kind() {
        K::DeviceNotAvailable | K::DeviceBusy | K::PermissionDenied => {
            AudioError::NoDevice(message)
        }
        K::UnsupportedConfig | K::InvalidInput | K::UnsupportedOperation => {
            AudioError::UnsupportedConfig(message)
        }
        _ => AudioError::Backend(message),
    }
}

/// The output devices of `host` with their names. cpal's JACK host also lists its input device
/// as an output; it is skipped. Names repeat on ALSA (`hw` and `plughw` of one card), so a repeated
/// name gets " (2)", " (3)", … in enumeration order.
fn output_devices(host: &cpal::Host) -> Result<Vec<(String, cpal::Device)>, AudioError> {
    let mut list: Vec<(String, cpal::Device)> = Vec::new();
    for device in host.output_devices().map_err(map_err)? {
        let Ok(description) = device.description() else {
            continue;
        };
        if description.direction() == cpal::DeviceDirection::Input {
            continue;
        }
        let base = description.name().to_owned();
        let mut name = base.clone();
        let mut n = 1;
        while list.iter().any(|(seen, _)| *seen == name) {
            n += 1;
            name = format!("{base} ({n})");
        }
        list.push((name, device));
    }
    Ok(list)
}

/// The device of `config`: by `device_id` if `host` lists it, else by name (as [`devices`] lists
/// it), else, when both are `None`, the host's default. An id of another host never matches.
pub(crate) fn find_device(
    host: &cpal::Host,
    config: &StreamConfig,
) -> Result<cpal::Device, AudioError> {
    if let Some(id) = &config.device_id {
        let found = output_devices(host)?
            .into_iter()
            .map(|(_, device)| device)
            .find(|device| device.id().is_ok_and(|own| own.to_string() == id.0));
        if let Some(device) = found {
            return Ok(device);
        }
    }
    match (&config.device, &config.device_id) {
        (Some(name), _) => output_devices(host)?
            .into_iter()
            .find(|(seen, _)| seen == name)
            .map(|(_, device)| device)
            .ok_or_else(|| AudioError::NoDevice(format!("{name}: not found"))),
        (None, Some(id)) => Err(AudioError::NoDevice(format!("{id}: not found"))),
        (None, None) => host
            .default_output_device()
            .ok_or_else(|| AudioError::NoDevice("no default output device".to_owned())),
    }
}

/// Whether `config`'s device can be found (the reconnect probe; runs on its own thread).
pub(crate) fn device_present(config: &StreamConfig) -> bool {
    config.host == HostKind::Null
        || host(config.host).is_ok_and(|h| find_device(&h, config).is_ok())
}

/// Output devices of `kind`.
pub(crate) fn devices(kind: HostKind) -> Result<Vec<DeviceInfo>, AudioError> {
    if kind == HostKind::Null {
        return Ok(vec![DeviceInfo {
            host: HostKind::Null,
            id: None,
            name: NULL_DEVICE.to_owned(),
            is_default: true,
            channels: 2,
            sample_rates: STANDARD_RATES.to_vec(),
            buffer_range: Some((1, MAX_BUFFER_FRAMES)),
        }]);
    }
    let host = host(kind)?;
    let default = host.default_output_device();
    let mut list = Vec::new();
    for (name, device) in output_devices(&host)? {
        let Ok(config) = device.default_output_config() else {
            continue;
        };
        let ranges: Vec<SupportedStreamConfigRange> = device
            .supported_output_configs()
            .map(Iterator::collect)
            .unwrap_or_default();
        let sample_rates = STANDARD_RATES
            .into_iter()
            .filter(|&rate| {
                ranges
                    .iter()
                    .any(|r| FORMATS.contains(&r.sample_format()) && supports_rate(r, rate))
            })
            .collect();
        let buffer_range = match *config.buffer_size() {
            SupportedBufferSize::Range { min, max } => Some((min, max)),
            SupportedBufferSize::Unknown => None,
        };
        list.push(DeviceInfo {
            host: kind,
            id: device.id().ok().map(|id| DeviceId(id.to_string())),
            name,
            is_default: default.as_ref() == Some(&device),
            channels: config.channels(),
            sample_rates,
            buffer_range,
        });
    }
    Ok(list)
}

fn supports_rate(range: &SupportedStreamConfigRange, rate: u32) -> bool {
    (range.min_sample_rate()..=range.max_sample_rate()).contains(&rate)
}

/// The cpal configuration for a requested one.
pub(crate) struct Negotiated {
    pub(crate) config: cpal::StreamConfig,
    pub(crate) format: SampleFormat,
    /// What actually runs: JACK imposes the server's rate and buffer size.
    pub(crate) effective: StreamConfig,
}

/// Picks format, channels, rate and buffer size for `requested` on `device`.
///
/// Rules: `f32` before `i32` before `i16`; the device's default channel count, else stereo. JACK
/// runs at the server's rate and buffer size whatever was requested. PipeWire accepts rates
/// outside its allowed list (it converts). ALSA and CoreAudio must support the rate and buffer
/// size (CoreAudio then sets the device's nominal rate and its IO buffer frame size; cpal
/// checks the buffer against the device's range).
pub(crate) fn negotiate(
    kind: HostKind,
    device: &cpal::Device,
    requested: &StreamConfig,
) -> Result<Negotiated, AudioError> {
    let default = device.default_output_config().map_err(map_err)?;
    let ranges: Vec<SupportedStreamConfigRange> = device
        .supported_output_configs()
        .map_err(map_err)?
        .collect();
    let best = |rate: Option<u32>| {
        ranges
            .iter()
            .filter(|r| rate.is_none_or(|rate| supports_rate(r, rate)))
            .filter_map(|r| {
                let format = FORMATS.iter().position(|&f| f == r.sample_format())?;
                let channels = match r.channels() {
                    c if c == default.channels() => 0,
                    2 => 1,
                    _ => 2,
                };
                Some(((format, channels), *r))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, r)| r)
    };
    let unsupported_rate =
        || AudioError::UnsupportedConfig(format!("{} Hz not supported", requested.sample_rate));
    let (range, sample_rate) = match best(Some(requested.sample_rate)) {
        Some(range) => (range, requested.sample_rate),
        None => match kind {
            HostKind::Jack => (
                best(Some(default.sample_rate())).ok_or_else(unsupported_rate)?,
                default.sample_rate(),
            ),
            HostKind::PipeWire => (
                best(None).ok_or_else(unsupported_rate)?,
                requested.sample_rate,
            ),
            _ => return Err(unsupported_rate()),
        },
    };
    let (buffer_size, buffer_frames) = match *range.buffer_size() {
        // JACK's buffer is the server's (min == max); it may change while the stream runs, so let
        // cpal follow it instead of pinning it.
        SupportedBufferSize::Range { min, .. } if kind == HostKind::Jack => {
            (BufferSize::Default, min)
        }
        SupportedBufferSize::Range { min, max }
            if !(min..=max).contains(&requested.buffer_frames) =>
        {
            return Err(AudioError::UnsupportedConfig(format!(
                "buffer size {} not in {min}..={max}",
                requested.buffer_frames
            )));
        }
        _ => (
            BufferSize::Fixed(requested.buffer_frames),
            requested.buffer_frames,
        ),
    };
    Ok(Negotiated {
        config: cpal::StreamConfig {
            channels: range.channels(),
            sample_rate,
            buffer_size,
        },
        format: range.sample_format(),
        effective: StreamConfig {
            host: kind,
            device: requested.device.clone(),
            device_id: requested.device_id.clone(),
            sample_rate,
            buffer_frames,
        },
    })
}

/// Builds (but does not start) the output stream that drives `callback`.
pub(crate) fn build_stream<P: AudioProcess>(
    device: &cpal::Device,
    negotiated: &Negotiated,
    callback: StreamCallback<P>,
    stats: Arc<StreamStats>,
) -> Result<cpal::Stream, AudioError> {
    let config = negotiated.config;
    match negotiated.format {
        SampleFormat::F32 => build_typed::<f32, P>(device, config, callback, stats),
        SampleFormat::I32 => build_typed::<i32, P>(device, config, callback, stats),
        SampleFormat::I16 => build_typed::<i16, P>(device, config, callback, stats),
        other => Err(AudioError::UnsupportedConfig(format!(
            "sample format {other}"
        ))),
    }
}

fn build_typed<T, P>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut callback: StreamCallback<P>,
    stats: Arc<StreamStats>,
) -> Result<cpal::Stream, AudioError>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
    P: AudioProcess,
{
    let shared = Arc::clone(&callback.shared);
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let timestamp = info.timestamp();
                let latency = timestamp
                    .playback
                    .saturating_duration_since(timestamp.callback);
                callback.run(data, latency);
            },
            move |err: cpal::Error| on_stream_error(&stats, &shared, err),
            Some(BUILD_TIMEOUT),
        )
        .map_err(map_err)
}

/// The cpal error callback. Xruns and refused real-time promotion may arrive on the audio thread:
/// atomics only. The stream is dead after the other handled kinds, so their message may allocate.
pub(crate) fn on_stream_error(stats: &StreamStats, shared: &StreamShared, err: cpal::Error) {
    use cpal::ErrorKind as K;
    match err.kind() {
        K::Xrun => stats.record_xrun(),
        K::RealtimeDenied => stats.record_realtime_denied(),
        K::DeviceNotAvailable | K::StreamInvalidated | K::HostUnavailable => {
            shared.mark_lost(err.to_string());
        }
        // Rerouted to the new default device, or transient: the stream keeps running.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::SlotHandle;
    use crate::testing::TestEngine;

    fn callback(
        value: f32,
        max_frames: u32,
        channels: u16,
    ) -> (StreamCallback<TestEngine>, SlotHandle<TestEngine>) {
        let (engine, _) = TestEngine::boxed(value);
        let (_sender, receiver) = crate::midi::queue(4);
        let (slot, handle) = EngineSlot::new(engine, receiver, Arc::new(StreamStats::new()));
        let params = StreamParams {
            sample_rate: 48_000,
            max_frames,
            channels,
            workgroup: None,
        };
        let callback = StreamCallback::new(slot, params, Arc::new(StreamShared::default()));
        (callback, handle)
    }

    #[test]
    fn long_callbacks_are_cut_into_prepared_pieces() {
        let (mut cb, mut handle) = callback(0.5, 64, 2);
        let latency = Duration::from_millis(3);
        let mut data = vec![0.0f32; 200 * 2];
        let before = Instant::now();
        cb.run(&mut data, latency);
        let after = Instant::now();
        assert!(data.iter().all(|&s| s == 0.5));
        assert_eq!(cb.shared.latency(), latency);

        drop(cb);
        let engine = handle.try_take_engine().unwrap();
        let infos: Vec<CallbackInfo> = engine.infos.iter().flatten().copied().collect();
        let frames: Vec<u32> = infos.iter().map(|i| i.frames).collect();
        assert_eq!(frames, [64, 64, 64, 8]);
        // Each piece ends where the next begins; the last one ends at callback entry.
        // (Durations are rounded down to whole nanoseconds.)
        let close = |a: Duration, b: Duration| a.abs_diff(b) <= Duration::from_nanos(1);
        for pair in infos.windows(2) {
            let step = pair[1].now - pair[0].now;
            let expected = crate::frames_to_duration(u64::from(pair[1].frames), 48_000);
            assert!(close(step, expected), "{step:?} {expected:?}");
            let step = pair[1].output_latency - pair[0].output_latency;
            let expected = crate::frames_to_duration(u64::from(pair[0].frames), 48_000);
            assert!(close(step, expected), "{step:?} {expected:?}");
        }
        let last = infos.last().unwrap();
        assert!(before <= last.now && last.now <= after);
        assert_eq!(infos[0].output_latency, latency);
    }

    /// The device backends (PipeWire, JACK, ALSA) all run `StreamCallback::run`, which takes its
    /// time base from `Instant::now()` like the MIDI stamps (the same clock): an event stamped
    /// just before a callback is released by it, exactly once, inside the callback's frames, also
    /// when the callback is cut into several pieces and the device reports a latency.
    #[test]
    fn a_device_callback_releases_events_stamped_just_before_it() {
        for (max_frames, total) in [(256u32, 256usize), (64, 200), (256, 1024), (256, 100)] {
            let (engine, probe) = TestEngine::boxed(0.0);
            let (sender, receiver) = crate::midi::queue(8);
            let (slot, _handle) = EngineSlot::new(engine, receiver, Arc::new(StreamStats::new()));
            let params = StreamParams {
                sample_rate: 48_000,
                max_frames,
                channels: 2,
                workgroup: None,
            };
            let mut cb = StreamCallback::new(slot, params, Arc::new(StreamShared::default()));
            let mut data = vec![0.0f32; total * 2];
            assert!(sender.push(crate::midi::MidiPortId(1), [0x90, 60, 100]));
            std::thread::sleep(Duration::from_millis(1));
            cb.run(&mut data, Duration::from_millis(5));
            assert_eq!(
                probe.events.load(Ordering::SeqCst),
                1,
                "{max_frames}/{total}"
            );
            let frame = probe.last_event_frame.load(Ordering::SeqCst);
            assert!(frame < max_frames, "{max_frames}/{total}: frame {frame}");
            // Events from later callbacks keep coming through.
            assert!(sender.push(crate::midi::MidiPortId(1), [0x80, 60, 0]));
            std::thread::sleep(Duration::from_millis(1));
            cb.run(&mut data, Duration::from_millis(5));
            assert_eq!(
                probe.events.load(Ordering::SeqCst),
                2,
                "{max_frames}/{total}"
            );
        }
    }

    #[test]
    fn samples_are_converted_to_the_device_format() {
        let (mut cb, _handle) = callback(0.5, 32, 2);
        let mut ints = [0i16; 64];
        cb.run(&mut ints, Duration::ZERO);
        assert!(ints.iter().all(|&s| s == 16_384), "{ints:?}");
        let mut wide = [0i32; 64];
        cb.run(&mut wide, Duration::ZERO);
        assert!(wide.iter().all(|&s| s == 1 << 30));
    }

    #[test]
    fn validation_ranges() {
        assert!(valid_sample_rate(44_100) && !valid_sample_rate(0));
        assert!(!valid_sample_rate(MAX_SAMPLE_RATE + 1));
        assert!(valid_buffer_frames(64) && !valid_buffer_frames(0));
        assert!(!valid_buffer_frames(MAX_BUFFER_FRAMES + 1));
    }

    #[test]
    fn null_stream_stops_on_drop_and_hands_the_engine_back() {
        let (engine, probe) = TestEngine::boxed(0.0);
        let (slot, mut handle) = EngineSlot::empty(Arc::new(StreamStats::new()));
        handle.attach(engine).unwrap();
        let params = StreamParams {
            sample_rate: 48_000,
            max_frames: 32,
            channels: 2,
            workgroup: None,
        };
        let stream = NullStream::start(StreamCallback::new(
            slot,
            params,
            Arc::new(StreamShared::default()),
        ))
        .unwrap();
        assert!(crate::testing::wait_until(Duration::from_secs(5), || probe
            .calls()
            > 5));
        drop(stream);
        assert!(handle.is_closed());
        assert!(handle.try_take_engine().is_some());
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    /// The device host of this system; the lookup tests only enumerate its devices.
    const HOST: HostKind = if cfg!(target_os = "macos") {
        HostKind::CoreAudio
    } else {
        HostKind::Alsa
    };
    const PREFIX: &str = if cfg!(target_os = "macos") {
        "coreaudio"
    } else {
        "alsa"
    };

    fn config(device: Option<&str>, device_id: Option<&str>) -> StreamConfig {
        StreamConfig {
            host: HOST,
            device: device.map(str::to_owned),
            device_id: device_id.map(|id| DeviceId(id.to_owned())),
            ..StreamConfig::default()
        }
    }

    fn id_of(device: &cpal::Device) -> String {
        device.id().unwrap().to_string()
    }

    /// Uses the machine's devices of the system's host (ALSA, CoreAudio) (enumeration only, nothing is opened); skipped without any.
    #[test]
    fn devices_are_found_by_id_before_name() {
        let (Ok(host), Ok(devices)) = (host(HOST), devices(HOST)) else {
            eprintln!("no {HOST:?} host: skipped");
            return;
        };
        let Some(info) = devices.iter().find(|d| d.id.is_some()) else {
            eprintln!("no {HOST:?} device with an id: skipped");
            return;
        };
        let id = info.id.as_ref().unwrap().0.as_str();
        let missing = format!("{PREFIX}:komponist-test-missing");
        let missing = missing.as_str();

        let by_id = find_device(&host, &config(Some("komponist test: wrong name"), Some(id)));
        assert_eq!(id_of(&by_id.unwrap()), id);
        let by_name = find_device(&host, &config(Some(&info.name), Some(missing)));
        assert_eq!(id_of(&by_name.unwrap()), id);
        let other_host = find_device(
            &host,
            &config(Some(&info.name), Some("komponist-other-host:x")),
        );
        assert_eq!(id_of(&other_host.unwrap()), id);
        assert!(matches!(
            find_device(&host, &config(None, Some(missing))),
            Err(AudioError::NoDevice(_))
        ));
        assert!(device_present(&config(None, Some(id))));
        assert!(!device_present(&config(None, Some(missing))));
    }

    #[test]
    fn hosts_of_other_systems_are_unavailable() {
        for kind in [
            HostKind::CoreAudio,
            HostKind::PipeWire,
            HostKind::Jack,
            HostKind::Alsa,
        ] {
            if kind.is_supported_here() {
                continue;
            }
            assert!(matches!(host(kind), Err(AudioError::HostUnavailable(k)) if k == kind));
            assert!(matches!(devices(kind), Err(AudioError::HostUnavailable(_))));
            assert!(!available_hosts().contains(&kind));
        }
        assert_eq!(available_hosts().last(), Some(&HostKind::Null));
    }

    #[test]
    fn stream_errors_update_stats_and_loss() {
        let stats = StreamStats::new();
        let shared = StreamShared::default();
        on_stream_error(
            &stats,
            &shared,
            cpal::Error::new(cpal::ErrorKind::RealtimeDenied),
        );
        assert!(stats.realtime_denied());
        on_stream_error(&stats, &shared, cpal::Error::new(cpal::ErrorKind::Xrun));
        on_stream_error(
            &stats,
            &shared,
            cpal::Error::new(cpal::ErrorKind::DeviceChanged),
        );
        assert_eq!(stats.xruns(), 1);
        assert_eq!(shared.lost(), None);
        on_stream_error(
            &stats,
            &shared,
            cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable),
        );
        assert!(shared.lost().is_some());
    }
}
