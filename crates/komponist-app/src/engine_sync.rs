//! The UI thread's side of the engine (ARCHITECTURE §6.3, §7, §9.3 "EngineSync").
//!
//! [`EngineSync`] owns the audio system (which owns the engine inside the stream callback), the
//! engine's command/event queues, the program last sent to the engine and the [`DeviceHost`]
//! that makes processors. After each document change, [`EngineSync::sync`] compiles the project
//! against the program it actually sent (WP-24 note: `compile::build` and `slot_changes` must see
//! the same `previous`), inserts the processors of new slots, installs the program, then removes
//! the processors of vanished slots — in that order, so a slot is filled before a program uses it
//! and emptied only after the program that used it is gone.
//!
//! During a gesture, fader, pan, mute and solo go to the engine as fast-path commands
//! ([`EngineSync::fast_path`]); the recompile waits for the gesture's end.
//!
//! The engine runs inside [`EngineProcess`], the app's [`AudioProcess`]: it maps live MIDI to
//! engine input and writes the engine's planar output into the device buffer. Garbage the engine
//! retires is dropped here, on the UI thread, when [`EngineSync::poll`] drains the event queue.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use komponist_audio::{
    AudioError, AudioProcess, AudioState, AudioSystem, CallbackInfo, HostKind, MidiIn,
    MidiInputHub, MidiReceiver, OutputBuffer, StreamConfig, StreamParams, StreamStats,
};
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, LiveInput, MAX_BLOCK,
    MAX_EVENTS_PER_BLOCK, MidiInEvent, MidiPort, Program, Telemetry, TrackMeter,
};
use komponist_model::{Document, Project, Tick, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent, ProcessorSetup};

use crate::device_host::DeviceHost;

/// How long [`EngineSync::shutdown`] lets the engine drain its last commands.
const SHUTDOWN_DRAIN: Duration = Duration::from_millis(30);

/// The engine as the stream's [`AudioProcess`].
pub struct EngineProcess {
    engine: Box<Engine>,
    left: Vec<f32>,
    right: Vec<f32>,
    /// Live MIDI of one callback (preallocated, never grows on the audio thread).
    midi: Vec<MidiInEvent>,
    /// The sample rate the engine runs at, for the UI thread (processor activation).
    rate: Arc<AtomicU32>,
}

impl EngineProcess {
    /// Wraps `engine`; `rate` receives the stream's sample rate in `prepare`.
    pub fn new(engine: Box<Engine>, rate: Arc<AtomicU32>) -> EngineProcess {
        EngineProcess {
            engine,
            left: vec![0.0; MAX_BLOCK],
            right: vec![0.0; MAX_BLOCK],
            midi: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            rate,
        }
    }
}

impl AudioProcess for EngineProcess {
    fn prepare(&mut self, params: &StreamParams) {
        let rate = params.sample_rate as f32;
        if rate != self.engine.sample_rate() {
            // Processors that fail stay in their slot and report through `ProcessorFailed`.
            let _ = self.engine.set_sample_rate(rate);
        }
        self.rate.store(params.sample_rate, Ordering::Relaxed);
        let frames = (params.max_frames as usize).max(MAX_BLOCK);
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
    }

    fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo, midi: &mut MidiIn<'_>) {
        komponist_audio::enable_flush_to_zero();
        self.midi.clear();
        while let Some(m) = midi.pop() {
            if self.midi.len() < self.midi.capacity() {
                self.midi.push(MidiInEvent {
                    frame: midi.frame_of(m.time),
                    port: MidiPort(m.port.0),
                    data: m.data,
                });
            }
        }
        let frames = out.frames().min(self.left.len());
        let latency = info.output_latency.as_secs_f64() * f64::from(info.sample_rate);
        let input = LiveInput {
            midi: &self.midi,
            output_latency: latency as u32,
        };
        let (left, right) = (&mut self.left[..frames], &mut self.right[..frames]);
        self.engine.process(left, right, &input);
        out.write_stereo(left, right);
    }
}

/// How the MIDI inputs are opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiSetup {
    /// Open the system's MIDI inputs (midir); falls back to none if that fails.
    System,
    /// No MIDI input (headless tests).
    None,
}

/// What [`EngineSync::poll`] reports to the app (everything but garbage).
#[derive(Debug)]
pub enum SyncEvent {
    /// An engine event (recorded notes, loop wraps, count-in end, failed processors).
    Engine(EngineEvent),
    /// A device host event (status, state snapshot, latency, …).
    Device(PluginManagerEvent),
}

/// The compiler's view of the app: MIDI ports from the hub, latencies from the device host.
struct Env<'a> {
    midi: Option<&'a MidiInputHub>,
    host: &'a dyn DeviceHost,
}

impl CompileEnv for Env<'_> {
    fn midi_port(&self, device_name: &str) -> Option<MidiPort> {
        self.midi?.port_id(device_name).map(|id| MidiPort(id.0))
    }

    fn device_latency(&self, device: komponist_model::DeviceId) -> u32 {
        self.host.latency(device)
    }
}

/// Engine, audio stream and program sync. UI thread.
pub struct EngineSync {
    audio: AudioSystem<EngineProcess>,
    handle: EngineHandle,
    midi: Option<MidiInputHub>,
    host: Box<dyn DeviceHost>,
    /// The program last sent to the engine.
    previous: Option<Program>,
    /// Document revision `previous` was compiled from.
    synced: Option<u64>,
    /// The latest revision whose changes reached the engine through fast-path commands only
    /// (during a gesture); the recompile waits for the gesture's end.
    fast: Option<u64>,
    /// Commands the full queue did not take yet, in order.
    pending: VecDeque<Command>,
    /// Device host events that survived a [`EngineSync::reset`], delivered by the next poll.
    carried: Vec<PluginManagerEvent>,
    rate: Arc<AtomicU32>,
    telemetry: Arc<Telemetry>,
}

impl core::fmt::Debug for EngineSync {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineSync")
            .field("state", self.audio.state())
            .field("synced", &self.synced)
            .finish_non_exhaustive()
    }
}

impl EngineSync {
    /// Creates the engine at `config`'s rate and starts the stream. A failing device leaves the
    /// engine on the null backend (state `NoDevice`, the UI shows "Kein Audio-Gerät"); the error
    /// is returned next to the working sync.
    pub fn new(
        config: &StreamConfig,
        midi: MidiSetup,
        host: Box<dyn DeviceHost>,
    ) -> (EngineSync, Result<(), AudioError>) {
        let (engine, handle) = Engine::new(EngineConfig {
            sample_rate: config.sample_rate as f32,
            ..EngineConfig::default()
        });
        let rate = Arc::new(AtomicU32::new(config.sample_rate));
        let process = EngineProcess::new(engine, Arc::clone(&rate));
        let (hub, receiver) = match midi {
            MidiSetup::System => match MidiInputHub::new() {
                Ok((mut hub, receiver)) => {
                    let _ = hub.refresh();
                    (Some(hub), receiver)
                }
                Err(_) => (None, MidiReceiver::disconnected()),
            },
            MidiSetup::None => (None, MidiReceiver::disconnected()),
        };
        let telemetry = Arc::clone(handle.telemetry());
        let mut sync = EngineSync {
            audio: AudioSystem::new(Box::new(process), receiver),
            handle,
            midi: hub,
            host,
            previous: None,
            synced: None,
            fast: None,
            pending: VecDeque::new(),
            carried: Vec::new(),
            rate,
            telemetry,
        };
        let started = sync.audio.start(config);
        (sync, started)
    }

    /// The null backend at 48 kHz / 256 frames, no MIDI: for tests and `--null-audio`.
    pub fn null(host: Box<dyn DeviceHost>) -> EngineSync {
        let config = StreamConfig {
            host: HostKind::Null,
            ..StreamConfig::default()
        };
        let (sync, started) = EngineSync::new(&config, MidiSetup::None, host);
        // The null backend only fails on an invalid configuration, and this one is valid.
        debug_assert!(started.is_ok(), "{started:?}");
        sync
    }

    /// Compiles and sends `doc`'s project if it changed since the last sync. During a gesture
    /// whose changes went out as fast-path commands, waits for the gesture's end.
    pub fn sync(&mut self, doc: &Document) {
        let revision = doc.revision();
        if self.synced == Some(revision) {
            return;
        }
        if doc.in_gesture() && self.fast == Some(revision) {
            return;
        }
        self.send_project(doc.project());
        self.synced = Some(revision);
        self.fast = None;
    }

    /// Compiles `project` against the program last sent and sends it (steps of the module docs).
    pub fn send_project(&mut self, project: &Project) {
        let next = {
            let env = Env {
                midi: self.midi.as_ref(),
                host: &*self.host,
            };
            compile::build(project, self.previous.as_ref(), &env)
        };
        let changes = compile::slot_changes(self.previous.as_ref(), &next);
        let setup = self.setup();
        for (device, slot) in changes.added {
            let Some((_, _, device)) = project.find_device(device) else {
                continue;
            };
            if let Some(processor) = self.host.create_processor(device, setup) {
                self.send(Command::InsertProcessor { slot, processor });
            }
        }
        self.send(Command::SetProgram(Box::new(next.clone())));
        self.previous = Some(next);
        for (device, slot) in changes.removed {
            self.send(Command::RemoveProcessor { slot });
            self.host.release(device);
        }
    }

    /// Replaces the processor of `device` with a fresh one from the host (pisynth preset
    /// loaded into the device state, "Neu laden"). Nothing happens if the device has no slot.
    pub fn reload_device(&mut self, project: &Project, device: komponist_model::DeviceId) {
        let Some(slot) = self.previous.as_ref().and_then(|p| p.slots.slot(device)) else {
            return;
        };
        let Some((_, _, d)) = project.find_device(device) else {
            return;
        };
        let setup = self.setup();
        if let Some(processor) = self.host.create_processor(d, setup) {
            self.send(Command::InsertProcessor { slot, processor });
        }
    }

    /// A fast-path command for a change the dispatcher just applied inside a gesture
    /// (`SetTrackGain`, `SetTrackPan`, `SetTrackMute`, `SetTrackSolo`, `SetMasterGain`):
    /// `before` and `after` are the document revisions around the change. The engine gets the
    /// command now; if the engine was up to date before, the recompile is deferred to the end of
    /// the gesture.
    pub fn fast_path(&mut self, command: Command, before: u64, after: u64) {
        if self.synced == Some(before) || self.fast == Some(before) {
            self.fast = Some(after);
        }
        self.send(command);
    }

    /// Queues `command`; if the engine's queue is full it is retried at the next
    /// [`EngineSync::poll`], keeping the order.
    pub fn send(&mut self, command: Command) {
        if !self.pending.is_empty() {
            self.pending.push_back(command);
            return;
        }
        if let Err(command) = self.handle.send(command) {
            self.pending.push_back(command);
        }
    }

    fn flush_pending(&mut self) {
        while let Some(command) = self.pending.pop_front() {
            if let Err(command) = self.handle.send(command) {
                self.pending.push_front(command);
                break;
            }
        }
    }

    /// Call every UI frame: polls the audio system, retries queued commands, drains the engine's
    /// events (dropping garbage here) and the device host's events.
    pub fn poll(&mut self) -> Vec<SyncEvent> {
        self.audio.poll();
        self.flush_pending();
        let mut out = Vec::new();
        while let Some(event) = self.handle.poll() {
            match event {
                // Dropped here, on the UI thread.
                EngineEvent::Retired(garbage) => drop(garbage),
                other => out.push(SyncEvent::Engine(other)),
            }
        }
        let carried = std::mem::take(&mut self.carried);
        out.extend(
            carried
                .into_iter()
                .chain(self.host.poll())
                .map(SyncEvent::Device),
        );
        out
    }

    /// Rebuilds the stream for `config` (S07: driver, device, rate, buffer). The engine survives;
    /// a new rate re-activates the processors.
    pub fn set_audio(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        self.audio.start(config)
    }

    /// Stops playback, lets the engine drain, stops the stream (app exit).
    pub fn shutdown(&mut self) {
        self.send(Command::Stop);
        self.flush_pending();
        std::thread::sleep(SHUTDOWN_DRAIN);
        self.audio.stop();
        while self.handle.poll().is_some() {}
    }

    /// Processor setup at the engine's current rate.
    pub fn setup(&self) -> ProcessorSetup {
        ProcessorSetup {
            sample_rate: self.sample_rate() as f32,
            max_block: MAX_BLOCK,
        }
    }

    /// The engine's sample rate in Hz (the stream's, once it started).
    pub fn sample_rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Transport state, playhead and master meter.
    pub fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    /// Playhead.
    pub fn playhead(&self) -> Tick {
        self.telemetry.playhead()
    }

    /// CPU load and xruns of the stream.
    pub fn stats(&self) -> &StreamStats {
        self.audio.stats()
    }

    /// The audio system's state (running config, `NoDevice`).
    pub fn audio_state(&self) -> &AudioState {
        self.audio.state()
    }

    /// The MIDI inputs, if the system's MIDI could be opened.
    pub fn midi(&self) -> Option<&MidiInputHub> {
        self.midi.as_ref()
    }

    /// The MIDI inputs, mutable (S07 switches).
    pub fn midi_mut(&mut self) -> Option<&mut MidiInputHub> {
        self.midi.as_mut()
    }

    /// The program last sent to the engine.
    pub fn program(&self) -> Option<&Program> {
        self.previous.as_ref()
    }

    /// The level meter of `track` in the program last sent.
    pub fn track_meter(&self, track: TrackId) -> Option<&Arc<TrackMeter>> {
        self.previous.as_ref()?.meter(track)
    }

    /// The device host.
    pub fn host(&self) -> &dyn DeviceHost {
        &*self.host
    }

    /// The device host, mutable (parameters, plugin GUIs, scans, state snapshots). Processors are
    /// created and released only through [`EngineSync::sync`], never directly.
    pub fn host_mut(&mut self) -> &mut dyn DeviceHost {
        &mut *self.host
    }

    /// Runtime status of a device.
    pub fn device_status(&self, device: komponist_model::DeviceId) -> DeviceStatus {
        self.host.status(device)
    }

    /// Replaces the device host (WP-45 plugs in the plugin manager). Forces a full resync: the
    /// next [`EngineSync::sync`] compiles from scratch and every device gets a processor from the
    /// new host. Removes the old processors first.
    pub fn set_host(&mut self, host: Box<dyn DeviceHost>) {
        if let Some(previous) = self.previous.take() {
            for &(device, slot) in previous.slots.entries() {
                self.send(Command::RemoveProcessor { slot });
                self.host.release(device);
            }
        }
        self.host = host;
        self.synced = None;
        self.fast = None;
    }

    /// Forgets the project: every processor leaves the engine and the host releases every device
    /// (bridges end), and the next [`EngineSync::sync`] compiles from scratch. Call it before
    /// another document becomes the open one (open, new): device ids restart at 1 in every
    /// project, so the old program's processors would otherwise be reused for the wrong devices.
    ///
    /// The host's events that are still queued and name a device belong to the old project and
    /// are dropped; the others (a finished scan, scan warnings) reach the next
    /// [`EngineSync::poll`].
    pub fn reset(&mut self) {
        if let Some(previous) = self.previous.take() {
            for &(device, slot) in previous.slots.entries() {
                self.send(Command::RemoveProcessor { slot });
                self.host.release(device);
            }
        }
        self.synced = None;
        self.fast = None;
        let events = self.host.poll();
        self.carried.extend(events.into_iter().filter(|e| {
            matches!(
                e,
                PluginManagerEvent::ScanFinished(_) | PluginManagerEvent::Log { device: None, .. }
            )
        }));
    }

    /// Commands waiting for room in the engine's queue (diagnostics, tests).
    pub fn pending_commands(&self) -> usize {
        self.pending.len()
    }
}

impl Drop for EngineSync {
    fn drop(&mut self) {
        self.audio.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_host::BuiltinHost;
    use komponist_engine::TransportState;
    use komponist_model::edits::{self, ChannelProp, TrackProp};
    use komponist_model::fixtures::demo_project;
    use std::time::Instant;

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn the_demo_plays_on_the_null_backend() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        assert!(matches!(sync.audio_state(), AudioState::Running { .. }));
        let doc = Document::new(demo_project());
        sync.sync(&doc);
        let program = sync.program().expect("sent");
        assert_eq!(program.tracks.len(), doc.project().tracks.len());
        sync.send(Command::Play);
        assert!(wait_until(|| {
            sync.poll();
            sync.playhead() > Tick::ZERO
        }));
        assert_eq!(sync.telemetry().transport(), TransportState::Playing);
        sync.send(Command::Stop);
        assert!(wait_until(|| {
            sync.poll();
            sync.telemetry().transport() == TransportState::Stopped
        }));
        sync.shutdown();
    }

    #[test]
    fn unchanged_documents_are_not_recompiled() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        let doc = Document::new(demo_project());
        sync.sync(&doc);
        let first = sync.program().unwrap().tracks[0].clone();
        sync.sync(&doc);
        assert!(Arc::ptr_eq(&first, &sync.program().unwrap().tracks[0]));
    }

    #[test]
    fn removed_devices_leave_the_host_and_new_ones_get_processors() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        let mut doc = Document::new(demo_project());
        sync.sync(&doc);
        let lead_synth = doc.project().tracks[2].channel.devices[0].id;
        assert_eq!(sync.device_status(lead_synth), DeviceStatus::Ok);
        let slot = sync.program().unwrap().slots.slot(lead_synth);
        assert!(slot.is_some());
        let lead = doc.project().tracks[2].id;
        doc.apply("Spur löschen", |p| edits::remove_tracks(p, &[lead]))
            .unwrap();
        sync.sync(&doc);
        assert_eq!(sync.program().unwrap().slots.slot(lead_synth), None);
        // Released: the host forgot it (unknown devices report Ok).
        assert_eq!(sync.device_status(lead_synth), DeviceStatus::Ok);
        let surge = doc.project().tracks[2].channel.devices[0].id;
        assert_eq!(sync.device_status(surge), DeviceStatus::Missing);
    }

    #[test]
    fn reset_starts_the_next_project_from_scratch() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        let first = Document::new(demo_project());
        sync.sync(&first);
        let lead = first.project().tracks[2].channel.devices[0].id;
        assert!(sync.program().unwrap().slots.slot(lead).is_some());
        sync.reset();
        assert!(sync.program().is_none());
        // Released: the host forgot every device (unknown devices report Ok).
        let klopfer = first.project().tracks[0].channel.devices[0].id;
        assert_eq!(sync.device_status(klopfer), DeviceStatus::Ok);
        // A new document starts at revision 0 like the old one; it is compiled anyway, and its
        // devices (ids from 1 again) get fresh processors.
        let second = Document::new(demo_project());
        assert_eq!(second.revision(), first.revision());
        sync.sync(&second);
        let program = sync.program().expect("compiled again");
        assert_eq!(program.tracks.len(), second.project().tracks.len());
        assert_eq!(sync.device_status(klopfer), DeviceStatus::Missing);
        // The same document again without reset: nothing is recompiled.
        let tracks = Arc::clone(&program.tracks[0]);
        sync.sync(&second);
        assert!(Arc::ptr_eq(&tracks, &sync.program().unwrap().tracks[0]));
    }

    #[test]
    fn fast_path_defers_the_recompile_to_the_gesture_end() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        let mut doc = Document::new(demo_project());
        sync.sync(&doc);
        let track = doc.project().tracks[1].id;
        let before_program = sync.program().unwrap().tracks[1].clone();
        doc.begin_gesture("Lautstärke");
        let before = doc.revision();
        doc.apply_in_gesture(|p| {
            edits::set_track_prop(
                p,
                &[track],
                TrackProp::Channel(ChannelProp::VolumeDb(-12.0)),
            )
        })
        .unwrap();
        sync.fast_path(
            Command::SetTrackGain { track, gain: 0.25 },
            before,
            doc.revision(),
        );
        sync.sync(&doc);
        assert!(
            Arc::ptr_eq(&before_program, &sync.program().unwrap().tracks[1]),
            "no recompile during the gesture"
        );
        doc.end_gesture();
        sync.sync(&doc);
        assert!(!Arc::ptr_eq(
            &before_program,
            &sync.program().unwrap().tracks[1]
        ));
    }
}
