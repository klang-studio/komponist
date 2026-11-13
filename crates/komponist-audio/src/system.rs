//! Audio hosts, devices and the stream lifecycle (ARCHITECTURE §7, R1, S07).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::midi::MidiReceiver;
use crate::process::AudioProcess;

/// Weight of one callback's load in the smoothed CPU load (time constant ≈ 20 callbacks).
const LOAD_SMOOTHING: f32 = 0.05;

/// Audio driver ("Treiber" in S07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    /// PipeWire (cpal feature `pipewire`), the default on current desktops.
    PipeWire,
    /// JACK (cpal feature `jack`).
    Jack,
    /// ALSA.
    Alsa,
    /// No device: a timer thread drives the engine and discards the output (tests, "Kein
    /// Audio-Gerät").
    Null,
}

/// An output device of a host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    /// The host it belongs to.
    pub host: HostKind,
    /// Device name as the backend reports it ("Focusrite Scarlett 2i2"); used in [`StreamConfig`].
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
pub struct StreamConfig {
    /// Driver.
    pub host: HostKind,
    /// Output device name; `None` = the host's default device.
    pub device: Option<String>,
    /// Sample rate in Hz ("48000 Hz").
    pub sample_rate: u32,
    /// Buffer size in frames (segments 64 / 128 / 256 / 512 / 1024 in S07).
    pub buffer_frames: u32,
}

impl Default for StreamConfig {
    /// PipeWire, default device, 48000 Hz, 256 frames.
    fn default() -> StreamConfig {
        StreamConfig {
            host: HostKind::PipeWire,
            device: None,
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
    /// The requested device could not be opened or was lost. The engine runs on the null backend
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

    /// RT. Folds one callback's load (duration / period) into the smoothed value and counts the
    /// callback.
    ///
    /// One-pole smoothing per callback (weight 0.05); the first callback sets the value directly.
    pub fn record_callback(&self, load: f32) {
        let load = if load.is_finite() { load.max(0.0) } else { 0.0 };
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
}

/// Owns the stream and the engine while no stream holds it. UI thread.
///
/// The engine survives every stream change (R1): on teardown the stream's
/// [`crate::EngineSlot`] hands the engine back, [`AudioProcess::prepare`] runs with the new
/// parameters, and the new stream takes it over. A buffer change is "short dropout, engine
/// untouched" (S07).
pub struct AudioSystem<P: AudioProcess> {
    _engine: core::marker::PhantomData<P>,
}

impl<P: AudioProcess> AudioSystem<P> {
    /// Takes ownership of the engine and the MIDI receiver; no stream yet (state `Stopped`).
    pub fn new(engine: Box<P>, midi: MidiReceiver) -> AudioSystem<P> {
        todo!("WP-13")
    }

    /// Hosts available on this machine, in preference order (PipeWire, JACK, ALSA; `Null` always
    /// last).
    pub fn available_hosts() -> Vec<HostKind> {
        todo!("WP-13")
    }

    /// Output devices of `host`.
    pub fn devices(host: HostKind) -> Result<Vec<DeviceInfo>, AudioError> {
        todo!("WP-13")
    }

    /// Starts a stream with `config`, replacing a running one (stop → prepare → start). On failure
    /// the engine runs on the null backend with the requested rate and buffer, the state becomes
    /// `NoDevice` and the error is returned.
    pub fn start(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        todo!("WP-13")
    }

    /// Stops the stream; the engine comes back into the system. State `Stopped`.
    pub fn stop(&mut self) {
        todo!("WP-13")
    }

    /// Current state.
    pub fn state(&self) -> &AudioState {
        todo!("WP-13")
    }

    /// Shared statistics (survive stream changes).
    pub fn stats(&self) -> &Arc<StreamStats> {
        todo!("WP-13")
    }

    /// UI thread, every frame: notices a lost device (→ `NoDevice`, engine to the null backend) and
    /// collects engines handed back by torn-down streams.
    pub fn poll(&mut self) {
        todo!("WP-13")
    }

    /// Takes the engine out of the running stream (export, ARCHITECTURE §6.7): the callback hands
    /// it back at its next run and outputs silence from then on. Waits at most `timeout`.
    pub fn detach(&mut self, timeout: Duration) -> Result<Box<P>, AudioError> {
        todo!("WP-13")
    }

    /// Gives a detached engine back: calls `prepare` with the stream's parameters, then the
    /// callback takes it over at its next run.
    pub fn attach(&mut self, engine: Box<P>) {
        todo!("WP-13")
    }
}

impl<P: AudioProcess> core::fmt::Debug for AudioSystem<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AudioSystem").finish_non_exhaustive()
    }
}
