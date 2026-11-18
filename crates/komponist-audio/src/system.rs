//! Audio hosts, devices and the stream lifecycle (ARCHITECTURE §7, R1, S07).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::StreamTrait;
use serde::{Deserialize, Serialize};

use crate::backend::{self, NullStream, StreamCallback, StreamShared};
use crate::midi::MidiReceiver;
use crate::process::{AudioProcess, StreamParams};
use crate::slot::{DetachStep, EngineSlot, SlotHandle};
use crate::workgroup;

/// Weight of one callback's load in the smoothed CPU load (time constant ≈ 20 callbacks).
const LOAD_SMOOTHING: f32 = 0.05;

/// How long a stream teardown may take to hand the engine back.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Poll interval while waiting for the callback to hand the engine out.
const DETACH_POLL: Duration = Duration::from_micros(500);

/// How often [`AudioSystem::poll`] looks for a missing device while the state is `NoDevice`.
const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// The same for an ASIO device: every probe loads every installed ASIO driver, which is slow and can
/// disturb an interface another program is using, so it runs less often.
const ASIO_RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Audio driver ("Treiber" in S07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    /// CoreAudio, the only device host on macOS.
    CoreAudio,
    /// WASAPI in shared mode, the default on Windows. Always unavailable off Windows.
    Wasapi,
    /// ASIO (cpal feature `asio`): Windows builds made with the crate feature `asio` only,
    /// unavailable everywhere else. One driver at a time per process; a sample rate change applies
    /// to the whole driver (ARCHITECTURE §7). Listed by [`AudioSystem::available_hosts`] only while at
    /// least one ASIO driver is registered.
    Asio,
    /// PipeWire (cpal feature `pipewire`), the default on current Linux desktops. Always
    /// unavailable off Linux.
    PipeWire,
    /// JACK (cpal feature `jack`). Unavailable off Linux.
    Jack,
    /// ALSA. Unavailable off Linux.
    Alsa,
    /// No device: a timer thread drives the engine and discards the output (tests, "Kein
    /// Audio-Gerät").
    Null,
}

impl HostKind {
    /// Whether this driver exists on the operating system the crate was built for: CoreAudio on
    /// macOS; WASAPI, and ASIO in builds with the feature `asio`, on Windows; PipeWire, JACK and
    /// ALSA on Linux; `Null` everywhere. The variants of the other systems stay in the enum so that a
    /// settings file written on one system still loads on the others (see [`StreamConfig`]); they are never available there.
    pub const fn is_supported_here(self) -> bool {
        match self {
            HostKind::Null => true,
            HostKind::CoreAudio => cfg!(target_os = "macos"),
            HostKind::Wasapi => cfg!(windows),
            HostKind::Asio => cfg!(all(windows, feature = "asio")),
            HostKind::PipeWire | HostKind::Jack | HostKind::Alsa => cfg!(target_os = "linux"),
        }
    }

    /// The driver a fresh configuration starts with: CoreAudio on macOS, WASAPI on Windows,
    /// PipeWire elsewhere.
    pub const fn platform_default() -> HostKind {
        if cfg!(target_os = "macos") {
            HostKind::CoreAudio
        } else if cfg!(windows) {
            HostKind::Wasapi
        } else {
            HostKind::PipeWire
        }
    }
}

/// Stable identity of an output device, persisted in [`StreamConfig::device_id`].
///
/// cpal's `DeviceId` in its string form `"<host>:<id>"`: the PipeWire node name
/// (`"pipewire:alsa_output.usb-Focusrite_Scarlett_2i2-00.analog-stereo"`), the ALSA PCM name
/// (`"alsa:hw:CARD=USB,DEV=0"`), `"jack:output"`, the CoreAudio device UID
/// (`"coreaudio:BuiltInSpeakerDevice"`), the WASAPI endpoint id
/// (`"wasapi:{0.0.0.00000000}.{guid}"`). Unlike [`DeviceInfo::name`] it is unique and
/// does not depend on enumeration order. Serialised as a plain string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub String);

impl core::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An output device of a host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    /// The host it belongs to.
    pub host: HostKind,
    /// Stable id, `None` if the backend gives none (and for the null host). Store it in
    /// [`StreamConfig::device_id`].
    pub id: Option<DeviceId>,
    /// Device name as the backend reports it ("Focusrite Scarlett 2i2"); used in [`StreamConfig`].
    /// Names are not unique: a repeated name gets " (2)", " (3)", … in enumeration order.
    pub name: String,
    /// The host's default output device.
    pub is_default: bool,
    /// Output channels of the default configuration.
    pub channels: u16,
    /// Supported sample rates in Hz, ascending (from 44100, 48000, 88200, 96000, 192000 those the
    /// device accepts).
    pub sample_rates: Vec<u32>,
    /// Supported buffer sizes in frames as `(min, max)`, `None` if the backend does not say.
    pub buffer_range: Option<(u32, u32)>,
}

/// The stream the user picked in S07. Persisted in the settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "StoredStreamConfig")]
pub struct StreamConfig {
    /// Driver. A saved configuration that names a driver this system does not have (PipeWire,
    /// JACK or ALSA on macOS and Windows, CoreAudio on Linux and Windows, WASAPI on Linux and
    /// macOS) loads as the default configuration of this system: [`HostKind::platform_default`] with the default device (`device` and `device_id`
    /// dropped, they belong to the other system), rate and buffer size kept.
    pub host: HostKind,
    /// Output device name (display, and fallback for the lookup); `None` together with
    /// `device_id: None` = the host's default device.
    pub device: Option<String>,
    /// Stable id of the output device ([`DeviceInfo::id`]). [`AudioSystem::start`] looks the
    /// device up by id first and by `device` (the name) if the id is unknown or belongs to another
    /// host. Configurations saved without it deserialise with `None`.
    #[serde(default)]
    pub device_id: Option<DeviceId>,
    /// Sample rate in Hz ("48000 Hz").
    pub sample_rate: u32,
    /// Buffer size in frames (segments 64 / 128 / 256 / 512 / 1024 in S07).
    pub buffer_frames: u32,
}

/// The on-disk shape of [`StreamConfig`]; the conversion migrates settings written on another
/// system.
#[derive(Deserialize)]
struct StoredStreamConfig {
    host: HostKind,
    device: Option<String>,
    #[serde(default)]
    device_id: Option<DeviceId>,
    sample_rate: u32,
    buffer_frames: u32,
}

impl From<StoredStreamConfig> for StreamConfig {
    fn from(stored: StoredStreamConfig) -> StreamConfig {
        let (host, device, device_id) = if stored.host.is_supported_here() {
            (stored.host, stored.device, stored.device_id)
        } else {
            (HostKind::platform_default(), None, None)
        };
        StreamConfig {
            host,
            device,
            device_id,
            sample_rate: stored.sample_rate,
            buffer_frames: stored.buffer_frames,
        }
    }
}

impl Default for StreamConfig {
    /// [`HostKind::platform_default`] (CoreAudio on macOS, WASAPI on Windows, PipeWire on Linux), default device,
    /// 48000 Hz, 256 frames.
    fn default() -> StreamConfig {
        StreamConfig {
            host: HostKind::platform_default(),
            device: None,
            device_id: None,
            sample_rate: 48_000,
            buffer_frames: 256,
        }
    }
}

/// State of the audio system as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioState {
    /// No stream (before the first start, after `stop`).
    Stopped,
    /// A device stream runs.
    Running {
        /// The active configuration.
        config: StreamConfig,
        /// Output latency reported by the stream; the S07 display shows
        /// `buffer / rate` plus this.
        latency: Duration,
    },
    /// The requested output could not be opened or was lost. The engine runs on the null backend
    /// meanwhile, so commands keep draining; the UI disables play and record and shows the red
    /// notice (transport bar, status bar, S07).
    NoDevice {
        /// The configuration that failed.
        config: StreamConfig,
        /// Backend message for the log and tooltip.
        reason: String,
    },
}

/// Why an audio operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// The host is not available on this system.
    HostUnavailable(HostKind),
    /// No device with that name, or it could not be opened.
    NoDevice(String),
    /// The device rejects the rate or buffer size.
    UnsupportedConfig(String),
    /// Other backend failure.
    Backend(String),
    /// The engine did not come back within the timeout (detach).
    Timeout,
    /// No engine to detach (already detached).
    Detached,
}

impl core::fmt::Display for AudioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AudioError::HostUnavailable(h) => write!(f, "audio host {h:?} not available"),
            AudioError::NoDevice(m) => write!(f, "no audio device: {m}"),
            AudioError::UnsupportedConfig(m) => write!(f, "unsupported configuration: {m}"),
            AudioError::Backend(m) => write!(f, "audio backend error: {m}"),
            AudioError::Timeout => write!(f, "engine did not return in time"),
            AudioError::Detached => write!(f, "engine is detached"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Stream statistics, written by the audio callback, read by the UI every frame.
#[derive(Debug, Default)]
pub struct StreamStats {
    /// Smoothed CPU load as `f32` bits: callback duration / buffer period, 0.0 ..= (above 1.0 means
    /// overload). Shown as "18 %".
    cpu_load: AtomicU32,
    /// Underruns reported by the backend or detected (callback late), since the system was created.
    xruns: AtomicU32,
    /// Callbacks run, since the system was created.
    callbacks: AtomicU64,
    /// Highest load of a single callback since the last [`StreamStats::take_peak_load`], as `f32`
    /// bits (non-negative floats order like their bits).
    peak_load: AtomicU32,
    /// Callbacks whose own work took longer than their buffer period (an xrun the app caused,
    /// whatever the scheduler did), since the system was created.
    overruns: AtomicU32,
    /// The backend refused real-time scheduling for the current stream's audio thread.
    realtime_denied: AtomicBool,
}

impl StreamStats {
    /// All zero.
    pub fn new() -> StreamStats {
        StreamStats::default()
    }

    /// Smoothed CPU load (1.0 = the whole buffer period).
    pub fn cpu_load(&self) -> f32 {
        f32::from_bits(self.cpu_load.load(Ordering::Relaxed))
    }

    /// Xruns so far.
    pub fn xruns(&self) -> u32 {
        self.xruns.load(Ordering::Relaxed)
    }

    /// Callbacks so far.
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }

    /// Highest load of a single callback since the last call (1.0 = the whole buffer period);
    /// resets it. For measurements and soak tests.
    pub fn take_peak_load(&self) -> f32 {
        f32::from_bits(self.peak_load.swap(0, Ordering::Relaxed))
    }

    /// Callbacks so far whose own work took longer than their buffer period (load above 1.0).
    /// Unlike [`StreamStats::xruns`] this does not count late wake-ups of the audio thread.
    pub fn overruns(&self) -> u32 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// RT. Folds one callback's load (duration / period) into the smoothed value and counts the
    /// callback (and an overrun above 1.0).
    ///
    /// One-pole smoothing per callback (weight 0.05); the first callback sets the value directly.
    pub fn record_callback(&self, load: f32) {
        let load = if load.is_finite() { load.max(0.0) } else { 0.0 };
        self.peak_load.fetch_max(load.to_bits(), Ordering::Relaxed);
        if load > 1.0 {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
        let first = self.callbacks.fetch_add(1, Ordering::Relaxed) == 0;
        let smoothed = if first {
            load
        } else {
            let old = self.cpu_load();
            old + (load - old) * LOAD_SMOOTHING
        };
        self.cpu_load.store(smoothed.to_bits(), Ordering::Relaxed);
    }

    /// RT. Counts one xrun.
    pub fn record_xrun(&self) {
        self.xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether the backend reported that real-time scheduling was refused for the audio thread of
    /// the current stream (cpal `RealtimeDenied`): the hint in S07 and the status bar. Reset when a
    /// stream is torn down.
    ///
    /// `false` does not prove real-time scheduling: the null backend and server-backed ALSA devices
    /// (such as ALSA's PipeWire default) are not promoted and report nothing. PipeWire is promoted
    /// through RTKit; JACK threads are the server's. CoreAudio never reports it (its I/O thread
    /// is the system's).
    pub fn realtime_denied(&self) -> bool {
        self.realtime_denied.load(Ordering::Relaxed)
    }

    /// Error callback (any thread): real-time scheduling was refused.
    pub(crate) fn record_realtime_denied(&self) {
        self.realtime_denied.store(true, Ordering::Relaxed);
    }

    fn reset_realtime_denied(&self) {
        self.realtime_denied.store(false, Ordering::Relaxed);
    }
}

/// Owns the stream and the engine while no stream holds it. UI thread.
///
/// The engine survives every stream change (R1): on teardown the stream's
/// [`crate::EngineSlot`] hands the engine back, [`AudioProcess::prepare`] runs with the new
/// parameters, and the new stream takes it over. A buffer change is "short dropout, engine
/// untouched" (S07).
///
/// A new stream starts empty and outputs silence until the system has handed it the engine and
/// the MIDI receiver (right after the stream started, or in [`AudioSystem::poll`]), so a stream that
/// fails to open never holds the engine.
pub struct AudioSystem<P: AudioProcess> {
    state: AudioState,
    stats: Arc<StreamStats>,
    /// The engine while no stream holds it (and it is not detached).
    engine: Option<Box<P>>,
    /// The MIDI receiver while no stream holds it.
    midi: Option<MidiReceiver>,
    /// The engine is out (detach) until `attach`.
    detached: bool,
    current: Option<Current<P>>,
    /// Handles of torn-down streams that have not handed back yet.
    retired: Vec<SlotHandle<P>>,
    /// Reconnection while the configured device is missing.
    retry: Option<Retry<StreamConfig>>,
    retry_interval: Duration,
    input: Option<crate::InputConfig>,
    input_error: Option<AudioError>,
    input_channels: u16,
    synthetic_input: Option<crate::InputReader>,
}

/// Looking for a missing output or a previously opened capture input.
struct Retry<T> {
    target: T,
    /// Time between two probes.
    interval: Duration,
    /// When the next probe starts.
    next: Instant,
    /// A probe running on its own thread: is the device present?
    probe: Option<JoinHandle<bool>>,
}

impl<T: Clone + Send + 'static> Retry<T> {
    fn new(target: T, interval: Duration) -> Self {
        Self {
            target,
            interval,
            next: Instant::now() + interval,
            probe: None,
        }
    }

    fn available(
        &mut self,
        thread_name: &str,
        present: impl FnOnce(&T) -> bool + Send + 'static,
    ) -> bool {
        match self.probe.take() {
            Some(probe) if probe.is_finished() => {
                self.next = Instant::now() + self.interval;
                return probe.join().unwrap_or(false);
            }
            Some(probe) => self.probe = Some(probe),
            None if Instant::now() >= self.next => {
                let target = self.target.clone();
                self.probe = thread::Builder::new()
                    .name(thread_name.to_owned())
                    .spawn(move || present(&target))
                    .ok();
                if self.probe.is_none() {
                    self.next = Instant::now() + self.interval;
                }
            }
            None => {}
        }
        false
    }
}

fn retry_interval(host: HostKind, nominal: Duration) -> Duration {
    if host == HostKind::Asio {
        nominal.max(ASIO_RETRY_INTERVAL)
    } else {
        nominal
    }
}

/// The running stream.
struct Current<P: AudioProcess> {
    /// Declared first: dropped before the handle, so the slot has handed back by then.
    backend: Backend,
    handle: SlotHandle<P>,
    params: StreamParams,
    shared: Arc<StreamShared>,
    /// The configuration `start` was called with.
    requested: StreamConfig,
}

/// Held for its `Drop`, which stops the stream.
enum Backend {
    // CPAL streams are large on Windows; allocate their storage only when starting a device.
    Device(Box<DeviceStreams>),
    Null { _stream: NullStream },
}

struct DeviceStreams {
    _stream: cpal::Stream,
    capture: Option<crate::input::CaptureStream>,
    capture_transfer: crate::input::CaptureTransfer,
    capture_retry: Option<Retry<DeviceId>>,
    asio_device: Option<AsioDeviceFactory>,
    /// Declared after the streams: the open ASIO count drops only once they are gone.
    _open: Option<backend::AsioOpen>,
}

type AsioDeviceFactory = Box<dyn Fn() -> cpal::Device + Send>;

fn asio_device_factory(device: &cpal::Device) -> Option<AsioDeviceFactory> {
    // The platform Device enum can contain a non-Send COM endpoint on Windows. Retain only
    // the concrete ASIO device so the audio system remains movable between control threads.
    #[cfg(all(windows, feature = "asio"))]
    if let cpal::platform::DeviceInner::Asio(loaded) = device.as_inner() {
        let loaded = loaded.clone();
        return Some(Box::new(move || cpal::Device::from(loaded.clone())));
    }
    let _ = device;
    None
}

impl DeviceStreams {
    fn poll_capture(
        &mut self,
        host: HostKind,
        params: &StreamParams,
        interval: Duration,
    ) -> Option<Result<u16, AudioError>> {
        let transfer_ready = self.capture_transfer.ready();
        if let Some(lost) = self.capture.take_if(|capture| capture.lost()) {
            self.capture_retry = lost
                .device_id()
                .cloned()
                .map(|id| Retry::new(id, retry_interval(host, interval)));
            return Some(Err(AudioError::NoDevice("capture input lost".into())));
        }

        let retry = self.capture_retry.as_mut()?;
        if !transfer_ready
            || !retry.available("komponist-capture-probe", move |id| {
                // ASIO can only use the output's already loaded driver. Its input does not need
                // enumeration, which might try to load another driver while playback is running.
                host == HostKind::Asio
                    || crate::input_devices(host).is_ok_and(|devices| {
                        devices.iter().any(|device| device.id.as_ref() == Some(id))
                    })
            })
        {
            return None;
        }

        // Reconnect the source that really opened, even if it was originally selected by name
        // or as the default. An exact ID cannot fall back to another device.
        let target = crate::InputConfig {
            device: None,
            device_id: Some(retry.target.clone()),
        };
        let asio_device = self.asio_device.as_ref().map(|device| device());
        match crate::input::CaptureStream::start(
            host,
            &target,
            params.sample_rate,
            params.max_frames,
            asio_device.as_ref(),
        ) {
            Ok((capture, reader)) => {
                let channels = reader.channels() as u16;
                if let Err(error) = self.capture_transfer.install(reader, params.max_frames) {
                    return Some(Err(error));
                }
                self.capture = Some(capture);
                self.capture_retry = None;
                Some(Ok(channels))
            }
            Err(error) => Some(Err(error)),
        }
    }
}

impl<P: AudioProcess> AudioSystem<P> {
    /// Takes ownership of the engine and the MIDI receiver; no stream yet (state `Stopped`).
    pub fn new(engine: Box<P>, midi: MidiReceiver) -> AudioSystem<P> {
        AudioSystem {
            state: AudioState::Stopped,
            stats: Arc::new(StreamStats::new()),
            engine: Some(engine),
            midi: Some(midi),
            detached: false,
            current: None,
            retired: Vec::new(),
            retry: None,
            retry_interval: RETRY_INTERVAL,
            input: None,
            input_error: None,
            input_channels: 0,
            synthetic_input: None,
        }
    }

    /// Hosts available on this machine, in preference order (CoreAudio on macOS; WASAPI, ASIO on
    /// Windows; PipeWire, JACK, ALSA on Linux; `Null` always last).
    ///
    /// ASIO counts as available when at least one driver is registered (`HKLM\SOFTWARE\ASIO`; the check reads
    /// the registry and loads no driver). Without one it is missing from the list, which S07 shows as a
    /// disabled segment.
    ///
    /// JACK counts as available when a JACK server (or PipeWire's JACK emulation) answers; the
    /// check opens a short-lived JACK client.
    pub fn available_hosts() -> Vec<HostKind> {
        backend::available_hosts()
    }

    /// Output devices of `host`.
    ///
    /// `Null` lists one device, `"null"`. PipeWire lists the graph's allowed rates as
    /// `sample_rates`; other rates still open (PipeWire converts). CoreAudio lists the rates and
    /// the buffer range the device reports; starting a stream sets the device's nominal rate.
    pub fn devices(host: HostKind) -> Result<Vec<DeviceInfo>, AudioError> {
        backend::devices(host)
    }

    /// Input devices of this host. Does not open any input stream.
    pub fn input_devices(host: HostKind) -> Result<Vec<crate::InputDeviceInfo>, AudioError> {
        crate::input_devices(host)
    }

    /// Configures capture explicitly. `None` disables it; a default configuration selects
    /// the host's default input. A capture error leaves playback running and keeps the requested
    /// selection so the UI can show it and the user can retry it.
    pub fn set_input(&mut self, input: Option<crate::InputConfig>) -> Result<(), AudioError> {
        self.set_input_with(input, Self::start)
    }

    fn set_input_with(
        &mut self,
        input: Option<crate::InputConfig>,
        start: impl FnOnce(&mut Self, &StreamConfig) -> Result<(), AudioError>,
    ) -> Result<(), AudioError> {
        if self.detached {
            return Err(AudioError::Detached);
        }
        if self.input == input && self.input_error.is_none() {
            return Ok(());
        }
        self.input = input;
        let config = match &self.state {
            AudioState::NoDevice { config, .. } => Some(config.clone()),
            _ => self
                .current
                .as_ref()
                .map(|current| current.requested.clone()),
        };
        if let Some(config) = config {
            start(self, &config)?;
        }
        self.input_error.clone().map_or(Ok(()), Err)
    }

    /// Requested capture configuration, absent by default.
    pub fn input(&self) -> Option<&crate::InputConfig> {
        self.input.as_ref()
    }

    /// Why requested capture is unavailable while playback is running.
    pub fn input_error(&self) -> Option<&AudioError> {
        self.input_error.as_ref()
    }

    /// Channels available from the current capture stream (zero without capture).
    pub fn input_channels(&self) -> u16 {
        self.input_channels
    }

    /// Installs synthetic capture on the null backend. Rebuilding a stream retires this
    /// source; tests explicitly install a new one after a rebuild. Never opens hardware.
    pub fn set_test_input(
        &mut self,
        channels: u16,
        sample_rate: u32,
    ) -> Result<crate::TestAudioSource, AudioError> {
        if self.detached {
            return Err(AudioError::Detached);
        }
        let config = self
            .current
            .as_ref()
            .map(|c| c.requested.clone())
            .unwrap_or(StreamConfig {
                host: HostKind::Null,
                ..StreamConfig::default()
            });
        if config.host != HostKind::Null {
            return Err(AudioError::UnsupportedConfig(
                "synthetic input requires the null backend".into(),
            ));
        }
        let capacity = ((config.buffer_frames as usize).saturating_mul(8)).clamp(4096, 65_536);
        let (source, reader) = crate::test_input(channels, sample_rate, capacity)?;
        self.teardown();
        self.synthetic_input = Some(reader);
        self.start(&config)?;
        Ok(source)
    }

    /// Starts playback using `config`, replacing a running one (stop → prepare → start). On failure
    /// the engine runs on the null backend with the requested rate and buffer, the state becomes
    /// `NoDevice` and the error is returned.
    ///
    /// The `Running` state carries the effective configuration: JACK imposes the server's rate and
    /// buffer size. Its `latency` is filled in by [`AudioSystem::poll`] once callbacks run.
    ///
    /// The device is looked up by `device_id`, then by `device` (name), and is the host's default
    /// only when both are `None`. When the device is missing (`NoDevice`, `HostUnavailable`) or the
    /// output backend fails (`Backend`), [`AudioSystem::poll`] keeps looking for it.
    ///
    /// An unavailable capture input is reported by [`AudioSystem::input_error`]; it does not fail
    /// playback or trigger an output reconnect.
    pub fn start(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        self.start_with(config, |system, config| match config.host {
            HostKind::Null => system.start_null(config).map(|()| config.clone()),
            _ => system.start_device(config),
        })
    }

    fn start_with(
        &mut self,
        config: &StreamConfig,
        start_output: impl FnOnce(&mut Self, &StreamConfig) -> Result<StreamConfig, AudioError>,
    ) -> Result<(), AudioError> {
        self.retry = None;
        self.input_error = None;
        self.teardown();
        match start_output(self, config) {
            Ok(effective) => {
                self.state = AudioState::Running {
                    config: effective,
                    latency: Duration::ZERO,
                };
                Ok(())
            }
            Err(err) => {
                self.input_error = None;
                self.input_channels = 0;
                // Keep the engine running so its commands drain. If even the null thread cannot
                // start, the engine waits in the system for the next `start`.
                let _ = self.start_null(&null_config(config));
                self.state = AudioState::NoDevice {
                    config: config.clone(),
                    reason: err.to_string(),
                };
                if matches!(
                    err,
                    AudioError::NoDevice(_)
                        | AudioError::HostUnavailable(_)
                        | AudioError::Backend(_)
                ) {
                    self.arm_retry(config.clone());
                }
                Err(err)
            }
        }
    }

    /// Stops the stream; the engine comes back into the system. State `Stopped`.
    pub fn stop(&mut self) {
        self.retry = None;
        self.input_error = None;
        self.teardown();
        self.state = AudioState::Stopped;
    }

    /// Current state.
    pub fn state(&self) -> &AudioState {
        &self.state
    }

    /// Shared statistics (survive stream changes).
    pub fn stats(&self) -> &Arc<StreamStats> {
        &self.stats
    }

    /// UI thread, every frame: notices a lost device (→ `NoDevice`, engine to the null backend) and
    /// collects engines handed back by torn-down streams. A lost capture input disconnects and
    /// probes for its stable ID independently while playback keeps running.
    ///
    /// It also hands an engine that came back late (after a timed-out detach) to the stream again
    /// and copies the stream's output latency into the `Running` state.
    ///
    /// While the state is `NoDevice` because the configured device is missing or was lost, it
    /// looks for the device about every 2 s. The lookup runs on a short-lived thread, so the UI
    /// thread does not wait for the enumeration; once the device is present, `poll` starts it again
    /// (on this thread, like [`AudioSystem::start`]) and the state returns to `Running` with the
    /// same configuration. That transition is the report that the device is back.
    pub fn poll(&mut self) {
        self.collect_retired();
        let mut returned = None;
        let mut failure = None;
        if let Some(current) = self.current.as_mut() {
            returned = current.handle.try_take_engine();
            let reason = current.shared.lost().or_else(|| {
                // The callback's thread ended on its own (it panicked).
                current
                    .handle
                    .is_closed()
                    .then(|| "audio stream stopped".to_owned())
            });
            match reason {
                Some(reason) => failure = Some((current.requested.clone(), reason)),
                None => {
                    if let Backend::Device(streams) = &mut current.backend
                        && let Some(capture) = streams.poll_capture(
                            current.requested.host,
                            &current.params,
                            self.retry_interval,
                        )
                    {
                        match capture {
                            Ok(channels) => {
                                self.input_channels = channels;
                                self.input_error = None;
                            }
                            Err(error) => {
                                self.input_channels = 0;
                                self.input_error = Some(error);
                            }
                        }
                    }
                    if let AudioState::Running { latency, .. } = &mut self.state {
                        *latency = current.shared.latency();
                    }
                }
            }
        }
        if let Some(engine) = returned {
            self.keep_engine(engine);
        }
        if let Some((config, reason)) = failure {
            self.input_error = None;
            self.teardown();
            let _ = self.start_null(&null_config(&config));
            self.state = AudioState::NoDevice {
                config: config.clone(),
                reason,
            };
            self.arm_retry(config);
        }
        self.hand_over();
        self.poll_retry();
    }

    /// Takes the engine out of the running stream (export, ARCHITECTURE §6.7): the callback hands
    /// it back at its next run and outputs silence from then on. Waits at most `timeout`.
    ///
    /// Without a stream the engine comes straight out of the system. After `Timeout` the request
    /// stays pending; an engine handed out later goes back into the stream at the next
    /// [`AudioSystem::poll`].
    pub fn detach(&mut self, timeout: Duration) -> Result<Box<P>, AudioError> {
        if self.detached {
            return Err(AudioError::Detached);
        }
        let deadline = Instant::now() + timeout;
        loop {
            self.collect_retired();
            let mut slot_pending = false;
            if let Some(current) = self.current.as_mut() {
                match current.handle.detach_step() {
                    DetachStep::Engine(engine) => self.keep_engine(engine),
                    DetachStep::Pending => slot_pending = true,
                    DetachStep::Empty => {}
                }
            }
            if let Some(engine) = self.engine.take() {
                self.detached = true;
                return Ok(engine);
            }
            let pending = slot_pending || !self.retired.is_empty();
            if !pending {
                return Err(AudioError::Detached);
            }
            if Instant::now() >= deadline {
                return Err(AudioError::Timeout);
            }
            thread::sleep(DETACH_POLL);
        }
    }

    /// Gives a detached engine back: calls `prepare` with the stream's parameters, then the
    /// callback takes it over at its next run.
    ///
    /// Without a stream the engine stays in the system and is prepared at the next `start`.
    pub fn attach(&mut self, engine: Box<P>) {
        self.detached = false;
        self.keep_engine(engine);
        self.hand_over();
    }

    /// Opens a cpal stream for `config`; returns the effective configuration.
    fn start_device(&mut self, config: &StreamConfig) -> Result<StreamConfig, AudioError> {
        backend::validate(config)?;
        let host = backend::host(config.host)?;
        let device = backend::find_device(&host, config)?;
        let negotiated = backend::negotiate(config.host, &device, config)?;
        let params = StreamParams {
            sample_rate: negotiated.effective.sample_rate,
            max_frames: negotiated.effective.buffer_frames,
            channels: negotiated.config.channels,
            // For the engine's workers; the callback runs on the HAL's IO thread, a member already.
            workgroup: workgroup::lookup(&device),
        };
        let (slot, handle) = EngineSlot::empty(Arc::clone(&self.stats));
        let shared = Arc::new(StreamShared::default());
        let callback_params = StreamParams {
            workgroup: None,
            ..params.clone()
        };
        let (capture, reader) = self.open_capture(|input| {
            crate::input::CaptureStream::start(
                config.host,
                input,
                negotiated.effective.sample_rate,
                negotiated.effective.buffer_frames,
                Some(&device),
            )
        });
        let mut callback =
            StreamCallback::new(slot, callback_params, Arc::clone(&shared)).with_input(reader);
        let capture_transfer = callback.enable_capture_reconnect();
        // Before the stream exists, so no enumeration sees an ASIO driver that is loading.
        let open = (config.host == HostKind::Asio).then(|| backend::AsioOpen::new(&device));
        let stream =
            backend::build_stream(&device, &negotiated, callback, Arc::clone(&self.stats))?;
        stream.play().map_err(backend::map_err)?;
        self.install(Current {
            backend: Backend::Device(Box::new(DeviceStreams {
                _stream: stream,
                capture,
                capture_transfer,
                capture_retry: None,
                asio_device: asio_device_factory(&device),
                _open: open,
            })),
            handle,
            params,
            shared,
            requested: config.clone(),
        });
        Ok(negotiated.effective)
    }

    /// Opens capture independently of playback. A failed input never vetoes a working output.
    fn open_capture<C>(
        &mut self,
        open: impl FnOnce(&crate::InputConfig) -> Result<(C, crate::InputReader), AudioError>,
    ) -> (Option<C>, Option<crate::InputReader>) {
        match self.input.as_ref().map(open) {
            Some(Ok((capture, reader))) => {
                self.input_channels = reader.channels() as u16;
                (Some(capture), Some(reader))
            }
            Some(Err(error)) => {
                self.input_error = Some(error);
                (None, None)
            }
            None => (None, None),
        }
    }

    /// Starts the null backend with `config`'s rate and buffer size (stereo).
    fn start_null(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        backend::validate(config)?;
        let params = StreamParams {
            sample_rate: config.sample_rate,
            max_frames: config.buffer_frames,
            channels: 2,
            workgroup: None,
        };
        let (slot, handle) = EngineSlot::empty(Arc::clone(&self.stats));
        let shared = Arc::new(StreamShared::default());
        let reader = self.synthetic_input.take();
        self.input_channels = reader.as_ref().map_or(0, |r| r.channels() as u16);
        let callback =
            StreamCallback::new(slot, params.clone(), Arc::clone(&shared)).with_input(reader);
        let stream = NullStream::start(callback)?;
        self.install(Current {
            backend: Backend::Null { _stream: stream },
            handle,
            params,
            shared,
            requested: config.clone(),
        });
        Ok(())
    }

    fn install(&mut self, current: Current<P>) {
        self.current = Some(current);
        self.hand_over();
    }

    /// Hands the MIDI receiver and the engine (prepared for the stream) to the running stream.
    fn hand_over(&mut self) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if let Some(midi) = self.midi.take()
            && let Err(midi) = current.handle.attach_midi(midi)
        {
            self.midi = Some(midi);
        }
        if current.handle.is_occupied() {
            return;
        }
        if let Some(mut engine) = self.engine.take() {
            engine.prepare(&current.params);
            if let Err(engine) = current.handle.attach(engine) {
                self.engine = Some(engine);
            }
        }
    }

    /// Drops the running stream and takes the engine and the MIDI receiver back.
    fn teardown(&mut self) {
        self.input_channels = 0;
        let Some(current) = self.current.take() else {
            return;
        };
        let Current {
            backend,
            mut handle,
            ..
        } = current;
        // Every backend drops its callback (and with it the slot) before this returns; the wait is
        // a safety net.
        drop(backend);
        self.stats.reset_realtime_denied();
        let deadline = Instant::now() + TEARDOWN_TIMEOUT;
        while !handle.is_closed() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        self.reclaim(&mut handle);
        if !handle.is_closed() {
            self.retired.push(handle);
        }
    }

    /// Takes back what `handle`'s slot handed out.
    fn reclaim(&mut self, handle: &mut SlotHandle<P>) {
        while let Some(engine) = handle.try_take_engine() {
            self.keep_engine(engine);
        }
        if let Some(midi) = handle.try_take_midi() {
            self.midi.get_or_insert(midi);
        }
    }

    fn collect_retired(&mut self) {
        let mut retired = std::mem::take(&mut self.retired);
        retired.retain_mut(|handle| {
            self.reclaim(handle);
            !handle.is_closed()
        });
        self.retired.append(&mut retired);
    }

    /// Keeps `engine` in the system. The system owns one engine; a second one (an `attach` without
    /// a `detach`) is dropped here, on the UI thread.
    fn keep_engine(&mut self, engine: Box<P>) {
        if self.engine.is_none() {
            self.engine = Some(engine);
        }
    }

    /// Starts probing for a missing output device.
    fn arm_retry(&mut self, config: StreamConfig) {
        let interval = retry_interval(config.host, self.retry_interval);
        self.retry = Some(Retry::new(config, interval));
    }

    /// Restarts the output once a probe finds it. Capture reconnection runs independently.
    fn poll_retry(&mut self) {
        if let Some(retry) = self.retry.as_mut()
            && retry.available("komponist-audio-probe", backend::device_present)
        {
            let config = retry.target.clone();
            let _ = self.start(&config);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_retry_interval(&mut self, interval: Duration) {
        self.retry_interval = interval;
    }

    #[cfg(test)]
    pub(crate) fn is_retrying(&self) -> bool {
        self.retry.is_some()
    }

    #[cfg(test)]
    pub(crate) fn simulate_device_loss(&self, reason: &str) {
        if let Some(current) = &self.current {
            current.shared.simulate_loss(reason);
        }
    }
}

impl<P: AudioProcess> Drop for AudioSystem<P> {
    /// Stops the stream first, so the engine comes back and drops here, on the UI thread.
    fn drop(&mut self) {
        self.teardown();
    }
}

/// The null backend's configuration for a stream that could not open: the requested rate and
/// buffer size, or 48000 Hz / 256 frames where those are out of range.
fn null_config(config: &StreamConfig) -> StreamConfig {
    let defaults = StreamConfig::default();
    StreamConfig {
        host: HostKind::Null,
        device: None,
        device_id: None,
        sample_rate: if backend::valid_sample_rate(config.sample_rate) {
            config.sample_rate
        } else {
            defaults.sample_rate
        },
        buffer_frames: if backend::valid_buffer_frames(config.buffer_frames) {
            config.buffer_frames
        } else {
            defaults.buffer_frames
        },
    }
}

impl<P: AudioProcess> core::fmt::Debug for AudioSystem<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AudioSystem").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::{MidiPortId, queue};
    use crate::testing::{Probe, TestEngine, wait_until};

    const WAIT: Duration = Duration::from_secs(5);

    fn null(buffer_frames: u32) -> StreamConfig {
        StreamConfig {
            host: HostKind::Null,
            device: None,
            device_id: None,
            sample_rate: 48_000,
            buffer_frames,
        }
    }

    fn system(value: f32) -> (AudioSystem<TestEngine>, Arc<Probe>, crate::midi::MidiSender) {
        let (engine, probe) = TestEngine::boxed(value);
        let (sender, receiver) = queue(64);
        (AudioSystem::new(engine, receiver), probe, sender)
    }

    /// Waits until the engine ran `n` more callbacks.
    fn runs(probe: &Probe, n: u64) -> bool {
        let start = probe.calls();
        wait_until(WAIT, || probe.calls() >= start + n)
    }

    #[test]
    fn stats_smooth_the_load() {
        let stats = StreamStats::new();
        stats.record_callback(1.0);
        assert_eq!(stats.cpu_load(), 1.0);
        stats.record_callback(0.0);
        assert!((stats.cpu_load() - 0.95).abs() < 1e-6);
        stats.record_callback(f32::NAN);
        assert!((stats.cpu_load() - 0.9025).abs() < 1e-6);
        assert_eq!(stats.callbacks(), 3);
        stats.record_xrun();
        assert_eq!(stats.xruns(), 1);
    }

    #[test]
    fn stats_keep_the_peak_load_and_count_overruns() {
        let stats = StreamStats::new();
        assert_eq!(stats.take_peak_load(), 0.0);
        for load in [0.2, 0.7, 0.1, f32::NAN] {
            stats.record_callback(load);
        }
        assert_eq!(stats.overruns(), 0);
        assert_eq!(stats.take_peak_load(), 0.7);
        assert_eq!(stats.take_peak_load(), 0.0, "taken");
        stats.record_callback(1.5);
        stats.record_callback(1.0);
        assert_eq!(stats.overruns(), 1, "only above the whole period");
        assert_eq!(stats.take_peak_load(), 1.5);
    }

    #[test]
    fn null_host_drives_the_engine() {
        let (mut system, probe, _sender) = system(0.0);
        assert_eq!(system.state(), &AudioState::Stopped);
        system.start(&null(64)).unwrap();
        assert_eq!(
            system.state(),
            &AudioState::Running {
                config: null(64),
                latency: Duration::ZERO
            }
        );
        assert!(runs(&probe, 10));
        assert_eq!(
            probe.params(),
            StreamParams {
                sample_rate: 48_000,
                max_frames: 64,
                channels: 2,
                workgroup: None,
            }
        );
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 1);
        assert!(system.stats().callbacks() >= 10);
        assert!(probe.max_seen_frames.load(Ordering::SeqCst) <= 64);
        system.stop();
        assert_eq!(system.state(), &AudioState::Stopped);
        let calls = probe.calls();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(probe.calls(), calls, "no callbacks after stop");
    }

    #[test]
    fn rebuild_keeps_the_engine_object_and_its_state() {
        let (mut system, probe, _sender) = system(0.0);
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 5));
        let address = probe.address.load(Ordering::SeqCst);
        for buffer in [256, 128, 32] {
            let calls = probe.calls();
            system.start(&null(buffer)).unwrap();
            assert!(runs(&probe, 3));
            assert_eq!(probe.params().max_frames, buffer);
            assert_eq!(probe.address.load(Ordering::SeqCst), address);
            assert!(probe.calls() > calls);
        }
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 4);
        system.stop();
        let engine = system.detach(Duration::ZERO).unwrap();
        assert_eq!(&*engine as *const TestEngine as usize, address);
        // The engine's own state counted every callback of every stream.
        assert_eq!(engine.processed, probe.calls());
    }

    #[test]
    fn detach_and_attach_while_running() {
        let (mut system, probe, _sender) = system(0.0);
        system.start(&null(32)).unwrap();
        assert!(runs(&probe, 3));
        let engine = system.detach(WAIT).unwrap();
        assert_eq!(
            &*engine as *const TestEngine as usize,
            probe.address.load(Ordering::SeqCst)
        );
        assert_eq!(system.detach(WAIT).unwrap_err(), AudioError::Detached);
        // The stream keeps running (silence) without the engine.
        let (calls, callbacks) = (probe.calls(), system.stats().callbacks());
        assert!(wait_until(WAIT, || system.stats().callbacks() > callbacks + 3));
        assert_eq!(probe.calls(), calls);
        system.poll();
        assert_eq!(
            probe.calls(),
            calls,
            "poll does not re-attach a detached engine"
        );

        system.attach(engine);
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 2);
        assert!(runs(&probe, 3));
    }

    #[test]
    fn detach_without_stream_returns_the_engine_at_once() {
        let (mut system, probe, _sender) = system(0.0);
        let engine = system.detach(Duration::ZERO).unwrap();
        assert_eq!(
            system.detach(Duration::ZERO).unwrap_err(),
            AudioError::Detached
        );
        system.attach(engine);
        assert_eq!(
            probe.prepares.load(Ordering::SeqCst),
            0,
            "prepared at start"
        );
        system.start(&null(64)).unwrap();
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 1);
        assert!(runs(&probe, 3));
    }

    /// A host whose device lookup only enumerates (nothing is opened). Windows has none: the
    /// tests never enumerate the machine's WASAPI endpoints (rule 14), so they use ALSA, which is
    /// unavailable there and fails with `HostUnavailable`.
    fn test_host() -> HostKind {
        if cfg!(target_os = "macos") {
            HostKind::CoreAudio
        } else {
            HostKind::Alsa
        }
    }

    fn test_host_prefix() -> &'static str {
        if cfg!(target_os = "macos") {
            "coreaudio"
        } else {
            "alsa"
        }
    }

    #[test]
    fn missing_device_falls_back_to_the_null_backend() {
        let (mut system, probe, _sender) = system(0.0);
        let config = StreamConfig {
            host: test_host(),
            device: Some("komponist test: no such device".to_owned()),
            device_id: None,
            sample_rate: 44_100,
            buffer_frames: 128,
        };
        let err = system.start(&config).unwrap_err();
        assert!(
            matches!(
                err,
                AudioError::NoDevice(_) | AudioError::HostUnavailable(_)
            ),
            "{err:?}"
        );
        match system.state() {
            AudioState::NoDevice {
                config: failed,
                reason,
            } => {
                assert_eq!(failed, &config);
                assert_eq!(reason, &err.to_string());
            }
            other => panic!("{other:?}"),
        }
        // The engine runs on the null backend with the requested rate and buffer.
        assert!(runs(&probe, 3));
        assert_eq!(
            probe.params(),
            StreamParams {
                sample_rate: 44_100,
                max_frames: 128,
                channels: 2,
                workgroup: None,
            }
        );
    }

    #[test]
    fn invalid_config_is_rejected_and_the_engine_keeps_running() {
        let (mut system, probe, _sender) = system(0.0);
        let err = system.start(&null(0)).unwrap_err();
        assert!(matches!(err, AudioError::UnsupportedConfig(_)), "{err:?}");
        assert!(matches!(system.state(), AudioState::NoDevice { .. }));
        assert!(runs(&probe, 3));
        assert_eq!(
            probe.params().max_frames,
            StreamConfig::default().buffer_frames
        );
    }

    #[test]
    fn lost_device_moves_the_engine_to_the_null_backend() {
        let (mut system, probe, _sender) = system(0.0);
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 3));
        system.simulate_device_loss("device unplugged");
        system.poll();
        assert_eq!(
            system.state(),
            &AudioState::NoDevice {
                config: null(64),
                reason: "device unplugged".to_owned()
            }
        );
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 2);
        assert!(runs(&probe, 3));
        // A new start recovers.
        system.start(&null(64)).unwrap();
        assert!(matches!(system.state(), AudioState::Running { .. }));
    }

    #[test]
    fn live_midi_reaches_the_engine_through_the_null_stream() {
        let (mut system, probe, sender) = system(0.0);
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 2));
        assert!(sender.push(MidiPortId(5), [0x91, 62, 90]));
        assert!(wait_until(WAIT, || probe.events.load(Ordering::SeqCst) == 1));
        assert_eq!(probe.last_event_data.load(Ordering::SeqCst), 0x0091_3E5A);
        assert!(probe.last_event_frame.load(Ordering::SeqCst) < 64);
        // The receiver survives a rebuild.
        system.start(&null(128)).unwrap();
        assert!(sender.push(MidiPortId(5), [0x81, 62, 0]));
        assert!(wait_until(WAIT, || probe.events.load(Ordering::SeqCst) == 2));
    }

    #[test]
    fn runs_without_a_midi_backend() {
        let (engine, probe) = TestEngine::boxed(0.0);
        let mut system = AudioSystem::new(engine, MidiReceiver::disconnected());
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 5));
        assert_eq!(probe.events.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_lost_device_is_started_again_once_it_is_back() {
        let (mut system, probe, _sender) = system(0.0);
        system.set_retry_interval(Duration::from_millis(10));
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 3));
        let address = probe.address.load(Ordering::SeqCst);
        system.simulate_device_loss("device unplugged");
        system.poll();
        assert!(matches!(system.state(), AudioState::NoDevice { .. }));
        assert!(system.is_retrying());
        // The probe finds the (null) device and `poll` starts it again.
        assert!(wait_until(WAIT, || {
            system.poll();
            matches!(system.state(), AudioState::Running { .. })
        }));
        assert_eq!(
            system.state(),
            &AudioState::Running {
                config: null(64),
                latency: Duration::ZERO
            }
        );
        assert!(!system.is_retrying());
        assert!(runs(&probe, 3));
        assert_eq!(probe.address.load(Ordering::SeqCst), address);
        // start, null fallback, reconnect.
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn an_asio_device_is_looked_for_less_often() {
        let (mut system, _probe, _sender) = system(0.0);
        system.set_retry_interval(Duration::from_millis(10));
        system.arm_retry(null(64));
        assert_eq!(
            system.retry.as_ref().unwrap().interval,
            Duration::from_millis(10)
        );
        let asio = StreamConfig {
            host: HostKind::Asio,
            ..null(64)
        };
        system.arm_retry(asio);
        assert_eq!(system.retry.as_ref().unwrap().interval, ASIO_RETRY_INTERVAL);
    }

    #[test]
    fn capture_retry_probes_only_the_identified_source() {
        let original = DeviceId("pipewire:alsa_input.capture".into());
        let mut retry = Retry::new(original.clone(), Duration::ZERO);
        assert!(!retry.available("komponist-test-capture-probe", |target| {
            target.0 == "pipewire:alsa_input.different_capture"
        }));
        assert!(wait_until(WAIT, || retry
            .probe
            .as_ref()
            .unwrap()
            .is_finished()));
        assert!(!retry.available("komponist-test-capture-probe", |_| unreachable!()));
        assert!(
            !retry.available("komponist-test-capture-probe", move |target| {
                *target == original
            })
        );
        assert!(wait_until(WAIT, || retry
            .probe
            .as_ref()
            .unwrap()
            .is_finished()));
        assert!(retry.available("komponist-test-capture-probe", |_| unreachable!()));
    }

    #[test]
    fn unavailable_capture_does_not_stop_playback_or_arm_output_retry() {
        fn launch(
            system: &mut AudioSystem<TestEngine>,
            config: &StreamConfig,
        ) -> Result<(), AudioError> {
            system.start_with(config, |system, config| {
                let (_, input) = system.open_capture(|selected| {
                    if selected.device_id.is_none() {
                        return Err(AudioError::NoDevice("ambiguous test input".into()));
                    }
                    let (_, reader) = crate::test_input(2, 48_000, 256)?;
                    Ok(((), reader))
                });
                system.synthetic_input = input;
                system.start_null(config)?;
                Ok(config.clone())
            })
        }

        let (mut system, probe, _sender) = system(0.0);
        let config = StreamConfig {
            host: HostKind::PipeWire,
            ..null(64)
        };
        let legacy = crate::InputConfig {
            device: Some("Same label".into()),
            device_id: None,
        };
        system.set_input(Some(legacy.clone())).unwrap();
        launch(&mut system, &config).unwrap();
        assert_eq!(system.input(), Some(&legacy));
        assert_eq!(system.input_channels(), 0);
        assert!(matches!(
            system.input_error(),
            Some(AudioError::NoDevice(_))
        ));
        assert!(
            matches!(system.state(), AudioState::Running { config: active, .. } if active == &config)
        );
        assert!(!system.is_retrying());
        assert!(runs(&probe, 3));

        // A failed choice can be attempted again. A stable ID resolves the ambiguity.
        assert!(system.set_input_with(Some(legacy.clone()), launch).is_err());
        let source = crate::InputConfig {
            device_id: Some(DeviceId("pipewire:test-capture".into())),
            ..legacy
        };
        system.set_input_with(Some(source.clone()), launch).unwrap();
        assert_eq!(system.input(), Some(&source));
        assert_eq!(system.input_channels(), 2);
        assert!(system.input_error().is_none());
        assert!(
            matches!(system.state(), AudioState::Running { config: active, .. } if active == &config)
        );
        assert!(!system.is_retrying());
        assert!(runs(&probe, 3));
    }

    #[test]
    fn a_missing_device_keeps_being_looked_for() {
        let (mut system, probe, _sender) = system(0.0);
        system.set_retry_interval(Duration::from_millis(5));
        let config = StreamConfig {
            host: test_host(),
            device: Some("komponist test: no such device".to_owned()),
            device_id: Some(DeviceId(format!(
                "{}:komponist-test-missing",
                test_host_prefix()
            ))),
            sample_rate: 48_000,
            buffer_frames: 64,
        };
        assert!(system.start(&config).is_err());
        assert!(system.is_retrying());
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            system.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(matches!(system.state(), AudioState::NoDevice { .. }));
        assert!(system.is_retrying());
        assert!(
            runs(&probe, 3),
            "the engine keeps running on the null backend"
        );
        system.stop();
        assert!(!system.is_retrying());

        // An unsupported configuration is not retried.
        assert!(system.start(&null(0)).is_err());
        assert!(!system.is_retrying());
    }

    /// How long S07's enumeration takes on this machine (WP-52 note, WP-61): `available_hosts`
    /// (the JACK probe opens a client) and `devices` per host. Opens no stream, plays nothing.
    /// On Windows with the feature `asio`, `devices(Asio)` loads every installed ASIO driver in turn.
    ///
    /// ```text
    /// cargo test --release -p komponist-audio device_enumeration_time -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "talks to the machine's audio servers; run with --ignored"]
    fn device_enumeration_time() {
        for round in 0..3 {
            let t0 = Instant::now();
            let hosts = AudioSystem::<TestEngine>::available_hosts();
            let probe = t0.elapsed();
            let mut line = format!("round {round}: available_hosts {probe:.1?}");
            for host in hosts {
                let t0 = Instant::now();
                let n = AudioSystem::<TestEngine>::devices(host).map_or(0, |d| d.len());
                line += &format!(", {host:?} {:.1?} ({n} devices)", t0.elapsed());
            }
            eprintln!("{line}");
        }
    }

    #[test]
    fn realtime_denial_is_reported_per_stream() {
        let (mut system, _probe, _sender) = system(0.0);
        system.start(&null(64)).unwrap();
        assert!(!system.stats().realtime_denied());
        system.stats().record_realtime_denied();
        assert!(system.stats().realtime_denied());
        system.start(&null(128)).unwrap();
        assert!(
            !system.stats().realtime_denied(),
            "reset for the new stream"
        );
    }

    #[test]
    fn stream_configs_without_device_id_still_load() {
        // A driver of this system (PipeWire, CoreAudio or WASAPI): other systems' drivers migrate.
        let host = if cfg!(target_os = "macos") {
            "coreaudio"
        } else if cfg!(windows) {
            "wasapi"
        } else {
            "pipewire"
        };
        let old = format!(
            r#"{{"host":"{host}","device":"Scarlett 2i2","sample_rate":48000,"buffer_frames":256}}"#
        );
        let config: StreamConfig = serde_json::from_str(&old).unwrap();
        assert_eq!(config.device.as_deref(), Some("Scarlett 2i2"));
        assert_eq!(config.device_id, None);
        let with_id = StreamConfig {
            device_id: Some(DeviceId(format!("{host}:alsa_output.usb-x"))),
            ..config
        };
        let json = serde_json::to_string(&with_id).unwrap();
        assert!(
            json.contains(&format!(r#""device_id":"{host}:alsa_output.usb-x""#)),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<StreamConfig>(&json).unwrap(),
            with_id
        );
    }

    #[test]
    fn hosts_and_devices() {
        let hosts = AudioSystem::<TestEngine>::available_hosts();
        assert_eq!(hosts.last(), Some(&HostKind::Null));
        // ASIO is listed in builds with the feature when a driver is registered (listing creates
        // cpal's host and reads the registry; it loads no driver: W8b), after WASAPI so the
        // platform default stays first. CI runners have no ASIO driver.
        let asio = cfg!(all(windows, feature = "asio")) && crate::backend::asio_driver_registered();
        assert_eq!(hosts.contains(&HostKind::Asio), asio);
        if asio {
            let position = |kind| hosts.iter().position(|&h| h == kind);
            assert!(position(HostKind::Wasapi) < position(HostKind::Asio));
        }
        let null = AudioSystem::<TestEngine>::devices(HostKind::Null).unwrap();
        assert_eq!(null.len(), 1);
        assert!(null[0].is_default);
        assert_eq!(
            null[0].sample_rates,
            [44_100, 48_000, 88_200, 96_000, 192_000]
        );
        // Real devices depend on the machine: reported, not asserted. Not on Windows: WASAPI
        // enumeration would touch the machine's endpoints (rule 14).
        for &host in hosts.iter().filter(|_| !cfg!(windows)) {
            match AudioSystem::<TestEngine>::devices(host) {
                Ok(devices) => {
                    for d in devices {
                        println!(
                            "{host:?}: {:?} default={} {:?}",
                            d.name, d.is_default, d.sample_rates
                        );
                    }
                }
                Err(e) => println!("{host:?}: {e}"),
            }
        }
    }

    #[test]
    fn the_system_and_the_slot_can_move_between_threads() {
        fn send<T: Send>() {}
        send::<AudioSystem<TestEngine>>();
        send::<EngineSlot<TestEngine>>();
        send::<SlotHandle<TestEngine>>();
        send::<MidiReceiver>();
    }

    #[test]
    fn default_stream_config_matches_s07() {
        let config = StreamConfig::default();
        assert_eq!(config.host, HostKind::platform_default());
        assert!(config.host.is_supported_here());
        assert_eq!((config.sample_rate, config.buffer_frames), (48_000, 256));
    }

    #[test]
    fn a_driver_of_another_system_loads_as_the_platform_default() {
        let json = |host: &str| {
            format!(
                r#"{{"host":"{host}","device":"X","device_id":"{host}:x","sample_rate":44100,"buffer_frames":128}}"#
            )
        };
        for kind in [
            HostKind::CoreAudio,
            HostKind::Wasapi,
            HostKind::Asio,
            HostKind::PipeWire,
            HostKind::Jack,
            HostKind::Alsa,
            HostKind::Null,
        ] {
            let name = serde_json::to_string(&kind).unwrap();
            let name = name.trim_matches('"');
            let config: StreamConfig = serde_json::from_str(&json(name)).unwrap();
            let expected = if kind.is_supported_here() {
                kind
            } else {
                HostKind::platform_default()
            };
            assert_eq!(config.host, expected, "{name}");
            assert_eq!((config.sample_rate, config.buffer_frames), (44_100, 128));
            // A migrated configuration uses the system's default device.
            if kind.is_supported_here() {
                assert_eq!(config.device.as_deref(), Some("X"));
                assert!(config.device_id.is_some());
            } else {
                assert_eq!((config.device, config.device_id), (None, None));
            }
        }
        assert!(serde_json::from_str::<StreamConfig>(&json("oss")).is_err());
    }

    #[test]
    fn host_names_are_lowercase() {
        assert_eq!(
            serde_json::to_string(&HostKind::CoreAudio).unwrap(),
            r#""coreaudio""#
        );
        assert_eq!(
            serde_json::to_string(&HostKind::Wasapi).unwrap(),
            r#""wasapi""#
        );
        assert_eq!(serde_json::to_string(&HostKind::Asio).unwrap(), r#""asio""#);
    }
}
