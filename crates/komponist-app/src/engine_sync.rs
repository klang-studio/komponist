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
//! ([`EngineSync::fast_path`]); the recompile waits for the gesture's end. A device parameter
//! (`SetParam`) is the same: the running processor already has the value, so the app stores it
//! in the document without [`EngineSync::sync_device_states`]; only undo, redo, cancelled
//! gestures, presets and pastes follow the document that way (as `SetParam`s where the change
//! allows, so hexa keeps sounding).
//!
//! The engine runs inside [`EngineProcess`], the app's [`AudioProcess`]: it maps live MIDI to
//! engine input and writes the engine's planar output into the device buffer. Garbage the engine
//! retires is dropped here, on the UI thread, when [`EngineSync::poll`] drains the event queue.
//!
//! Tests that need exact positions take the engine out of the stream ([`EngineSync::hold_engine`])
//! and run it themselves with simulated MIDI input ([`EngineSync::run_held`]): time then passes
//! only there.
//!
//! The WAV export (ARCHITECTURE §6.7) takes the engine out of the stream together with its handle
//! ([`EngineSync::detach`] → [`DetachedEngine`]) and renders on a worker thread. Meanwhile commands
//! wait in the pending queue and no engine events arrive; [`EngineSync::attach`] gives both back
//! and sends what waited.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_audio::{
    AudioError, AudioProcess, AudioState, AudioSystem, CallbackInfo, HostKind, MidiIn,
    MidiInputHub, MidiPortInfo, MidiReceiver, OutputBuffer, StreamConfig, StreamParams,
    StreamStats,
};
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::workers::{
    WorkerRt, WorkerStats, WorkerStatus, auto_workers, cap_to_workgroup,
};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, LiveInput, MAX_BLOCK,
    MAX_EVENTS_PER_BLOCK, MidiInEvent, MidiPort, Program, SlotId, Telemetry, TrackMeter,
    TransportState,
};
use komponist_model::{Document, Project, Tick, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent, ProcessorSetup};

use crate::device_host::DeviceHost;

/// How long [`EngineSync::shutdown`] lets the engine drain its last commands.
const SHUTDOWN_DRAIN: Duration = Duration::from_millis(30);

/// How often [`EngineSync::poll`] looks for MIDI devices that appeared or vanished (hot-plug).
pub const MIDI_SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// How long [`EngineSync::hold_engine`] waits for the stream to hand the engine over.
const HOLD_TIMEOUT: Duration = Duration::from_secs(2);

/// Frames per `Engine::process` call of [`EngineSync::run_held`]: the null backend's default
/// buffer, so commands take effect at the same granularity as in the running app.
const HELD_BLOCK: usize = 256;

/// The engine as the stream's [`AudioProcess`].
pub struct EngineProcess {
    engine: Box<Engine>,
    left: Vec<f32>,
    right: Vec<f32>,
    /// Live MIDI of one callback (preallocated, never grows on the audio thread).
    midi: Vec<MidiInEvent>,
    /// The sample rate the engine runs at, for the UI thread (processor activation).
    rate: Arc<AtomicU32>,
    /// Worker threads the engine should run (WP-65), started in `prepare`.
    workers: usize,
    /// `workers` is [`auto_workers`] (not the user's setting): a stream's workgroup may cap it.
    workers_auto: bool,
    /// How they run in realtime: the period and the audio workgroup of the latest stream (#45).
    rt: WorkerRt,
    /// Outcome of the last worker start, for the UI thread (only touched outside the callback).
    worker_status: Arc<Mutex<Option<WorkerStatus>>>,
    /// Set by a worker start that ran without realtime priority; the app takes it once
    /// ([`EngineSync::take_worker_notice`]).
    worker_notice: Arc<AtomicBool>,
    /// Likewise for workers that could not join the audio workgroup
    /// ([`EngineSync::take_workgroup_notice`]).
    workgroup_notice: Arc<AtomicBool>,
}

/// Whether a worker start deserves a status bar notice: workers run, but without the realtime
/// priority that was asked for. Workers off or none started: nothing to say (single-threaded).
pub fn lacks_realtime(status: &WorkerStatus) -> bool {
    status.threads > 0 && status.realtime_error.is_some()
}

/// Whether a worker start deserves a status bar notice because workers could not join the audio
/// device's workgroup (macOS; without it the realtime policy alone gives dropouts at 64-128
/// frames, real-IO measurement #79). Only a workgroup that was given and refused counts: streams
/// without one (null backend, Linux) have `workgroup_error == None`.
pub fn lacks_workgroup(status: &WorkerStatus) -> bool {
    status.threads > 0 && status.workgroup_error.is_some()
}

impl EngineProcess {
    /// Wraps `engine`; `rate` receives the stream's sample rate in `prepare`. The engine gets
    /// [`auto_workers`] worker threads when the stream starts ([`EngineProcess::set_workers`]).
    pub fn new(engine: Box<Engine>, rate: Arc<AtomicU32>) -> EngineProcess {
        EngineProcess {
            engine,
            left: vec![0.0; MAX_BLOCK],
            right: vec![0.0; MAX_BLOCK],
            midi: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
            rate,
            workers: auto_workers(),
            workers_auto: true,
            // Replaced by the stream's in `prepare`.
            rt: WorkerRt::new(Duration::from_secs_f64(HELD_BLOCK as f64 / 48_000.0)),
            worker_status: Arc::default(),
            worker_notice: Arc::default(),
            workgroup_notice: Arc::default(),
        }
    }

    /// Non-RT (out of the stream). Sets the worker threads the engine runs from the next
    /// `prepare` on (0: single-threaded).
    pub fn set_workers(&mut self, threads: usize) {
        self.workers = threads;
        self.workers_auto = false;
    }

    /// Like [`EngineProcess::set_workers`] with [`auto_workers`], which the stream's workgroup
    /// may cap (the IO thread plus the workers stay within its `max_parallel_threads`).
    pub fn set_auto_workers(&mut self) {
        self.workers = auto_workers();
        self.workers_auto = true;
    }

    /// The settings file's `worker_threads`: `None` is automatic, `Some(n)` is exactly `n`.
    fn apply_worker_setting(&mut self, setting: Option<u32>) {
        match setting {
            Some(n) => self.set_workers(n as usize),
            None => self.set_auto_workers(),
        }
    }

    /// The worker count to start: the setting, or for the automatic one capped by the workgroup.
    fn effective_workers(&self) -> usize {
        if self.workers_auto {
            cap_to_workgroup(self.workers, self.rt.workgroup.as_ref())
        } else {
            self.workers
        }
    }

    /// Starts or replaces the engine's worker pool if the count or the realtime set-up changed
    /// (non-RT).
    fn apply_workers(&mut self) {
        let status = self
            .engine
            .set_workers(self.effective_workers(), Some(self.rt.clone()));
        self.worker_notice
            .store(lacks_realtime(&status), Ordering::Relaxed);
        self.workgroup_notice
            .store(lacks_workgroup(&status), Ordering::Relaxed);
        *self.worker_status.lock().unwrap_or_else(|e| e.into_inner()) = Some(status);
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
        // The worker pool is created when a stream starts (WP-65); unchanged it stays. On macOS the
        // workers' time-constraint policy follows the stream's period and they join its device's
        // IO workgroup, looked up anew for every stream, so the pool is rebuilt (#45).
        self.rt.period = params.period();
        self.rt.workgroup = params.workgroup.clone();
        self.apply_workers();
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

/// A MIDI input device that appeared or vanished between two scans ([`midi_changes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiChange {
    /// The port is listed and connected now, and was not before.
    Appeared(String),
    /// The port was present before and is gone (or not connected) now.
    Vanished(String),
}

/// The devices that appeared or vanished from `before` to `after`. A device is present when its
/// port is listed and connected; ports are compared by name (ids and the `enabled` switch do not
/// matter, a disabled port still counts as plugged in). Order: vanished first, then appeared,
/// each in port order.
pub fn midi_changes(before: &[MidiPortInfo], after: &[MidiPortInfo]) -> Vec<MidiChange> {
    let present =
        |ports: &[MidiPortInfo], name: &str| ports.iter().any(|p| p.connected && p.name == name);
    let vanished = before
        .iter()
        .filter(|p| p.connected && !present(after, &p.name))
        .map(|p| MidiChange::Vanished(p.name.clone()));
    let appeared = after
        .iter()
        .filter(|p| p.connected && !present(before, &p.name))
        .map(|p| MidiChange::Appeared(p.name.clone()));
    vanished.chain(appeared).collect()
}

/// Engine, audio stream and program sync. UI thread.
pub struct EngineSync {
    audio: AudioSystem<EngineProcess>,
    /// `None` while an export has the engine ([`EngineSync::detach`]).
    handle: Option<EngineHandle>,
    midi: Option<MidiInputHub>,
    /// When [`EngineSync::poll`] scans the MIDI ports next ([`MIDI_SCAN_INTERVAL`]).
    next_midi_scan: Instant,
    /// Devices that appeared or vanished in scans since the last
    /// [`EngineSync::take_midi_changes`].
    midi_changes: Vec<MidiChange>,
    host: Box<dyn DeviceHost>,
    /// The program last sent to the engine.
    previous: Option<Program>,
    /// Document revision `previous` was compiled from.
    synced: Option<u64>,
    /// How many programs [`EngineSync::send_project`] compiled and sent (tests: a gesture must not
    /// recompile per frame).
    programs_sent: u64,
    /// The latest revision whose changes reached the engine through fast-path commands only
    /// (during a gesture); the recompile waits for the gesture's end.
    fast: Option<u64>,
    /// How many times [`EngineSync::reload_device`] made a fresh processor.
    reloads: u64,
    /// Commands the full queue did not take yet, in order.
    pending: VecDeque<Command>,
    /// Device host events that survived a [`EngineSync::reset`], delivered by the next poll.
    carried: Vec<PluginManagerEvent>,
    rate: Arc<AtomicU32>,
    telemetry: Arc<Telemetry>,
    /// The engine taken out of the stream ([`EngineSync::hold_engine`]).
    held: Option<Box<EngineProcess>>,
    /// The playhead when an export took the engine ([`EngineSync::detach`]): shown meanwhile.
    frozen: Option<Tick>,
    /// Worker threads asked for while an export had the engine (applied at [`EngineSync::attach`]).
    workers_after_export: Option<Option<u32>>,
    /// Outcome of the engine's last worker start ([`EngineProcess`] writes it in `prepare`).
    worker_status: Arc<Mutex<Option<WorkerStatus>>>,
    /// See [`EngineProcess`]'s `worker_notice`.
    worker_notice: Arc<AtomicBool>,
    /// See [`EngineProcess`]'s `workgroup_notice`.
    workgroup_notice: Arc<AtomicBool>,
    worker_stats: Arc<WorkerStats>,
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
        EngineSync::with_receiver(config, hub, receiver, host)
    }

    /// [`EngineSync::new`] with the MIDI queue's receiver given (no hub): a
    /// [`komponist_audio::TestMidiSource`] feeds it in tests.
    pub fn with_receiver(
        config: &StreamConfig,
        hub: Option<MidiInputHub>,
        receiver: MidiReceiver,
        host: Box<dyn DeviceHost>,
    ) -> (EngineSync, Result<(), AudioError>) {
        let (engine, handle) = Engine::new(EngineConfig {
            sample_rate: config.sample_rate as f32,
            ..EngineConfig::default()
        });
        let rate = Arc::new(AtomicU32::new(config.sample_rate));
        let worker_stats = engine.worker_stats();
        let process = EngineProcess::new(engine, Arc::clone(&rate));
        let worker_status = Arc::clone(&process.worker_status);
        let worker_notice = Arc::clone(&process.worker_notice);
        let workgroup_notice = Arc::clone(&process.workgroup_notice);
        let telemetry = Arc::clone(handle.telemetry());
        let mut sync = EngineSync {
            audio: AudioSystem::new(Box::new(process), receiver),
            handle: Some(handle),
            midi: hub,
            next_midi_scan: Instant::now() + MIDI_SCAN_INTERVAL,
            midi_changes: Vec::new(),
            host,
            previous: None,
            synced: None,
            programs_sent: 0,
            fast: None,
            reloads: 0,
            pending: VecDeque::new(),
            carried: Vec::new(),
            rate,
            telemetry,
            held: None,
            frozen: None,
            workers_after_export: None,
            worker_status,
            worker_notice,
            workgroup_notice,
            worker_stats,
        };
        let started = sync.audio.start(config);
        (sync, started)
    }

    /// The null backend at 48 kHz / 256 frames, no MIDI: for tests and `--null-audio`.
    pub fn null(host: Box<dyn DeviceHost>) -> EngineSync {
        EngineSync::null_with_midi(host, MidiReceiver::disconnected())
    }

    /// [`EngineSync::null`] with a MIDI queue (tests: [`komponist_audio::TestMidiSource`]).
    pub fn null_with_midi(host: Box<dyn DeviceHost>, midi: MidiReceiver) -> EngineSync {
        let config = StreamConfig {
            host: HostKind::Null,
            ..StreamConfig::default()
        };
        let (sync, started) = EngineSync::with_receiver(&config, None, midi, host);
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

    /// How many programs were compiled and sent so far (tests).
    pub fn programs_sent(&self) -> u64 {
        self.programs_sent
    }

    /// Compiles `project` against the program last sent and sends it (steps of the module docs).
    pub fn send_project(&mut self, project: &Project) {
        let mut next = {
            let env = Env {
                midi: self.midi.as_ref(),
                host: &*self.host,
            };
            compile::build(project, self.previous.as_ref(), &env)
        };
        let changes = compile::slot_changes(self.previous.as_ref(), &next);
        let setup = self.setup();
        let mut late_latency = false;
        for (device, slot) in changes.added {
            let Some((_, _, device)) = project.find_device(device) else {
                continue;
            };
            if let Some(processor) = self.host.create_processor(device, setup) {
                late_latency |= processor.latency() > 0;
                self.send(Command::InsertProcessor { slot, processor });
            }
        }
        if late_latency {
            // The host knows a new processor's latency only once it exists (WP-91): compile again
            // so the delay compensation counts it. The slots stay as they are.
            let env = Env {
                midi: self.midi.as_ref(),
                host: &*self.host,
            };
            next = compile::build(project, Some(&next), &env);
        }
        self.send(Command::SetProgram(Box::new(next.clone())));
        self.programs_sent += 1;
        self.previous = Some(next);
        for (device, slot) in changes.removed {
            self.send(Command::RemoveProcessor { slot });
            self.host.release(device);
        }
    }

    /// Replaces the processor of `device` with a fresh one from the host (hexa preset
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
            self.reloads += 1;
            self.send(Command::InsertProcessor { slot, processor });
        }
    }

    /// How many fresh processors [`EngineSync::reload_device`] made ("Neu laden", a found plugin,
    /// a hexa state change no card parameter reaches). A knob turn and its undo or redo must not
    /// count here: they reach the running processor as `SetParam`.
    pub fn device_reloads(&self) -> u64 {
        self.reloads
    }

    /// Brings the running devices in line with their document state after a change that
    /// replaced states outside the dispatcher's own paths (undo, redo, a preset, a paste over):
    /// every device in both `before` and `after` whose state differs follows `after`.
    ///
    /// - hexa gets the patch fields that differ as `SetParam`s of its card parameters
    ///   (`komponist_builtin::device::param_changes`), so a sounding note goes on; a change no
    ///   parameter reaches (filters, effects, modulation, … as between most presets), more than
    ///   [`komponist_engine::MAX_PENDING_PARAMS`] changes or a state that does not decode give a
    ///   fresh processor made from the patch (like "Neu laden");
    /// - an internal device gets the parameters that differ as `SetParam`, so tails and sounding
    ///   notes go on; the Sampler also loads or clears the pads whose sample link differs (into
    ///   `project_dir`'s samples); a state that does not decode gives a fresh processor;
    /// - a bridged plugin gets `SetState` over its control socket.
    ///
    /// Devices without slot (not in the sent program yet) are skipped: their processor is made
    /// from the document state when the program is sent.
    pub fn sync_device_states(
        &mut self,
        before: &Project,
        after: &Project,
        project_dir: Option<&std::path::Path>,
    ) {
        let devices = after
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .chain(after.master.devices.iter());
        for device in devices {
            let Some((_, _, old)) = before.find_device(device.id) else {
                continue;
            };
            if Arc::ptr_eq(old, device) || old.state == device.state {
                continue;
            }
            if self.slot_of(device.id).is_some() {
                self.follow_state(old, device, after, project_dir);
            }
        }
    }

    /// The slot of `device` in the program last sent.
    fn slot_of(&self, device: komponist_model::DeviceId) -> Option<SlotId> {
        self.previous.as_ref().and_then(|p| p.slots.slot(device))
    }

    fn follow_state(
        &mut self,
        old: &komponist_model::Device,
        device: &komponist_model::Device,
        project: &Project,
        project_dir: Option<&std::path::Path>,
    ) {
        use crate::device_host::{internal_kind, is_hexa};
        if is_hexa(device) {
            // The patch fields that differ, as the card's parameters, so a sounding note goes on
            // (#19); a change no parameter reaches (a preset with other filters, say) needs a
            // fresh processor.
            let decode = |d: &komponist_model::Device| {
                komponist_builtin::decode_state(d.state.as_deref()).ok()
            };
            let changes = decode(old)
                .zip(decode(device))
                .and_then(|(was, now)| komponist_builtin::device::param_changes(&was, &now))
                .filter(|c| c.len() <= komponist_engine::MAX_PENDING_PARAMS);
            let Some(changes) = changes else {
                self.reload_device(project, device.id);
                return;
            };
            let slot = self.slot_of(device.id).expect("checked by the caller");
            for (id, value) in changes {
                self.send(Command::SetParam { slot, id, value });
            }
        } else if let Some(kind) = internal_kind(device) {
            // A state with only sample links has no `params`: its values are the defaults (as
            // the processor made from it has them).
            let values = |d: &komponist_model::Device| match kind.decode_state(d.state.as_deref()) {
                Err(komponist_builtin::internal::StateError::NoParams) => Ok(kind.defaults()),
                other => other,
            };
            let (Ok(was), Ok(now)) = (values(old), values(device)) else {
                self.reload_device(project, device.id);
                return;
            };
            let slot = self.slot_of(device.id).expect("checked by the caller");
            for (id, (a, b)) in was.iter().zip(&now).enumerate() {
                if a != b {
                    self.send(Command::SetParam {
                        slot,
                        id: id as u32,
                        value: *b,
                    });
                }
            }
            if let Some(handle) = self.host.sampler(device.id) {
                use komponist_builtin::sampler::links;
                let old_links = links::read(old.state.as_deref());
                let new_links = links::read(device.state.as_deref());
                for (pad, link) in &new_links {
                    if !old_links.iter().any(|(p, l)| p == pad && l == link) {
                        handle.load_link(*pad, link.clone(), project_dir.map(Into::into));
                    }
                }
                for (pad, _) in &old_links {
                    if !new_links.iter().any(|(p, _)| p == pad) {
                        handle.clear(*pad);
                    }
                }
            }
        } else if let Some(state) = &device.state {
            self.host.set_state(device.id, state);
        }
    }

    /// A fast-path command for a change the dispatcher just applied inside a gesture
    /// (`SetTrackGain`, `SetTrackPan`, `SetTrackMute`, `SetTrackSolo`, `SetGates`,
    /// `SetMasterGain`):
    /// `before` and `after` are the document revisions around the change. The engine gets the
    /// command now; if the engine was up to date before, the recompile is deferred to the end of
    /// the gesture.
    pub fn fast_path(&mut self, command: Command, before: u64, after: u64) {
        if self.synced == Some(before) || self.fast == Some(before) {
            self.fast = Some(after);
        }
        self.send(command);
    }

    /// The fast-path gate table (`Command::SetGates`) of `project`'s mute and solo states for the
    /// program last sent (ARCHITECTURE §6.8.6): with buses, what solo silences follows the signal
    /// path, which only the routing graph knows. `None` before the first program.
    pub fn gates(&self, project: &Project) -> Option<Command> {
        let program = self.previous.as_ref()?;
        Some(Command::SetGates(compile::gates(project, program)))
    }

    /// Queues `command`; if the engine's queue is full (or an export has the engine) it is
    /// retried at the next [`EngineSync::poll`], keeping the order.
    pub fn send(&mut self, command: Command) {
        let Some(handle) = self.handle.as_mut().filter(|_| self.pending.is_empty()) else {
            self.pending.push_back(command);
            return;
        };
        if let Err(command) = handle.send(command) {
            self.pending.push_back(command);
        }
    }

    fn flush_pending(&mut self) {
        let Some(handle) = self.handle.as_mut() else {
            return;
        };
        while let Some(command) = self.pending.pop_front() {
            if let Err(command) = handle.send(command) {
                self.pending.push_front(command);
                break;
            }
        }
    }

    /// Call every UI frame: polls the audio system, retries queued commands, drains the engine's
    /// events (dropping garbage here) and the device host's events.
    pub fn poll(&mut self) -> Vec<SyncEvent> {
        self.audio.poll();
        if self.midi.is_some() && Instant::now() >= self.next_midi_scan {
            self.scan_midi();
        }
        self.flush_pending();
        let mut out = Vec::new();
        while let Some(event) = self.handle.as_mut().and_then(EngineHandle::poll) {
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

    /// Looks for MIDI devices that appeared or vanished and connects new ones (hot-plug), then
    /// restarts the [`MIDI_SCAN_INTERVAL`] timer. Returns whether the port list changed; the next
    /// [`EngineSync::sync`] then compiles the project again, because a track that names a device
    /// resolves it to a port at compile time (a device that was absent then receives nothing).
    pub fn scan_midi(&mut self) -> bool {
        self.next_midi_scan = Instant::now() + MIDI_SCAN_INTERVAL;
        let Some(hub) = self.midi.as_mut() else {
            return false;
        };
        let before = hub.ports();
        // A port that cannot be connected stays listed; the next scan tries again.
        let _ = hub.refresh();
        let after = hub.ports();
        let changed = after != before;
        self.midi_changes.extend(midi_changes(&before, &after));
        if changed {
            self.synced = None;
            self.fast = None;
        }
        changed
    }

    /// The devices that appeared or vanished in scans since the last call (the status bar says
    /// so). The scan at start-up is not among them: it runs before the first scan.
    pub fn take_midi_changes(&mut self) -> Vec<MidiChange> {
        std::mem::take(&mut self.midi_changes)
    }

    /// Rebuilds the stream for `config` (S07: driver, device, rate, buffer). The engine survives;
    /// a new rate re-activates the processors.
    pub fn set_audio(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        self.audio.start(config)
    }

    /// Parallel track processing (WP-65, settings file `worker_threads`): `None` = automatic
    /// ([`auto_workers`]), `Some(0)` = off, `Some(n)` = `n` worker threads. Takes the engine out
    /// of the stream for a moment (a short silence) if the count changes; during an export the
    /// change waits for its end. Returns the outcome of the worker start, if one happened.
    pub fn set_workers(
        &mut self,
        setting: Option<u32>,
    ) -> Result<Option<WorkerStatus>, AudioError> {
        if self.handle.is_none() {
            self.workers_after_export = Some(setting);
            return Ok(None);
        }
        match self.held.as_mut() {
            Some(process) => {
                process.apply_worker_setting(setting);
                process.apply_workers();
            }
            None => {
                let mut process = self.audio.detach(HOLD_TIMEOUT)?;
                process.apply_worker_setting(setting);
                // `attach` prepares the engine for the stream, which starts the workers.
                self.audio.attach(process);
            }
        }
        Ok(self.worker_status())
    }

    /// Outcome of the engine's last worker start (thread count, `SCHED_FIFO` refused, …).
    pub fn worker_status(&self) -> Option<WorkerStatus> {
        self.worker_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// True once per stream start whose workers run without realtime priority (the status bar
    /// says so; [`lacks_realtime`]).
    pub fn take_worker_notice(&self) -> bool {
        self.worker_notice.swap(false, Ordering::Relaxed)
    }

    /// True once per stream start whose workers could not join the audio workgroup (the status
    /// bar says so; [`lacks_workgroup`]).
    pub fn take_workgroup_notice(&self) -> bool {
        self.workgroup_notice.swap(false, Ordering::Relaxed)
    }

    /// The engine's worker pool counters (batches, jobs on workers, late waits).
    pub fn worker_stats(&self) -> &WorkerStats {
        &self.worker_stats
    }

    /// Stops playback, lets the engine drain, stops the stream (app exit).
    pub fn shutdown(&mut self) {
        self.send(Command::Stop);
        self.flush_pending();
        std::thread::sleep(SHUTDOWN_DRAIN);
        self.audio.stop();
        while self.handle.as_mut().and_then(EngineHandle::poll).is_some() {}
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

    /// Playhead. While an export has the engine: the playhead from before (the render moves the
    /// engine's own, but the transport stops during an export, ARCHITECTURE §6.7).
    pub fn playhead(&self) -> Tick {
        self.frozen.unwrap_or_else(|| self.telemetry.playhead())
    }

    /// Transport state; `Stopped` while an export has the engine.
    pub fn transport(&self) -> TransportState {
        if self.handle.is_none() {
            TransportState::Stopped
        } else {
            self.telemetry.transport()
        }
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
    /// A running recording stops (`StopRecording`; the app drops its take), and the engine's
    /// queued events (recorded notes, loop wraps, a count-in's end) are dropped: they belong to
    /// the old project. The host's events that are still queued and name a device are dropped
    /// too; the others (a finished scan, scan warnings) reach the next [`EngineSync::poll`].
    pub fn reset(&mut self) {
        self.send(Command::StopRecording);
        // Garbage among them is dropped here, on the UI thread.
        while self.handle.as_mut().and_then(EngineHandle::poll).is_some() {}
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

    /// Tests: takes the engine out of the stream (which goes on outputting silence), so that it
    /// runs only in [`EngineSync::run_held`] and positions are exact. Waits for the stream to hand
    /// it over. Fails while an export has the engine.
    pub fn hold_engine(&mut self) -> Result<(), AudioError> {
        if self.handle.is_none() {
            return Err(AudioError::Detached);
        }
        if self.held.is_none() {
            self.held = Some(self.audio.detach(HOLD_TIMEOUT)?);
        }
        Ok(())
    }

    /// Tests: runs the held engine for `frames` frames in blocks of 256, with live `midi` (frames
    /// counted from the start of this call, sorted) heard `output_latency` frames late, as a
    /// stream would. Returns `false` if no engine is held.
    pub fn run_held(&mut self, frames: usize, midi: &[MidiInEvent], output_latency: u32) -> bool {
        self.run_held_capture(frames, midi, output_latency)
            .is_some()
    }

    /// [`EngineSync::run_held`] that also returns what the engine rendered (left, right); `None`
    /// if no engine is held.
    pub fn run_held_capture(
        &mut self,
        frames: usize,
        midi: &[MidiInEvent],
        output_latency: u32,
    ) -> Option<(Vec<f32>, Vec<f32>)> {
        let process = self.held.as_mut()?;
        let mut out = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        let mut block_midi = Vec::with_capacity(midi.len());
        let mut done = 0;
        while done < frames {
            let n = HELD_BLOCK.min(frames - done).min(process.left.len());
            block_midi.clear();
            block_midi.extend(
                midi.iter()
                    .filter(|m| (done..done + n).contains(&(m.frame as usize)))
                    .map(|m| MidiInEvent {
                        frame: m.frame - done as u32,
                        ..*m
                    }),
            );
            let input = LiveInput {
                midi: &block_midi,
                output_latency,
            };
            let EngineProcess {
                engine,
                left,
                right,
                ..
            } = &mut **process;
            engine.process(&mut left[..n], &mut right[..n], &input);
            out.0.extend_from_slice(&left[..n]);
            out.1.extend_from_slice(&right[..n]);
            done += n;
        }
        Some(out)
    }

    /// Tests: gives the held engine back to the stream.
    pub fn release_engine(&mut self) {
        if let Some(process) = self.held.take() {
            self.audio.attach(process);
        }
    }

    /// Export (ARCHITECTURE §6.7): takes the engine out of the stream (which goes on outputting
    /// silence) together with its handle, for a worker thread. Queued commands are sent first,
    /// so the engine gets them before the export's own. Until [`EngineSync::attach`], commands
    /// wait in the pending queue and [`EngineSync::poll`] reports device host events only. A held
    /// engine ([`EngineSync::hold_engine`]) is taken from the hold and goes back there.
    pub fn detach(&mut self) -> Result<DetachedEngine, AudioError> {
        if self.handle.is_none() {
            return Err(AudioError::Detached);
        }
        self.flush_pending();
        let (process, held) = match self.held.take() {
            Some(process) => (process, true),
            None => (self.audio.detach(HOLD_TIMEOUT)?, false),
        };
        let handle = self.handle.take().expect("checked above");
        self.frozen = Some(self.telemetry.playhead());
        Ok(DetachedEngine {
            process,
            handle,
            held,
        })
    }

    /// Gives a [`DetachedEngine`] back: the handle to the UI thread, the engine to the stream
    /// (which re-activates the processors if the engine's rate differs from the stream's), and
    /// sends the commands that waited.
    pub fn attach(&mut self, engine: DetachedEngine) {
        let DetachedEngine {
            mut process,
            handle,
            held,
        } = engine;
        self.handle = Some(handle);
        self.frozen = None;
        if let Some(setting) = self.workers_after_export.take() {
            process.apply_worker_setting(setting);
            if held {
                process.apply_workers();
            }
        }
        if held {
            self.held = Some(process);
        } else {
            self.audio.attach(process);
        }
        self.flush_pending();
    }

    /// Whether an export has the engine ([`EngineSync::detach`]).
    pub fn is_detached(&self) -> bool {
        self.handle.is_none()
    }
}

/// The engine and its handle out of the stream, for an export on a worker thread
/// ([`EngineSync::detach`]; `komponist_engine::export` lists what the caller does before and
/// after). Back with [`EngineSync::attach`].
pub struct DetachedEngine {
    process: Box<EngineProcess>,
    handle: EngineHandle,
    /// Taken from [`EngineSync::hold_engine`]'s hold.
    held: bool,
}

impl DetachedEngine {
    /// The engine and its handle, as `komponist_engine::export::export` takes them.
    pub fn parts(&mut self) -> (&mut Engine, &mut EngineHandle) {
        (&mut self.process.engine, &mut self.handle)
    }

    /// The engine.
    pub fn engine(&mut self) -> &mut Engine {
        &mut self.process.engine
    }
}

impl core::fmt::Debug for DetachedEngine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DetachedEngine")
            .field("rate", &self.process.engine.sample_rate())
            .finish_non_exhaustive()
    }
}

impl Drop for EngineSync {
    fn drop(&mut self) {
        self.audio.stop();
    }
}

#[cfg(test)]
mod tests {
    use komponist_audio::MidiPortId;

    fn port(name: &str, connected: bool) -> MidiPortInfo {
        MidiPortInfo {
            id: MidiPortId(0),
            name: name.to_owned(),
            enabled: true,
            connected,
        }
    }

    #[test]
    fn midi_changes_compare_present_ports_by_name() {
        use MidiChange::{Appeared, Vanished};
        let a = || port("P-125", true);
        let b = || port("KeyStep", true);
        assert!(midi_changes(&[], &[]).is_empty());
        assert!(midi_changes(&[a()], &[a()]).is_empty());
        assert_eq!(midi_changes(&[], &[a()]), [Appeared("P-125".into())]);
        assert_eq!(
            midi_changes(&[a()], &[port("P-125", false)]),
            [Vanished("P-125".into())]
        );
        // A listed but absent port is no device; plugging it in again is an appearance.
        assert!(midi_changes(&[port("P-125", false)], &[port("P-125", false)]).is_empty());
        assert_eq!(
            midi_changes(&[port("P-125", false)], &[a()]),
            [Appeared("P-125".into())]
        );
        // Ids and the enabled switch do not matter.
        let mut moved = a();
        moved.id = MidiPortId(7);
        moved.enabled = false;
        assert!(midi_changes(&[a()], &[moved.clone()]).is_empty());
        assert_eq!(midi_changes(&[], &[moved]), [Appeared("P-125".into())]);
        // Several at once: vanished first.
        assert_eq!(
            midi_changes(&[a()], &[port("P-125", false), b()]),
            [Vanished("P-125".into()), Appeared("KeyStep".into())]
        );
    }

    use super::*;
    use crate::device_host::BuiltinHost;
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

    /// WP-65: the stream starts the automatic worker pool; the setting changes it (with the
    /// engine held, during an export and in the stream) and playback goes on.
    #[test]
    fn worker_threads_follow_the_setting() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        let realtime = Some(komponist_engine::workers::DEFAULT_WORKER_PRIORITY);
        let status = sync.worker_status().expect("prepared at stream start");
        // The automatic count depends on the machine: none on a small one (a 3-core CI VM before
        // #79 counted performance cores), and no pool has no priority (#94).
        assert_eq!(status.threads, auto_workers());
        let expected = if status.threads > 0 { realtime } else { None };
        assert_eq!(status.priority, expected, "{status:?}");
        let status = sync.set_workers(Some(2)).unwrap().unwrap();
        assert_eq!(status.threads, 2);
        assert_eq!(status.priority, realtime);
        let doc = Document::new(demo_project());
        sync.sync(&doc);
        sync.send(Command::Play);
        assert!(wait_until(|| {
            sync.poll();
            sync.worker_stats().worker_jobs() > 0
        }));
        assert_eq!(sync.set_workers(Some(0)).unwrap().unwrap().threads, 0);
        // Held engine: applied at once.
        sync.hold_engine().unwrap();
        assert_eq!(sync.set_workers(Some(1)).unwrap().unwrap().threads, 1);
        sync.release_engine();
        // During an export: applied when the engine comes back.
        let detached = sync.detach().unwrap();
        assert_eq!(sync.set_workers(Some(3)).unwrap(), None);
        sync.attach(detached);
        assert!(wait_until(|| sync
            .worker_status()
            .is_some_and(|s| s.threads == 3)));
        let before = sync.playhead();
        assert!(wait_until(|| {
            sync.poll();
            sync.playhead() > before
        }));
        sync.shutdown();
    }

    #[test]
    fn realtime_notice_only_for_running_workers_without_priority() {
        let status = |threads, realtime_error: Option<&str>| WorkerStatus {
            threads,
            priority: Some(komponist_engine::workers::DEFAULT_WORKER_PRIORITY),
            realtime_error: realtime_error.map(str::to_owned),
            ..WorkerStatus::default()
        };
        assert!(lacks_realtime(&status(3, Some("Operation not permitted"))));
        assert!(!lacks_realtime(&status(3, None)));
        assert!(!lacks_realtime(&status(0, Some("Operation not permitted"))));
        assert!(!lacks_realtime(&WorkerStatus::default()));
    }

    #[test]
    fn workgroup_notice_only_for_running_workers_that_could_not_join() {
        let status = |threads, workgroup_error: Option<&str>| WorkerStatus {
            threads,
            joined_workgroup: workgroup_error.is_none(),
            workgroup_error: workgroup_error.map(str::to_owned),
            ..WorkerStatus::default()
        };
        assert!(lacks_workgroup(&status(3, Some("Invalid argument"))));
        assert!(!lacks_workgroup(&status(3, None)));
        assert!(!lacks_workgroup(&status(0, Some("Invalid argument"))));
        // No workgroup given (null backend, Linux): nothing to say.
        assert!(!lacks_workgroup(&WorkerStatus::default()));
        // A refused priority alone is the other notice's business.
        assert!(!lacks_workgroup(&WorkerStatus {
            threads: 3,
            realtime_error: Some("Operation not permitted".into()),
            ..WorkerStatus::default()
        }));
    }

    /// Mute and solo on the fast path carry the gate table of the routing graph: soloing a group
    /// member keeps its group open (WP-102).
    #[test]
    fn mute_and_solo_gestures_send_the_routing_gates() {
        let mut sync = EngineSync::null(Box::new(BuiltinHost::new()));
        let mut doc = Document::new(demo_project());
        assert!(sync.gates(doc.project()).is_none());
        let (drums, bass) = (doc.project().tracks[0].id, doc.project().tracks[1].id);
        doc.apply("Gruppe", |p| {
            edits::add_bus(p, edits::NewBus::group(vec![drums, bass])).map(|(p, _)| p)
        })
        .unwrap();
        sync.sync(&doc);
        let program = sync.program().unwrap().clone();
        assert!(!program.routing.is_flat());
        doc.apply("Solo", |p| {
            edits::set_track_prop(p, &[bass], TrackProp::Channel(ChannelProp::Solo(true)))
        })
        .unwrap();
        let Some(Command::SetGates(gates)) = sync.gates(doc.project()) else {
            panic!("no gate table");
        };
        let index = |id| program.tracks.iter().position(|t| t.id == id).unwrap();
        let group = doc.project().tracks[0].id;
        assert!(doc.project().tracks[0].is_group());
        assert!(gates.output_open(index(bass)) && gates.output_open(index(group)));
        assert!(!gates.output_open(index(drums)));
        assert_eq!(gates, compile::gates(doc.project(), &program));
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
        let drums = first.project().tracks[0].channel.devices[0].id;
        assert_eq!(sync.device_status(drums), DeviceStatus::Ok);
        // A new document starts at revision 0 like the old one; it is compiled anyway, and its
        // devices (ids from 1 again) get fresh processors.
        let second = Document::new(demo_project());
        assert_eq!(second.revision(), first.revision());
        sync.sync(&second);
        let program = sync.program().expect("compiled again");
        assert_eq!(program.tracks.len(), second.project().tracks.len());
        // The Sampler runs in process.
        assert_eq!(sync.device_status(drums), DeviceStatus::Ok);
        // The compressor behind it is not installed: it stays missing.
        let compressor = second.project().tracks[0].channel.devices[1].id;
        assert_eq!(sync.device_status(compressor), DeviceStatus::Missing);
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
