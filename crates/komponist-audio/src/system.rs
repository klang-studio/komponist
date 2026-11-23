//! Audio hosts, devices and the stream lifecycle (ARCHITECTURE §7, R1, S07).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::StreamTrait;
use serde::{Deserialize, Serialize};

use crate::backend::{self, NullStream, StreamCallback, StreamShared};
use crate::input::{CaptureStream, CaptureTarget};
use crate::midi::MidiReceiver;
use crate::process::{AudioProcess, StreamParams};
use crate::slot::{DetachStep, EngineSlot, SlotHandle};
use crate::{JackCaptureChannel, JackRoutes, workgroup};

/// Weight of one callback's load in the smoothed CPU load (time constant ≈ 20 callbacks).
const LOAD_SMOOTHING: f32 = 0.05;

/// Histogram bins are percentage points of a buffer period. The last bin includes overflow.
const CALLBACK_TIMING_OVERFLOW: usize = 400;
type CallbackHistogram = [AtomicU64; CALLBACK_TIMING_OVERFLOW + 1];

/// How long a stream torn down by dropping the system may take to hand the engine back. A
/// teardown during a stream change does not wait: `poll` collects a late engine (#221).
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
    /// Explicit JACK channel routing, used only with the JACK host.
    #[serde(skip_serializing_if = "JackRoutes::is_automatic")]
    pub jack_routes: JackRoutes,
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
    #[serde(default)]
    jack_routes: JackRoutes,
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
            jack_routes: stored.jack_routes,
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
            jack_routes: JackRoutes::default(),
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
    /// Overruns the capture device reported (frames lost before they reached komponist), since
    /// the system was created. Each one marks a discontinuity for recording.
    input_xruns: AtomicU32,
    /// Live MIDI messages lost because the queue from the MIDI threads was full.
    midi_overflows: AtomicU64,
    /// Live MIDI messages discarded as stale: queued while no engine ran, or older than one
    /// period when a stream took the MIDI receiver over.
    midi_stale: AtomicU64,
    /// Callbacks run, since the system was created.
    callbacks: AtomicU64,
    /// Highest load of a single callback since the last [`StreamStats::take_peak_load`], as `f32`
    /// bits (non-negative floats order like their bits).
    peak_load: AtomicU32,
    /// Allocated on the control thread when a callback benchmark requests timing percentiles.
    callback_timing: OnceLock<Box<CallbackHistogram>>,
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

    /// Capture-device overruns so far (the output's are [`StreamStats::xruns`]).
    pub fn input_xruns(&self) -> u32 {
        self.input_xruns.load(Ordering::Relaxed)
    }

    /// Live MIDI messages lost so far because their queue was full. Each loss releases the held
    /// notes, so a lost note-off cannot leave a note hanging.
    pub fn midi_overflows(&self) -> u64 {
        self.midi_overflows.load(Ordering::Relaxed)
    }

    /// Live MIDI messages discarded so far as stale (see [`crate::MidiIn`]).
    pub fn midi_stale(&self) -> u64 {
        self.midi_stale.load(Ordering::Relaxed)
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

    /// Starts collecting individual loads in one-percentage-point buckets for benchmarking.
    /// Call from the control thread. Starting another measurement clears the previous buckets.
    pub fn start_callback_timing(&self) {
        let buckets = self
            .callback_timing
            .get_or_init(|| Box::new(std::array::from_fn(|_| AtomicU64::new(0))));
        for count in buckets.iter() {
            count.store(0, Ordering::Relaxed);
        }
    }

    /// Approximate p95 and p99 of individual callback loads, as whole percentages of the
    /// buffer period. A value of 400 includes all loads at or above 400%.
    pub fn callback_percentiles(&self) -> Option<[usize; 2]> {
        let buckets = self.callback_timing.get()?;
        let counts: [u64; CALLBACK_TIMING_OVERFLOW + 1] =
            std::array::from_fn(|bucket| buckets[bucket].load(Ordering::Relaxed));
        let total = counts.iter().sum::<u64>();
        if total == 0 {
            return None;
        }
        Some([95, 99].map(|percentile| {
            let rank = (total * percentile).div_ceil(100);
            let mut seen = 0;
            counts
                .iter()
                .position(|count| {
                    seen += count;
                    seen >= rank
                })
                .expect("nonempty histogram contains its percentile")
        }))
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
        if let Some(buckets) = self.callback_timing.get() {
            let bucket = ((load * 100.0) as usize).min(CALLBACK_TIMING_OVERFLOW);
            buckets[bucket].fetch_add(1, Ordering::Relaxed);
        }
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

    /// RT (capture error callback). Counts one capture-device overrun.
    pub(crate) fn record_input_xrun(&self) {
        self.input_xruns.fetch_add(1, Ordering::Relaxed);
    }

    /// RT. Counts live MIDI messages lost to a full queue.
    pub(crate) fn record_midi_overflows(&self, messages: u64) {
        self.midi_overflows.fetch_add(messages, Ordering::Relaxed);
    }

    /// RT. Counts live MIDI messages discarded as stale.
    pub(crate) fn record_midi_stale(&self, messages: u64) {
        self.midi_stale.fetch_add(messages, Ordering::Relaxed);
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
    /// An engine that came back poisoned ([`AudioProcess::is_poisoned`]), for the owner
    /// ([`AudioSystem::take_poisoned`]); never handed to a stream.
    poisoned: Option<Box<P>>,
    /// The MIDI receiver while no stream holds it.
    midi: Option<MidiReceiver>,
    /// The engine is out (detach) until `attach`.
    detached: bool,
    current: Option<Current<P>>,
    /// Handles of torn-down streams that have not handed back yet.
    retired: Vec<SlotHandle<P>>,
    /// Reconnection while the configured device is missing.
    retry: Option<OutputRetry<P>>,
    retry_interval: Duration,
    input: Option<crate::InputConfig>,
    input_error: Option<AudioError>,
    input_channels: u16,
    synthetic_input: Option<crate::InputReader>,
    requested_jack_capture: Option<Vec<JackCaptureChannel>>,
    /// Tests: replaces what a reconnection worker does (probe and open).
    #[cfg(test)]
    fake_reopen: Option<FakeReopen<P>>,
}

#[cfg(test)]
type FakeReopen<P> = Arc<dyn Fn(&StreamConfig, &ReopenContext) -> Reopen<P> + Send + Sync>;

/// Runs at most one device operation off the control thread and retries at a bounded cadence.
struct Retry<T, R> {
    target: T,
    /// Time between two probes.
    interval: Duration,
    /// When the next probe starts.
    next: Instant,
    worker: Option<JoinHandle<R>>,
}

impl<T: Clone + Send + 'static, R: Send + 'static> Retry<T, R> {
    fn new(target: T, interval: Duration) -> Self {
        Self {
            target,
            interval,
            next: Instant::now() + interval,
            worker: None,
        }
    }

    fn poll(&mut self, thread_name: &str, run: impl FnOnce(&T) -> R + Send + 'static) -> Option<R> {
        match self.worker.take() {
            Some(worker) if worker.is_finished() => {
                self.next = Instant::now() + self.interval;
                return worker.join().ok();
            }
            Some(worker) => self.worker = Some(worker),
            None if Instant::now() >= self.next => {
                let target = self.target.clone();
                self.worker = thread::Builder::new()
                    .name(thread_name.to_owned())
                    .spawn(move || run(&target))
                    .ok();
                if self.worker.is_none() {
                    self.next = Instant::now() + self.interval;
                }
            }
            None => {}
        }
        None
    }
}

type CaptureOpenResult = Result<(CaptureStream, crate::InputReader), AudioError>;

/// Owned by one output generation. Cancelling it prevents a delayed open from starting capture.
struct CaptureRetry {
    open: Retry<CaptureTarget, CaptureOpenResult>,
    current: Arc<AtomicBool>,
}

impl CaptureRetry {
    fn for_output(
        host: HostKind,
        target: Option<CaptureTarget>,
        interval: Duration,
    ) -> Option<Self> {
        // CPAL's ASIO input and output share a driver buffer set. Opening input while output
        // is active can replace that set even on failure. Reconfigure the two streams together
        // in start_device, without restarting a healthy output in response to a capture error.
        if host == HostKind::Asio {
            return None;
        }
        Some(Self::new(target?, interval))
    }

    fn new(target: CaptureTarget, interval: Duration) -> Self {
        Self {
            open: Retry::new(target, interval),
            current: Arc::new(AtomicBool::new(true)),
        }
    }

    fn poll(
        &mut self,
        open: impl FnOnce(&CaptureTarget, &AtomicBool) -> CaptureOpenResult + Send + 'static,
    ) -> Option<CaptureOpenResult> {
        let current = Arc::clone(&self.current);
        self.open.poll("komponist-capture-reopen", move |target| {
            open(target, &current)
        })
    }
}

impl Drop for CaptureRetry {
    fn drop(&mut self) {
        self.current.store(false, Ordering::Release);
    }
}

/// A stream opened off the control thread: the control thread only installs it (#221).
struct Opened<P: AudioProcess> {
    current: Current<P>,
    /// The configuration the stream runs with.
    effective: StreamConfig,
    input_channels: u16,
    input_error: Option<AudioError>,
}

/// What opening the capture input next to an output gave.
struct OpenedInput<C> {
    capture: Option<C>,
    reader: Option<crate::InputReader>,
    channels: u16,
    error: Option<AudioError>,
}

/// Opens capture independently of playback. A failed input never vetoes a working output.
fn open_input<C>(
    input: Option<&crate::InputConfig>,
    open: impl FnOnce(&crate::InputConfig) -> Result<(C, crate::InputReader), AudioError>,
) -> OpenedInput<C> {
    match input.map(open) {
        Some(Ok((capture, reader))) => OpenedInput {
            channels: reader.channels() as u16,
            capture: Some(capture),
            reader: Some(reader),
            error: None,
        },
        Some(Err(error)) => OpenedInput {
            capture: None,
            reader: None,
            channels: 0,
            error: Some(error),
        },
        None => OpenedInput {
            capture: None,
            reader: None,
            channels: 0,
            error: None,
        },
    }
}

/// What a reconnection worker found.
enum Reopen<P: AudioProcess> {
    /// The device is still missing.
    Missing,
    /// The device is back; the control thread opens it ([`opens_on_control_thread`]).
    Present,
    /// The device is back and the worker opened its stream (or failed to).
    Opened(Result<Box<Opened<P>>, AudioError>),
}

/// What a reconnection worker needs to open the stream on its own.
#[derive(Clone)]
struct ReopenContext {
    input: Option<crate::InputConfig>,
    stats: Arc<StreamStats>,
    retry_interval: Duration,
}

/// Looks for a missing output device and, once it is back, opens its stream on the worker, as
/// capture reconnection does, so the control thread never waits for a device (#221). Dropping it
/// supersedes a running worker: that worker opens nothing more and drops what it opened.
struct OutputRetry<P: AudioProcess> {
    probe: Retry<StreamConfig, Reopen<P>>,
    current: Arc<AtomicBool>,
}

impl<P: AudioProcess> OutputRetry<P> {
    /// Supersedes the worker and waits for it, so it holds no device when another stream opens.
    fn cancel(mut self) {
        self.current.store(false, Ordering::Release);
        if let Some(worker) = self.probe.worker.take() {
            let _ = worker.join();
        }
    }
}

impl<P: AudioProcess> Drop for OutputRetry<P> {
    fn drop(&mut self) {
        self.current.store(false, Ordering::Release);
    }
}

/// Whether the configured output is there (the probe of a reconnection).
fn output_present(config: &StreamConfig, has_input: bool) -> bool {
    #[cfg(target_os = "linux")]
    if native_jack_route(config, has_input) && !uses_saved_jack_output(config) {
        return crate::jack_backend::server_present();
    }
    #[cfg(not(target_os = "linux"))]
    let _ = has_input;
    backend::device_present(config)
}

/// Outputs that a reconnection opens on the control thread: an ASIO driver is loaded and counted
/// there ([`backend::AsioOpen`]), the native JACK client keeps its graph work there, and the null
/// backend opens at once.
fn opens_on_control_thread(config: &StreamConfig, has_input: bool) -> bool {
    #[cfg(target_os = "linux")]
    if native_jack_route(config, has_input) {
        return true;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = has_input;
    matches!(config.host, HostKind::Asio | HostKind::Null)
}

/// The reconnection worker: probes for the device and opens its stream unless superseded.
fn reopen<P: AudioProcess>(
    config: &StreamConfig,
    context: &ReopenContext,
    current: &AtomicBool,
) -> Reopen<P> {
    let has_input = context.input.is_some();
    if !output_present(config, has_input) {
        return Reopen::Missing;
    }
    if opens_on_control_thread(config, has_input) {
        return Reopen::Present;
    }
    if !current.load(Ordering::Acquire) {
        return Reopen::Missing;
    }
    let opened = open_device(
        config,
        context.input.as_ref(),
        &context.stats,
        context.retry_interval,
    );
    if !current.load(Ordering::Acquire) {
        // Superseded while opening: the stream closes here, it never held the engine.
        return Reopen::Missing;
    }
    Reopen::Opened(opened.map(Box::new))
}

/// Opens a cpal stream for `config`, with capture if `input` asks for it. Touches nothing of the
/// system, so it runs on the control thread (`start`) or on a reconnection worker.
fn open_device<P: AudioProcess>(
    config: &StreamConfig,
    input: Option<&crate::InputConfig>,
    stats: &Arc<StreamStats>,
    retry_interval: Duration,
) -> Result<Opened<P>, AudioError> {
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
    let (slot, handle) = EngineSlot::empty(Arc::clone(stats));
    let shared = Arc::new(StreamShared::default());
    let callback_params = StreamParams {
        workgroup: None,
        ..params.clone()
    };
    let capture_stats = Arc::clone(stats);
    let opened_input = open_input(input, |input| {
        CaptureStream::start(
            config.host,
            input,
            negotiated.effective.sample_rate,
            negotiated.effective.buffer_frames,
            Some(&device),
            capture_stats,
        )
    });
    let target = input
        .zip(opened_input.error.as_ref())
        .and_then(|(input, error)| CaptureTarget::missing_at_start(input, error));
    let capture_retry = CaptureRetry::for_output(config.host, target, retry_interval);
    let mut callback = StreamCallback::new(slot, callback_params, Arc::clone(&shared))
        .with_input(opened_input.reader);
    let capture_transfer = callback.enable_capture_reconnect();
    // Before the stream exists, so no enumeration sees an ASIO driver that is loading.
    let open = (config.host == HostKind::Asio).then(|| backend::AsioOpen::new(&device));
    let stream = backend::build_stream(&device, &negotiated, callback, Arc::clone(stats))?;
    stream.play().map_err(backend::map_err)?;
    Ok(Opened {
        current: Current {
            backend: Backend::Device(Box::new(DeviceStreams {
                capture_retry,
                _stream: stream,
                capture: opened_input.capture,
                capture_transfer,
                _open: open,
            })),
            handle,
            params,
            shared,
            requested: config.clone(),
        },
        effective: negotiated.effective,
        input_channels: opened_input.channels,
        input_error: opened_input.error,
    })
}

/// Opens the null backend with `config`'s rate and buffer size (stereo).
fn open_null<P: AudioProcess>(
    config: &StreamConfig,
    reader: Option<crate::InputReader>,
    stats: &Arc<StreamStats>,
) -> Result<Opened<P>, AudioError> {
    backend::validate(config)?;
    let params = StreamParams {
        sample_rate: config.sample_rate,
        max_frames: config.buffer_frames,
        channels: 2,
        workgroup: None,
    };
    let (slot, handle) = EngineSlot::empty(Arc::clone(stats));
    let shared = Arc::new(StreamShared::default());
    let input_channels = reader.as_ref().map_or(0, |r| r.channels() as u16);
    let callback =
        StreamCallback::new(slot, params.clone(), Arc::clone(&shared)).with_input(reader);
    let stream = NullStream::start(callback)?;
    Ok(Opened {
        current: Current {
            backend: Backend::Null {
                _stream: stream,
                #[cfg(not(target_os = "linux"))]
                _processor: std::marker::PhantomData,
            },
            handle,
            params,
            shared,
            requested: config.clone(),
        },
        effective: config.clone(),
        input_channels,
        input_error: None,
    })
}

fn check_current_capture(current: &AtomicBool) -> Result<(), AudioError> {
    if current.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(AudioError::NoDevice("capture reconnect superseded".into()))
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
    backend: Backend<P>,
    handle: SlotHandle<P>,
    params: StreamParams,
    shared: Arc<StreamShared>,
    /// The configuration `start` was called with.
    requested: StreamConfig,
}

/// Held for its `Drop`, which stops the stream.
enum Backend<P: AudioProcess> {
    // CPAL streams are large on Windows; allocate their storage only when starting a device.
    Device(Box<DeviceStreams>),
    Null {
        _stream: NullStream,
        #[cfg(not(target_os = "linux"))]
        _processor: std::marker::PhantomData<P>,
    },
    #[cfg(target_os = "linux")]
    Jack(Box<crate::jack_backend::JackStream<P>>),
}

struct DeviceStreams {
    // Cancel a pending reopen before the output and its capture handoff are torn down.
    capture_retry: Option<CaptureRetry>,
    _stream: cpal::Stream,
    capture: Option<CaptureStream>,
    capture_transfer: crate::input::CaptureTransfer,
    /// Declared after the streams: the open ASIO count drops only once they are gone.
    _open: Option<backend::AsioOpen>,
}

impl DeviceStreams {
    fn poll_capture(
        &mut self,
        host: HostKind,
        params: &StreamParams,
        interval: Duration,
        stats: &Arc<StreamStats>,
    ) -> Option<Result<u16, AudioError>> {
        let transfer_ready = self.capture_transfer.ready();
        if let Some(lost) = self.capture.take_if(|capture| capture.lost()) {
            self.capture_retry = CaptureRetry::for_output(host, lost.target().cloned(), interval);
            return Some(Err(AudioError::NoDevice("capture input lost".into())));
        }
        let retry = self.capture_retry.as_mut()?;
        if !transfer_ready {
            return None;
        }

        let rate = params.sample_rate;
        let frames = params.max_frames;
        let stats = Arc::clone(stats);
        let result = retry.poll(move |target, current| {
            check_current_capture(current)?;
            let (capture, reader) = CaptureStream::prepare(
                host,
                &target.selection(),
                rate,
                frames,
                None,
                Arc::clone(&stats),
            )?;
            check_current_capture(current)?;
            capture.play()?;
            check_current_capture(current)?;
            Ok((capture, reader))
        })?;
        match result {
            Ok((capture, reader)) => {
                let channels = reader.channels() as u16;
                if let Err(error) = self.capture_transfer.install(reader, frames) {
                    return Some(Err(error));
                }
                // Only the running output generation can claim a worker and enable its input.
                capture.enable();
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
            poisoned: None,
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
            requested_jack_capture: None,
            #[cfg(test)]
            fake_reopen: None,
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
            // The preference for automatic capture takes effect when the explicit route is removed.
            if config.host == HostKind::Jack && config.jack_routes.capture.is_some() {
                return Ok(());
            }
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
        #[cfg(target_os = "linux")]
        if let Some(Current {
            backend: Backend::Jack(jack),
            ..
        }) = &self.current
        {
            return jack.channels();
        }
        self.input_channels
    }

    /// Whether all requested slots can supply audio, without compacting missing JACK slots.
    pub fn input_route_available(&self, first: u16, count: u16) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(Current {
            backend: Backend::Jack(jack),
            ..
        }) = &self.current
        {
            return jack.input_route_available(first, count);
        }
        first
            .checked_add(count)
            .is_some_and(|end| count != 0 && end <= self.input_channels())
    }

    /// Verified active JACK playback links in master left/right order.
    pub fn jack_playback_connections(&self) -> Option<[bool; 2]> {
        #[cfg(target_os = "linux")]
        if let Some(Current {
            backend: Backend::Jack(jack),
            ..
        }) = &self.current
        {
            return Some(jack.playback_connections());
        }
        None
    }

    /// Actual JACK capture sources, ordered by input channel. A selected port keeps its slot
    /// even when unavailable. None means this backend has no active JACK capture configuration.
    pub fn jack_capture_sources(&self) -> Option<&[JackCaptureChannel]> {
        #[cfg(target_os = "linux")]
        if let Some(Current {
            backend: Backend::Jack(jack),
            ..
        }) = &self.current
            && (self.requested_jack_capture.is_some() || self.input.is_some())
        {
            return Some(jack.capture_sources());
        }
        if self.input.is_some()
            && matches!(&self.state, AudioState::NoDevice { config, .. } if config.host == HostKind::Jack)
        {
            // Preserve explicitly configured names when playback fell back to the null stream.
            return Some(self.requested_jack_capture.as_deref().unwrap_or_default());
        }
        self.requested_jack_capture.as_deref()
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
            #[cfg(target_os = "linux")]
            HostKind::Jack if native_jack_route(config, system.input.is_some()) => {
                system.start_jack(config, None)
            }
            _ => system.start_device(config),
        })
    }

    fn start_with(
        &mut self,
        config: &StreamConfig,
        start_output: impl FnOnce(&mut Self, &StreamConfig) -> Result<StreamConfig, AudioError>,
    ) -> Result<(), AudioError> {
        self.cancel_retry();
        self.input_error = None;
        self.teardown();
        self.requested_jack_capture = requested_jack_capture(config);
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
        self.cancel_retry();
        self.input_error = None;
        self.teardown();
        self.requested_jack_capture = None;
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
    /// looks for the device about every 2 s. The lookup runs on a short-lived thread, which also
    /// opens the device's stream once it is back, so the UI thread waits neither for the
    /// enumeration nor for the stream (#221; ASIO, the native JACK client and the null backend
    /// open on this thread, like [`AudioSystem::start`]). `poll` installs the finished stream and
    /// the state returns to `Running` with the same configuration. That transition is the report
    /// that the device is back.
    pub fn poll(&mut self) {
        self.collect_retired();
        let mut returned = None;
        let mut failure = None;
        #[cfg(target_os = "linux")]
        let mut restart = None;
        if let Some(current) = self.current.as_mut() {
            returned = current.handle.try_take_engine();
            let reason = current.shared.lost().or_else(|| {
                // The callback's thread ended on its own (it panicked).
                current
                    .handle
                    .is_closed()
                    .then(|| "audio stream stopped".to_owned())
            });
            #[cfg(target_os = "linux")]
            let reason = if let Backend::Jack(jack) = &mut current.backend {
                if let Some(rate) = jack.active_rate()
                    && rate != current.params.sample_rate
                {
                    restart = Some((current.requested.clone(), rate));
                }
                if restart.is_some() || jack.poll() {
                    reason
                } else {
                    Some("JACK server stopped".to_owned())
                }
            } else {
                reason
            };
            match reason {
                Some(reason) => failure = Some((current.requested.clone(), reason)),
                None => {
                    if let Backend::Device(streams) = &mut current.backend
                        && let Some(capture) = streams.poll_capture(
                            current.requested.host,
                            &current.params,
                            self.retry_interval,
                            &self.stats,
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
                    if let AudioState::Running {
                        config: _config,
                        latency,
                    } = &mut self.state
                    {
                        *latency = current.shared.latency();
                        #[cfg(target_os = "linux")]
                        if let Backend::Jack(jack) = &current.backend {
                            _config.buffer_frames = jack.period();
                            *latency = jack.latency(current.params.sample_rate);
                        }
                    }
                }
            }
        }
        if let Some(engine) = returned {
            self.keep_engine(engine);
        }
        #[cfg(target_os = "linux")]
        if let Some((config, rate)) = restart {
            let _ = self.start_with(&config, |system, config| {
                system.start_jack(config, Some(rate))
            });
            return;
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
        let opened = open_device(
            config,
            self.input.as_ref(),
            &self.stats,
            self.retry_interval,
        )?;
        Ok(self.install_opened(opened))
    }

    /// Installs a stream opened by [`open_device`]; returns its effective configuration.
    fn install_opened(&mut self, opened: Opened<P>) -> StreamConfig {
        self.input_channels = opened.input_channels;
        self.input_error = opened.input_error;
        self.install(opened.current);
        opened.effective
    }

    /// Explicit JACK routes share one server clock and one process callback.
    #[cfg(target_os = "linux")]
    fn start_jack(
        &mut self,
        config: &StreamConfig,
        active_rate: Option<u32>,
    ) -> Result<StreamConfig, AudioError> {
        use crate::jack_backend::JackStream;

        if uses_saved_jack_output(config) {
            backend::find_device(&backend::host(HostKind::Jack)?, config)?;
        }
        let (client, params, inactive_rate) = JackStream::<P>::open(active_rate)?;
        let automatic_capture = if config.jack_routes.capture.is_some() {
            false
        } else {
            match jack_capture_enabled(self.input.as_ref(), || crate::input_devices(HostKind::Jack))
            {
                Ok(enabled) => enabled,
                Err(error) => {
                    self.input_error = Some(error);
                    false
                }
            }
        };
        let (slot, handle) = EngineSlot::empty(Arc::clone(&self.stats));
        let jack = JackStream::start(
            client,
            &params,
            inactive_rate,
            &config.jack_routes,
            automatic_capture,
            slot,
            Arc::clone(&self.stats),
        )?;
        let effective = StreamConfig {
            sample_rate: params.sample_rate,
            buffer_frames: params.max_frames,
            ..config.clone()
        };
        self.install(Current {
            backend: Backend::Jack(Box::new(jack)),
            handle,
            params,
            shared: Arc::new(StreamShared::default()),
            requested: config.clone(),
        });
        Ok(effective)
    }

    /// [`open_input`] for `self.input`, noting the channels or the error in the system.
    #[cfg(test)]
    fn open_capture<C>(
        &mut self,
        open: impl FnOnce(&crate::InputConfig) -> Result<(C, crate::InputReader), AudioError>,
    ) -> (Option<C>, Option<crate::InputReader>) {
        let opened = open_input(self.input.as_ref(), open);
        if opened.error.is_some() {
            self.input_error = opened.error;
        } else if opened.capture.is_some() {
            self.input_channels = opened.channels;
        }
        (opened.capture, opened.reader)
    }

    /// Starts the null backend with `config`'s rate and buffer size (stereo).
    fn start_null(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        backend::validate(config)?;
        let reader = self.synthetic_input.take();
        let opened = open_null(config, reader, &self.stats)?;
        self.input_channels = opened.input_channels;
        self.install(opened.current);
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
        #[cfg(target_os = "linux")]
        if let Backend::Jack(jack) = &current.backend
            && jack.active_rate() != Some(current.params.sample_rate)
        {
            return;
        }
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
        // Every backend drops its callback (and with it the slot) before this returns. A handle
        // that has not closed yet is not waited for here: `poll` collects it (#221).
        drop(backend);
        self.stats.reset_realtime_denied();
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
    /// a `detach`) is dropped here, on the UI thread. A poisoned engine (one whose callback
    /// unwound, #210) is not kept for the next stream: it waits for
    /// [`AudioSystem::take_poisoned`], and the streams output silence until the owner attaches a
    /// new one.
    fn keep_engine(&mut self, engine: Box<P>) {
        if engine.is_poisoned() {
            self.poisoned = Some(engine);
        } else if self.engine.is_none() {
            self.engine = Some(engine);
        }
    }

    /// UI thread, after [`AudioSystem::poll`]: the engine that came back from a stream (or an
    /// [`AudioSystem::attach`]) poisoned, i.e. whose `process` unwound in a panic
    /// ([`AudioProcess::is_poisoned`]). The system holds no engine then; the owner drops this one
    /// and attaches a new one, built from scratch.
    pub fn take_poisoned(&mut self) -> Option<Box<P>> {
        self.poisoned.take()
    }

    /// Starts probing for a missing output device.
    fn arm_retry(&mut self, config: StreamConfig) {
        self.cancel_retry();
        let interval = retry_interval(config.host, self.retry_interval);
        self.retry = Some(OutputRetry {
            probe: Retry::new(config, interval),
            current: Arc::new(AtomicBool::new(true)),
        });
    }

    /// Ends the reconnection, waiting for a worker still opening the device.
    fn cancel_retry(&mut self) {
        if let Some(retry) = self.retry.take() {
            retry.cancel();
        }
    }

    /// Installs the output once the reconnection worker has it open, or starts it here for the
    /// outputs that open on this thread. Capture reconnection runs independently.
    fn poll_retry(&mut self) {
        let context = ReopenContext {
            input: self.input.clone(),
            stats: Arc::clone(&self.stats),
            retry_interval: self.retry_interval,
        };
        #[cfg(test)]
        let fake = self.fake_reopen.clone();
        let Some(retry) = self.retry.as_mut() else {
            return;
        };
        let current = Arc::clone(&retry.current);
        let found = retry.probe.poll("komponist-audio-reopen", move |config| {
            #[cfg(test)]
            if let Some(fake) = fake {
                return fake(config, &context);
            }
            reopen(config, &context, &current)
        });
        let config = retry.probe.target.clone();
        match found {
            None | Some(Reopen::Missing) | Some(Reopen::Opened(Err(_))) => {}
            Some(Reopen::Present) => {
                let _ = self.start(&config);
            }
            Some(Reopen::Opened(Ok(opened))) => {
                self.retry = None;
                self.input_error = None;
                self.teardown();
                self.requested_jack_capture = requested_jack_capture(&config);
                let effective = self.install_opened(*opened);
                self.state = AudioState::Running {
                    config: effective,
                    latency: Duration::ZERO,
                };
            }
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
    fn set_fake_reopen(
        &mut self,
        fake: impl Fn(&StreamConfig, &ReopenContext) -> Reopen<P> + Send + Sync + 'static,
    ) {
        self.fake_reopen = Some(Arc::new(fake));
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
        self.cancel_retry();
        self.teardown();
        let deadline = Instant::now() + TEARDOWN_TIMEOUT;
        while !self.retired.is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
            self.collect_retired();
        }
    }
}

/// The JACK capture ports a configuration names, all missing until the stream reports them.
fn requested_jack_capture(config: &StreamConfig) -> Option<Vec<JackCaptureChannel>> {
    if config.host != HostKind::Jack {
        return None;
    }
    config.jack_routes.capture.as_ref().map(|capture| {
        capture
            .iter()
            .map(|name| JackCaptureChannel::missing(name))
            .collect()
    })
}

#[cfg(target_os = "linux")]
fn native_jack_route(config: &StreamConfig, has_input: bool) -> bool {
    config.host == HostKind::Jack && (!config.jack_routes.is_automatic() || has_input)
}

#[cfg(target_os = "linux")]
fn uses_saved_jack_output(config: &StreamConfig) -> bool {
    config.jack_routes.playback.iter().all(Option::is_none)
        && (config.device.is_some() || config.device_id.is_some())
}

#[cfg(target_os = "linux")]
fn jack_capture_enabled(
    input: Option<&crate::InputConfig>,
    devices: impl FnOnce() -> Result<Vec<crate::InputDeviceInfo>, AudioError>,
) -> Result<bool, AudioError> {
    let Some(input) = input else { return Ok(false) };
    if input == &crate::InputConfig::default() || input.device_index(&devices()?).is_some() {
        return Ok(true);
    }
    let name = input
        .device
        .clone()
        .or_else(|| input.device_id.as_ref().map(|id| id.0.clone()));
    Err(AudioError::NoDevice(name.unwrap_or_default()))
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
        jack_routes: Default::default(),
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
            jack_routes: Default::default(),
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
    fn callback_timing_uses_unsmoothed_load_and_can_restart() {
        let stats = StreamStats::new();
        stats.record_callback(20.0);
        assert_eq!(stats.callback_percentiles(), None);
        stats.start_callback_timing();
        for _ in 0..94 {
            stats.record_callback(0.25);
        }
        for _ in 0..4 {
            stats.record_callback(1.5);
        }
        for _ in 0..2 {
            stats.record_callback(5.0);
        }
        assert_eq!(stats.callback_percentiles(), Some([150, 400]));
        stats.start_callback_timing();
        assert_eq!(stats.callback_percentiles(), None);
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

    /// An engine that comes back poisoned from a stream (its callback unwound, #210) is not
    /// handed to the next stream or to a detach: the owner takes it and attaches a new one.
    #[test]
    fn a_poisoned_engine_is_not_reused() {
        let (mut system, probe, _sender) = system(0.0);
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 3));
        assert!(system.take_poisoned().is_none());
        probe.poisoned.store(true, Ordering::SeqCst);
        // A rebuild takes the engine out of the old stream; the new one runs without it.
        system.start(&null(128)).unwrap();
        let (calls, callbacks) = (probe.calls(), system.stats().callbacks());
        assert!(wait_until(WAIT, || system.stats().callbacks() > callbacks + 3));
        system.poll();
        assert_eq!(probe.calls(), calls, "the poisoned engine runs no more");
        assert_eq!(probe.prepares.load(Ordering::SeqCst), 1);
        assert_eq!(
            system.detach(Duration::ZERO).unwrap_err(),
            AudioError::Detached
        );
        let poisoned = system.take_poisoned().expect("kept apart for the owner");
        assert_eq!(
            &*poisoned as *const TestEngine as usize,
            probe.address.load(Ordering::SeqCst)
        );
        assert!(system.take_poisoned().is_none(), "taken once");
        // An attach of a poisoned engine is refused the same way.
        system.attach(poisoned);
        assert!(system.take_poisoned().is_some());
        // A new engine takes over the running stream.
        let (fresh, fresh_probe) = TestEngine::boxed(0.0);
        system.attach(fresh);
        assert_eq!(fresh_probe.params().max_frames, 128);
        assert!(runs(&fresh_probe, 3));
        assert_eq!(probe.calls(), calls);
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
            jack_routes: Default::default(),
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

    /// A blocking reconnection for the tests below: signals `began`, waits for `release` (at most
    /// `WAIT`), then opens a null stream.
    fn blocking_reopen(
        system: &mut AudioSystem<TestEngine>,
    ) -> (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = std::sync::Mutex::new(gate);
        let (began, opening) = std::sync::mpsc::channel();
        system.set_fake_reopen(move |config, context| {
            let _ = began.send(());
            let _ = gate.lock().unwrap().recv_timeout(WAIT);
            Reopen::Opened(open_null(config, None, &context.stats).map(Box::new))
        });
        (release, opening)
    }

    /// #221: a reconnection opens the stream on its worker; `poll` never waits for it and
    /// installs the finished stream.
    #[test]
    fn a_slow_reopen_does_not_block_poll() {
        let (mut system, probe, _sender) = system(0.0);
        system.set_retry_interval(Duration::from_millis(5));
        system.start(&null(64)).unwrap();
        assert!(runs(&probe, 3));
        let address = probe.address.load(Ordering::SeqCst);
        let (release, opening) = blocking_reopen(&mut system);
        system.simulate_device_loss("device unplugged");
        system.poll();
        assert!(matches!(system.state(), AudioState::NoDevice { .. }));
        assert!(wait_until(WAIT, || {
            system.poll();
            opening.try_recv().is_ok()
        }));
        for _ in 0..20 {
            let started = Instant::now();
            system.poll();
            assert!(
                started.elapsed() < Duration::from_millis(100),
                "poll waited for the device"
            );
            assert!(matches!(system.state(), AudioState::NoDevice { .. }));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            runs(&probe, 3),
            "the engine runs on the null fallback meanwhile"
        );
        release.send(()).unwrap();
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

    /// A `start` while a reconnection worker is opening the device waits for that worker, and
    /// its stream is never installed.
    #[test]
    fn a_superseded_reopen_is_never_installed() {
        let (mut system, probe, _sender) = system(0.0);
        system.set_retry_interval(Duration::from_millis(5));
        system.start(&null(64)).unwrap();
        let (release, opening) = blocking_reopen(&mut system);
        system.simulate_device_loss("device unplugged");
        assert!(wait_until(WAIT, || {
            system.poll();
            opening.try_recv().is_ok()
        }));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release.send(()).unwrap();
        });
        system.start(&null(128)).unwrap();
        releaser.join().unwrap();
        for _ in 0..10 {
            system.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            system.state(),
            &AudioState::Running {
                config: null(128),
                latency: Duration::ZERO
            }
        );
        assert!(runs(&probe, 3));
        assert_eq!(probe.params().max_frames, 128);
    }

    #[test]
    fn an_asio_device_is_looked_for_less_often() {
        let (mut system, _probe, _sender) = system(0.0);
        system.set_retry_interval(Duration::from_millis(10));
        system.arm_retry(null(64));
        assert_eq!(
            system.retry.as_ref().unwrap().probe.interval,
            Duration::from_millis(10)
        );
        let asio = StreamConfig {
            host: HostKind::Asio,
            ..null(64)
        };
        system.arm_retry(asio);
        assert_eq!(
            system.retry.as_ref().unwrap().probe.interval,
            ASIO_RETRY_INTERVAL
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn jack_capture_only_routing_keeps_the_saved_output_selection() {
        let mut config = null(64);
        config.host = HostKind::Jack;
        config.jack_routes.capture = Some(vec!["USB:mic".into()]);
        assert!(!uses_saved_jack_output(&config));
        config.device = Some("Retired JACK Output".into());
        assert!(uses_saved_jack_output(&config));
        config.jack_routes.playback[0] = Some("Current Speaker:left".into());
        assert!(!uses_saved_jack_output(&config));
        config.jack_routes.playback[0] = None;
        config.device = None;
        config.device_id = Some(DeviceId("jack:output".into()));
        assert!(uses_saved_jack_output(&config));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn jack_saved_input_never_falls_back_to_an_unrelated_default() {
        let inventory = || {
            Ok(vec![crate::InputDeviceInfo {
                id: Some(DeviceId("jack:input".into())),
                name: "JACK Input".into(),
                channels: 2,
                is_default: true,
                sample_rate: 48_000,
            }])
        };
        let saved = |name: &str| crate::InputConfig {
            device: Some(name.into()),
            device_id: None,
        };
        assert!(!jack_capture_enabled(None, || panic!("disabled capture cannot scan")).unwrap());
        assert!(
            jack_capture_enabled(Some(&crate::InputConfig::default()), || panic!(
                "default is explicit"
            ))
            .unwrap()
        );
        assert!(jack_capture_enabled(Some(&saved("JACK Input")), inventory).unwrap());
        assert!(matches!(
            jack_capture_enabled(Some(&saved("Retired JACK Input")), inventory),
            Err(AudioError::NoDevice(_))
        ));
    }

    #[test]
    fn unavailable_jack_keeps_requested_source_slots_until_switching_hosts() {
        let (mut system, _, _) = system(0.0);
        let mut config = null(64);
        config.host = HostKind::Jack;
        config.jack_routes.capture = Some(vec!["USB:right".into(), "USB:left".into()]);
        assert!(
            system
                .start_with(&config, |_, _| Err(AudioError::HostUnavailable(
                    HostKind::Jack
                )))
                .is_err()
        );
        let sources = system.jack_capture_sources().unwrap();
        assert_eq!(sources[0].port.name, "USB:right");
        assert_eq!(sources[1].port.name, "USB:left");
        assert!(sources.iter().all(|source| !source.connected));
        system.start(&null(64)).unwrap();
        assert!(system.jack_capture_sources().is_none());
    }

    #[test]
    fn asio_output_never_arms_a_capture_only_reopen() {
        let target = CaptureTarget::Exact(DeviceId("asio:duplex".into()));
        assert!(CaptureRetry::for_output(HostKind::Asio, Some(target), Duration::ZERO).is_none());
        let target = CaptureTarget::Exact(DeviceId("wasapi:capture".into()));
        assert!(CaptureRetry::for_output(HostKind::Wasapi, Some(target), Duration::ZERO).is_some());
    }

    #[test]
    fn capture_retry_keeps_exact_target_and_cancels_a_superseded_worker() {
        let original = DeviceId("pipewire:alsa_input.capture".into());
        let target = CaptureTarget::Exact(original.clone());
        let mut retry = CaptureRetry::new(target, Duration::ZERO);
        let (began, starting) = std::sync::mpsc::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let (complete, finished) = std::sync::mpsc::channel();
        assert!(
            retry
                .poll(move |target, current| {
                    assert_eq!(target.selection().device_id, Some(original));
                    check_current_capture(current).unwrap();
                    began.send(()).unwrap();
                    paused.recv_timeout(WAIT).unwrap();
                    let state = check_current_capture(current);
                    complete.send(state.clone()).unwrap();
                    state?;
                    unreachable!("a cancelled worker must not finish opening")
                })
                .is_none()
        );
        starting.recv_timeout(WAIT).unwrap();
        assert!(retry.poll(|_, _| unreachable!("only one worker")).is_none());
        drop(retry);
        resume.send(()).unwrap();
        assert!(finished.recv_timeout(WAIT).unwrap().is_err());
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
            jack_routes: Default::default(),
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
        send::<CaptureOpenResult>();
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
