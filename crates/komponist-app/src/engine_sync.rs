//! The UI thread's side of the engine (ARCHITECTURE §6.3, §7, §9.3 "EngineSync").
//!
//! [`EngineSync`] owns the audio system (which owns the engine inside the stream callback), the
//! engine's command/event queues, the program last sent to the engine and the [`DeviceHost`]
//! that makes processors. After each document change, [`EngineSync::sync`] compiles the project
//! against the program it actually sent (WP-24 note: `compile::build` and `slot_changes` must see
//! the same `previous`), inserts the processors of new slots, installs the program, then removes
//! the processors of vanished slots — in that order, so a slot is filled before a program uses it
//! and emptied only after the program that used it is gone. If a replacement needs a slot freed
//! by the first program, a second program installs it after the removal.
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_audio::{
    AudioError, AudioProcess, AudioState, AudioSystem, CallbackInfo, HostKind, MidiIn,
    MidiInputHub, MidiPortInfo, MidiReceiver, OutputBuffer, StreamConfig, StreamParams,
    StreamStats,
};
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::processor::ParameterEditKind;
use komponist_engine::workers::{
    WorkerRt, WorkerStats, WorkerStatus, auto_workers, cap_to_workgroup,
};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, LiveInput, MAX_BLOCK,
    MAX_EVENTS_PER_BLOCK, MAX_SLOTS, MidiInEvent, MidiPort, Program, SlotId, Telemetry, TrackMeter,
    TransportState,
};
use komponist_model::{DeviceId, Document, Project, Tick, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent, ProcessorSetup};

use crate::device_host::{DeviceHost, LegacyParameters};

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
        self.process_input(out, info, midi, &komponist_audio::InputBuffer::empty());
    }

    fn process_input(
        &mut self,
        out: &mut OutputBuffer<'_>,
        info: &CallbackInfo,
        midi: &mut MidiIn<'_>,
        audio: &komponist_audio::InputBuffer<'_>,
    ) {
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
        // JACK reports whole frames. Converting via Duration truncates to nanoseconds, so
        // round on the way back to avoid losing another frame at common sample rates.
        let latency_frames = |duration: Duration| {
            (duration.as_secs_f64() * f64::from(info.sample_rate)).round() as u32
        };
        let input = LiveInput {
            midi: &self.midi,
            output_latency: latency_frames(info.output_latency),
        };
        let (left, right) = (&mut self.left[..frames], &mut self.right[..frames]);
        self.engine.process_with_audio(
            left,
            right,
            &input,
            &komponist_engine::AudioInputBuffer {
                data: audio.data(),
                channels: audio.channels(),
                latency: latency_frames(audio.latency()),
                discontinuous: audio.discontinuous(),
                routes: audio
                    .routes()
                    .map(|routes| komponist_engine::AudioInputRoutes {
                        latencies: routes.latencies,
                        available: routes.available,
                        discontinuous: routes.discontinuous,
                        history: routes.history,
                        history_cursor: routes.history_cursor,
                        history_valid: routes.history_valid,
                    }),
            },
        );
        out.write_stereo(left, right);
    }

    fn is_poisoned(&self) -> bool {
        self.engine.is_poisoned()
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
    /// The stream's sample rate: audio clips are compiled to frames of it.
    rate: f64,
}

impl CompileEnv for Env<'_> {
    fn midi_port(&self, device_name: &str) -> Option<MidiPort> {
        self.midi?.port_id(device_name).map(|id| MidiPort(id.0))
    }

    fn device_latency(&self, device: komponist_model::DeviceId) -> u32 {
        self.host.latency(device)
    }

    fn automation_parameter(
        &self,
        device: &komponist_model::Device,
        param: &komponist_model::params::ParamId,
    ) -> Option<komponist_engine::automation::Parameter> {
        use komponist_engine::automation::{Parameter, PlainMap};
        if let Some(parameters) = LegacyParameters::new(device) {
            let p = parameters.get(komponist_i18n::Lang::default(), param)?;
            return p.info.automatable.then(|| Parameter {
                wire_id: p.wire,
                map: PlainMap::from_kind(&p.info.kind),
                base: p.value,
            });
        }
        if matches!(
            self.host.status(device.id),
            DeviceStatus::Missing | DeviceStatus::Crashed(_)
        ) {
            return None;
        }
        let wire_id = param.plugin_id()?;
        let info = self
            .host
            .params(device.id)?
            .iter()
            .find(|p| p.id == wire_id)?;
        komponist_plugins::params::is_automatable(info).then(|| Parameter {
            wire_id,
            map: PlainMap::linear(info.min, info.max, info.step_count),
            base: self
                .host
                .param_base(device.id, wire_id)
                .unwrap_or(info.default),
        })
    }

    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn project_dir(&self) -> Option<std::path::PathBuf> {
        self.host.project_dir()
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

#[derive(Clone, Copy)]
pub(crate) enum MacroValueEdit {
    Base(f32),
    AutomationOverride(f32),
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
    /// What `previous` leaves out of its project (#92), in project order.
    diagnostics: Vec<compile::Diagnostic>,
    /// A sync left out something the one before did not; the app takes it once
    /// ([`EngineSync::take_diagnostics_notice`]).
    diagnostics_notice: bool,
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
    /// Last plain macro outputs passed to the plugin snapshot verifier.
    macro_params: Vec<(DeviceId, u32, f64)>,
    macro_edit: u64,
    pending_macro_edits: HashMap<(TrackId, u8), (u64, MacroValueEdit)>,
    rate: Arc<AtomicU32>,
    telemetry: Arc<Telemetry>,
    /// The engine taken out of the stream ([`EngineSync::hold_engine`]).
    held: Option<Box<EngineProcess>>,
    /// The playhead when an export took the engine ([`EngineSync::detach`]): shown meanwhile.
    frozen: Option<Tick>,
    /// Worker threads asked for while an export had the engine (applied at [`EngineSync::attach`]).
    workers_after_export: Option<Option<u32>>,
    /// The worker setting last asked for ([`EngineSync::set_workers`]; `None` is automatic), kept
    /// here because a replacement engine that is not derived from a surviving one needs it
    /// ([`EngineSync::recover`]).
    worker_setting: Option<u32>,
    /// A replacement engine's worker start is not reported: the crash message outranks it
    /// ([`EngineSync::recover`]). Ends once the new engine has prepared.
    quiet_worker_notices: std::cell::Cell<bool>,
    /// The engine came back poisoned and a new one replaced it; the app takes it once
    /// ([`EngineSync::take_rebuild_notice`]): the playhead when it happened.
    rebuild_notice: Option<Tick>,
    /// Where a rebuilt engine's transport goes once the program is in it (#210).
    locate_after_rebuild: Option<Tick>,
    /// Events the poisoned engine had queued before it was replaced, for the next
    /// [`EngineSync::poll`] (recorded notes up to the crash).
    stranded: Vec<EngineEvent>,
    /// Outcome of the engine's last worker start ([`EngineProcess`] writes it in `prepare`).
    worker_status: Arc<Mutex<Option<WorkerStatus>>>,
    /// See [`EngineProcess`]'s `worker_notice`.
    worker_notice: Arc<AtomicBool>,
    /// See [`EngineProcess`]'s `workgroup_notice`.
    workgroup_notice: Arc<AtomicBool>,
    worker_stats: Arc<WorkerStats>,
    #[cfg(test)]
    test_jack_capture_sources: Option<Vec<komponist_audio::JackCaptureChannel>>,
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
            diagnostics: Vec::new(),
            diagnostics_notice: false,
            synced: None,
            programs_sent: 0,
            fast: None,
            reloads: 0,
            pending: VecDeque::new(),
            carried: Vec::new(),
            macro_params: Vec::new(),
            macro_edit: 0,
            pending_macro_edits: HashMap::new(),
            rate,
            telemetry,
            held: None,
            frozen: None,
            workers_after_export: None,
            worker_setting: None,
            quiet_worker_notices: std::cell::Cell::new(false),
            rebuild_notice: None,
            locate_after_rebuild: None,
            stranded: Vec::new(),
            worker_status,
            worker_notice,
            workgroup_notice,
            worker_stats,
            #[cfg(test)]
            test_jack_capture_sources: None,
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
        self.sync_macro_params();
        let next = self.compile_project(project);
        let changes = compile::slot_changes(self.previous.as_ref(), &next.program);
        // The first build reserves the slots it removes. If that exhausts the table, see
        // whether the released slots can now take devices excluded from the first program.
        let check_freed_slots = !changes.removed.is_empty()
            && next.program.slots.entries().len() + changes.removed.len() == MAX_SLOTS;
        self.send_compiled_project(project, next, changes);
        if check_freed_slots {
            let next = self.compile_project(project);
            let changes = compile::slot_changes(self.previous.as_ref(), &next.program);
            if !changes.added.is_empty() {
                self.send_compiled_project(project, next, changes);
            }
        }
    }

    fn compile_project(&self, project: &Project) -> compile::CompileResult {
        let env = Env {
            midi: self.midi.as_ref(),
            host: &*self.host,
            rate: f64::from(self.sample_rate()),
        };
        compile::build_with_diagnostics(project, self.previous.as_ref(), &env)
    }

    fn send_compiled_project(
        &mut self,
        project: &Project,
        mut next: compile::CompileResult,
        changes: compile::SlotChanges,
    ) {
        let setup = self.setup();
        let mut new_processor_metadata = false;
        for (device, slot) in changes.added {
            let Some((_, _, device)) = project.find_device(device) else {
                continue;
            };
            if let Some(processor) = self.host.create_processor(device, setup) {
                self.host
                    .prepare_device_assets(device, project.tempo.bpm, None);
                new_processor_metadata |= processor.latency() > 0
                    || self
                        .host
                        .params(device.id)
                        .is_some_and(|params| !params.is_empty());
                self.send(Command::InsertProcessor { slot, processor });
            }
        }
        if new_processor_metadata {
            // Latency and CLAP parameter metadata can first become available on insertion.
            // Compile the automation and compensation with those descriptors before publishing
            // the program, including when export detaches it before the next host poll.
            // Keep slots whose removals have not yet been queued reserved.
            next = self.compile_project(project);
        }
        let values = self.macro_parameters(&next.program);
        if values != self.macro_params {
            let retained: HashSet<_> = values.iter().map(|&(device, id, _)| (device, id)).collect();
            let released = self.macro_params.iter().any(|&(device, id, _)| {
                next.program.slots.slot(device).is_some() && !retained.contains(&(device, id))
            });
            self.set_macro_params(values);
            if released {
                // Unmapping makes the last controlled value the plugin's ordinary base. Any
                // automation lane that becomes active again must compile against that base.
                next = self.compile_project(project);
            }
        }
        if self
            .previous
            .as_ref()
            .is_some_and(|p| p.tempo_map.tempo != project.tempo)
        {
            for device in project.tracks.iter().flat_map(|t| &t.channel.devices) {
                self.host
                    .prepare_device_assets(device, project.tempo.bpm, None);
            }
        }
        let compile::CompileResult {
            program: next,
            diagnostics,
        } = next;
        self.host
            .set_automated_params(&Self::automated_parameters(&next, project));
        self.send(Command::SetProgram(Box::new(next.clone())));
        // A rebuilt engine stays stopped where the old one was; ticks map to frames only with
        // the program's tempo map, so after it.
        if let Some(at) = self.locate_after_rebuild.take() {
            self.send(Command::Locate(at));
        }
        self.programs_sent += 1;
        self.previous = Some(next);
        self.diagnostics_notice |= diagnostics.iter().any(|d| !self.diagnostics.contains(d));
        self.diagnostics = diagnostics;
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
            self.host.prepare_device_assets(d, project.tempo.bpm, None);
            self.reloads += 1;
            self.send(Command::InsertProcessor { slot, processor });
            self.host.set_macro_params(&self.macro_params);
            if let Some(program) = &self.previous {
                self.host
                    .set_automated_params(&Self::automated_parameters(program, project));
            }
        } else if self.host.status(device) == DeviceStatus::Missing {
            self.send(Command::RemoveProcessor { slot });
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
    /// A reference processor can follow parameter and embedded asset edits in place. Hosted
    /// plugins restore their opaque saved state through the generic state interface.
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
            if Arc::ptr_eq(old, device) {
                continue;
            }
            if let Some(slot) = self.slot_of(device.id) {
                if old.plugin != device.plugin {
                    // Migration can retain the device ID while changing its host and state format.
                    self.send(Command::RemoveProcessor { slot });
                    self.host.release(device.id);
                    self.reload_device(after, device.id);
                } else {
                    let state_changed =
                        old.state != device.state || old.parameter_bases != device.parameter_bases;
                    if state_changed {
                        self.follow_state(old, device, after, project_dir);
                    }
                    if state_changed || old.asset_links != device.asset_links {
                        self.host.sync_asset_links(
                            device.id,
                            &old.asset_links,
                            &device.asset_links,
                        );
                    }
                }
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
        if let Some(parameters) = LegacyParameters::new(device) {
            let Some(changes) = parameters.changes_from(old.state.as_deref()) else {
                self.reload_device(project, device.id);
                return;
            };
            let slot = self.slot_of(device.id).expect("checked by the caller");
            for (id, value) in changes {
                self.send(Command::SetParam { slot, id, value });
            }
            self.host
                .follow_embedded_assets(old, device, project.tempo.bpm, project_dir);
            self.reassert_device_macros(device.id);
        } else if let Some(state) = device.saved_state() {
            if !self.host.restore_saved_state(device.id, &state)
                && komponist_plugins::requires_bundled(&device.plugin)
                && self.host.status(device.id) == DeviceStatus::Loading
            {
                // A replacement bridge is still loading the previous revision (for example
                // Redo immediately after Undo recreated the default instance).
                self.host.release(device.id);
                self.reload_device(project, device.id);
            }
        } else if komponist_plugins::requires_bundled(&device.plugin) {
            // Undo may restore the new instance's absent state. A regular reload preserves
            // the previous processor's snapshot, so start a fresh instance at its defaults.
            let slot = self.slot_of(device.id).expect("checked by the caller");
            self.send(Command::RemoveProcessor { slot });
            self.host.release(device.id);
            self.reload_device(project, device.id);
        }
    }

    fn reassert_device_macros(&mut self, device: DeviceId) {
        use komponist_model::automation::AutomationTarget;
        let owner = self.previous.as_ref().and_then(|program| {
            program.tracks.iter().find(|track| {
                track.source.macros.0.iter().any(|mac| {
                    mac.mappings.iter().any(|mapping| {
                        matches!(mapping.target, AutomationTarget::Param { device: id, .. } if id == device)
                    })
                })
            })
        });
        if let Some(track) = owner.map(|track| track.id) {
            self.send(Command::MacroReassert { track });
        }
    }

    fn sync_macro_params(&mut self) {
        if let Some(program) = &self.previous {
            let values = self.macro_parameters(program);
            self.set_macro_params(values);
        }
    }

    /// Refresh the model positions used for state snapshots after a fast-path macro edit.
    pub(crate) fn refresh_snapshot_automation(&mut self, project: &Project) {
        if let Some(program) = &self.previous {
            self.host
                .set_automated_params(&Self::automated_parameters(program, project));
        }
    }

    fn automated_parameters(
        program: &Program,
        project: &Project,
    ) -> Vec<(DeviceId, u32, Option<f64>)> {
        let mut values = Vec::new();
        program.for_each_automated_parameter(
            |track, index| Some(project.track(track)?.macros.0[usize::from(index)].value),
            |device, id, base| values.push((device, id, base)),
        );
        values
    }

    fn macro_parameters(&self, program: &Program) -> Vec<(DeviceId, u32, f64)> {
        let mut values = Vec::new();
        program.for_each_macro_parameter(
            |track| self.telemetry.macro_values(track),
            |slot, id, value| {
                if let Some(device) = program.slots.device(slot)
                    && self.host.params(device).is_some()
                {
                    values.push((device, id, value));
                }
            },
        );
        values
    }

    fn set_macro_params(&mut self, values: Vec<(DeviceId, u32, f64)>) {
        if values != self.macro_params {
            self.host.set_macro_params(&values);
            self.macro_params = values;
        }
    }

    /// Starts tracking a manual value edit until the engine publishes its effect.
    pub(crate) fn macro_value_command(
        &mut self,
        track: TrackId,
        index: u8,
        change: MacroValueEdit,
    ) -> Command {
        self.macro_edit += 1;
        let edit = self.macro_edit;
        self.pending_macro_edits
            .insert((track, index), (edit, change));
        let value = match change {
            MacroValueEdit::Base(value) | MacroValueEdit::AutomationOverride(value) => value,
        };
        Command::SetMacro {
            track,
            index,
            value,
            edit,
        }
    }

    pub(crate) fn pending_macro_value(&self, track: TrackId, index: u8, base: f32) -> Option<f32> {
        let &(edit, change) = self.pending_macro_edits.get(&(track, index))?;
        if edit <= self.telemetry.applied_macro_edit() {
            return None;
        }
        match change {
            // Undo or cancellation may have already replaced the pending base in the document.
            MacroValueEdit::Base(value) => (value == base).then_some(value),
            MacroValueEdit::AutomationOverride(value) => Some(value),
        }
    }

    pub(crate) fn discard_pending_macro_overrides(&mut self) {
        self.pending_macro_edits
            .retain(|_, (_, change)| matches!(change, MacroValueEdit::Base(_)));
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
        if let Command::Locate(at) = command
            && self.locate_after_rebuild.is_some()
        {
            // A newer position wins over the one a rebuilt engine returns to. It is still sent
            // now, but only the one after the first program (which knows the tempo map) holds.
            self.locate_after_rebuild = Some(at);
        }
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
        let mut events: Vec<_> = self
            .poll_engine_events()
            .into_iter()
            .map(|event| {
                if let EngineEvent::PluginParams {
                    slot,
                    device,
                    ref events,
                } = event
                    && self.plugin_parameter_slot_matches(slot, device)
                {
                    for (change, temporary) in events.iter() {
                        self.update_plugin_parameter(device, change.kind, temporary, false);
                    }
                }
                SyncEvent::Engine(event)
            })
            .collect();
        events.extend(self.poll_device_events().into_iter().map(SyncEvent::Device));
        events
    }

    pub(crate) fn plugin_parameter_slot_matches(&self, slot: SlotId, device: DeviceId) -> bool {
        self.program()
            .is_some_and(|program| program.slots.slot(device) == Some(slot))
    }

    pub(crate) fn plugin_mapping_parameter(&self, device: DeviceId, id: u32) -> bool {
        self.host.params(device).is_some_and(|params| {
            params
                .iter()
                .any(|info| info.id == id && komponist_plugins::params::is_automatable(info))
        })
    }

    pub(crate) fn update_plugin_parameter(
        &mut self,
        device: DeviceId,
        edit: ParameterEditKind,
        temporary: bool,
        learning: bool,
    ) -> Option<f64> {
        let id = match edit {
            ParameterEditKind::Value { id, .. } | ParameterEditKind::Gesture { id, .. } => id,
        };
        let previous = if learning {
            self.macro_params
                .iter()
                .find(|&&(target, wire, _)| target == device && wire == id)
                .map(|&(_, _, value)| value)
                .or_else(|| self.host.param_value(device, id).map(|p| p.value))
                .or_else(|| self.host.param_base(device, id))
        } else {
            None
        };
        if let ParameterEditKind::Value { value, .. } = edit {
            if temporary || learning {
                self.host.preview_param(device, id, value);
            } else {
                self.host.edited_param(device, id, value);
            }
        }
        previous
    }

    pub(crate) fn poll_engine_events(&mut self) -> Vec<EngineEvent> {
        let before = self.sample_rate();
        self.audio.poll();
        if let Some(poisoned) = self.audio.take_poisoned() {
            self.rebuild_engine(Some(*poisoned));
        }
        if self.sample_rate() != before {
            self.synced = None;
            self.fast = None;
        }
        if self.midi.is_some() && Instant::now() >= self.next_midi_scan {
            self.scan_midi();
        }
        self.flush_pending();
        let applied_edit = self.telemetry.applied_macro_edit();
        self.pending_macro_edits
            .retain(|_, (edit, _)| *edit > applied_edit);
        self.sync_macro_params();
        let mut out = std::mem::take(&mut self.stranded);
        while let Some(event) = self.handle.as_mut().and_then(EngineHandle::poll) {
            if !matches!(event, EngineEvent::Retired(_)) {
                out.push(event);
            }
        }
        out
    }

    pub(crate) fn poll_device_events(&mut self) -> Vec<PluginManagerEvent> {
        let mut events = std::mem::take(&mut self.carried);
        events.extend(self.host.poll());
        for event in &events {
            if let PluginManagerEvent::StateApplied { device } = *event {
                if let Some(slot) = self.slot_of(device) {
                    self.send(Command::AutomationReassert { slot });
                }
                self.reassert_device_macros(device);
            }
        }
        events
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

    /// Tests: a MIDI change and a new compile diagnostic waiting for the app, as a scan and a
    /// sync leave them.
    #[cfg(test)]
    pub(crate) fn queue_notices(&mut self, change: MidiChange, diagnostic: compile::Diagnostic) {
        self.midi_changes.push(change);
        self.diagnostics.push(diagnostic);
        self.diagnostics_notice = true;
    }

    /// The devices that appeared or vanished in scans since the last call (the status bar says
    /// so). The scan at start-up is not among them: it runs before the first scan.
    pub fn take_midi_changes(&mut self) -> Vec<MidiChange> {
        std::mem::take(&mut self.midi_changes)
    }

    /// Rebuilds the stream for `config` (S07: driver, device, rate, buffer). The engine survives;
    /// a new rate re-activates the processors.
    pub fn set_audio(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        let changed = match self.audio.state() {
            AudioState::Running { config: old, .. } | AudioState::NoDevice { config: old, .. } => {
                old != config
            }
            AudioState::Stopped => false,
        };
        if changed {
            self.send(Command::AbortAudioRecording);
        }
        let before = self.sample_rate();
        let started = self.audio.start(config);
        if self.sample_rate() != before {
            // Audio clips are compiled to frames of the rate: the program is compiled again.
            self.synced = None;
            self.fast = None;
        }
        started
    }

    /// Explicitly configures capture; absent by default so old projects never open inputs.
    pub fn set_audio_input(
        &mut self,
        config: Option<komponist_audio::InputConfig>,
    ) -> Result<(), AudioError> {
        if self.audio.input() != config.as_ref() {
            self.send(Command::AbortAudioRecording);
        }
        self.audio.set_input(config)
    }
    /// Synthetic capture on the null backend, without enumerating or opening hardware.
    pub fn set_test_audio_input(
        &mut self,
        channels: u16,
        rate: u32,
    ) -> Result<komponist_audio::TestAudioSource, AudioError> {
        self.send(Command::AbortAudioRecording);
        self.audio.set_test_input(channels, rate)
    }

    /// Requested capture configuration, independent of per-track channel selection.
    pub fn audio_input_config(&self) -> Option<&komponist_audio::InputConfig> {
        self.audio.input()
    }
    /// Why the capture stream is unavailable while playback is running.
    pub fn audio_input_error(&self) -> Option<&AudioError> {
        self.audio.input_error()
    }
    /// Input channels currently available from the configured device.
    pub fn audio_input_channels(&self) -> u16 {
        self.audio.input_channels()
    }
    /// Whether the selected hardware lane or stereo pair is available on the open stream.
    pub fn hardware_input_available(&self, input: komponist_model::HardwareInput) -> bool {
        #[cfg(test)]
        if let Some(sources) = self.test_jack_capture_sources.as_ref() {
            let first = usize::from(input.first);
            return sources
                .get(first..first + usize::from(input.channel_count()))
                .is_some_and(|ports| ports.iter().all(|port| port.connected));
        }
        self.audio
            .input_route_available(input.first, input.channel_count())
    }

    /// Verified native JACK master links, in left/right order.
    pub fn jack_playback_connections(&self) -> Option<[bool; 2]> {
        self.audio.jack_playback_connections()
    }

    /// Source port metadata for the capture channel order of the active JACK route.
    pub fn jack_capture_sources(&self) -> Option<&[komponist_audio::JackCaptureChannel]> {
        #[cfg(test)]
        if let Some(sources) = self.test_jack_capture_sources.as_deref() {
            return Some(sources);
        }
        self.audio.jack_capture_sources()
    }

    #[cfg(test)]
    pub(crate) fn set_test_jack_capture_sources(
        &mut self,
        sources: Vec<komponist_audio::JackCaptureChannel>,
    ) {
        self.test_jack_capture_sources = Some(sources);
    }

    /// Metadata-only input enumeration for the active host (no stream is opened).
    pub fn audio_input_devices(&self) -> Result<Vec<komponist_audio::InputDeviceInfo>, AudioError> {
        let host = match self.audio.state() {
            AudioState::Running { config, .. } | AudioState::NoDevice { config, .. } => config.host,
            AudioState::Stopped => HostKind::Null,
        };
        AudioSystem::<EngineProcess>::input_devices(host)
    }

    /// Parallel track processing (WP-65, settings file `worker_threads`): `None` = automatic
    /// ([`auto_workers`]), `Some(0)` = off, `Some(n)` = `n` worker threads. Takes the engine out
    /// of the stream for a moment (a short silence) if the count changes; during an export the
    /// change waits for its end. Returns the outcome of the worker start, if one happened.
    pub fn set_workers(
        &mut self,
        setting: Option<u32>,
    ) -> Result<Option<WorkerStatus>, AudioError> {
        self.worker_setting = setting;
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
        let notice = self.worker_notice.swap(false, Ordering::Relaxed);
        notice && !self.quiet_worker_notices.get()
    }

    /// True once per stream start whose workers could not join the audio workgroup (the status
    /// bar says so; [`lacks_workgroup`]).
    ///
    /// Called after [`EngineSync::take_worker_notice`]; ends the quiet period of a replacement
    /// engine once it has prepared.
    pub fn take_workgroup_notice(&self) -> bool {
        let notice = self.workgroup_notice.swap(false, Ordering::Relaxed);
        let quiet = self.quiet_worker_notices.get();
        if quiet && self.worker_status().is_some() {
            self.quiet_worker_notices.set(false);
        }
        notice && !quiet
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

    /// The tracks and devices the program last sent leaves out of its project, and why (#92), in
    /// project order; empty before the first sync.
    pub fn diagnostics(&self) -> &[compile::Diagnostic] {
        &self.diagnostics
    }

    /// Whether a sync since the last call left out a track or device that the program before it
    /// played (or a known one for another reason, or at another position): the app tells it in
    /// the status bar.
    pub fn take_diagnostics_notice(&mut self) -> bool {
        std::mem::take(&mut self.diagnostics_notice)
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
        self.macro_params.clear();
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
        // A rebuilt engine's old position and events belong to the old project.
        self.locate_after_rebuild = None;
        self.stranded.clear();
        // The next project's diagnostics are all new, even those equal to the old project's.
        self.diagnostics.clear();
        self.diagnostics_notice = false;
        self.macro_params.clear();
        self.pending_macro_edits.clear();
        let events = self.host.poll();
        self.carried.extend(events.into_iter().filter(|e| {
            matches!(
                e,
                PluginManagerEvent::ScanFinished(_) | PluginManagerEvent::Log { device: None, .. }
            )
        }));
    }

    /// Replaces an engine that came back poisoned from the stream (a panic unwound its
    /// `process`, #210; [`AudioSystem::take_poisoned`]): its state is half-updated, so it is
    /// dropped here, on the UI thread, with its processors, and a new engine at the same rate and
    /// with the same worker setting takes its place in the stream. The program is forgotten, so
    /// the next [`EngineSync::sync`] compiles the document from scratch and every device gets a
    /// fresh processor (a bridged plugin a fresh bridge with its last known state). The transport
    /// stays stopped at the old playhead (located there with the first program); commands that
    /// waited for the old engine are dropped, the events it had queued reach the next poll. The
    /// app reports it once and ends a running take there ([`EngineSync::take_rebuild_notice`]).
    ///
    /// `None`: the engine is lost altogether (its export worker died, [`EngineSync::recover`]);
    /// the new one runs at the stream's rate with the worker setting last asked for.
    fn rebuild_engine(&mut self, poisoned: Option<EngineProcess>) {
        let sample_rate = poisoned.as_ref().map_or_else(
            || self.rate.load(Ordering::Relaxed) as f32,
            |p| p.engine.sample_rate(),
        );
        let (engine, handle) = Engine::new(EngineConfig {
            sample_rate,
            ..EngineConfig::default()
        });
        let fresh = EngineProcess::new(engine, Arc::clone(&self.rate));
        let mut process = match poisoned {
            Some(EngineProcess {
                workers,
                workers_auto,
                rt,
                worker_status,
                worker_notice,
                workgroup_notice,
                ..
            }) => EngineProcess {
                workers,
                workers_auto,
                rt,
                worker_status,
                worker_notice,
                workgroup_notice,
                ..fresh
            },
            None => {
                let mut fresh = fresh;
                fresh.apply_worker_setting(self.worker_setting);
                fresh
            }
        };
        self.worker_status = Arc::clone(&process.worker_status);
        self.worker_notice = Arc::clone(&process.worker_notice);
        self.workgroup_notice = Arc::clone(&process.workgroup_notice);
        // A worker setting asked for during an export applies to the new engine, as at `attach`.
        if let Some(setting) = self.workers_after_export.take() {
            process.apply_worker_setting(setting);
        }
        let playhead = self
            .frozen
            .take()
            .unwrap_or_else(|| self.telemetry.playhead());
        self.worker_stats = process.engine.worker_stats();
        self.telemetry = Arc::clone(handle.telemetry());
        // The old handle goes with its queues; garbage in them is dropped here, the rest is kept.
        if let Some(mut old) = self.handle.replace(handle) {
            while let Some(event) = old.poll() {
                if !matches!(event, EngineEvent::Retired(_)) {
                    self.stranded.push(event);
                }
            }
        }
        self.pending.clear();
        self.previous = None;
        self.synced = None;
        self.fast = None;
        self.macro_params.clear();
        self.pending_macro_edits.clear();
        self.rebuild_notice = Some(playhead);
        self.locate_after_rebuild = Some(playhead);
        // `prepare` starts the workers for the running stream.
        self.audio.attach(Box::new(process));
    }

    /// An export worker panicked or died ([`EngineSync::detach`]): replaces the engine with a new
    /// one built from the document, the rebuild of [`AudioSystem::take_poisoned`] (#210). `engine`
    /// is what came back from the worker, if anything: its state is unknown, so it is dropped
    /// here, on the UI thread, never reattached; its handle is drained like a poisoned engine's.
    /// Commands that waited for it are dropped, so the queue cannot grow while nothing runs.
    /// Reported through [`EngineSync::take_rebuild_notice`].
    pub fn recover(&mut self, engine: Option<DetachedEngine>) {
        self.held = None;
        // The new pool's start is silent until the engine has prepared (`worker_status` is set by
        // that); the caller reports the crash.
        *self.worker_status.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.quiet_worker_notices.set(true);
        match engine {
            Some(DetachedEngine {
                process, handle, ..
            }) => {
                self.handle = Some(handle);
                self.rebuild_engine(Some(*process));
            }
            None => self.rebuild_engine(None),
        }
    }

    /// The playhead at which the engine was rebuilt after a panic, if it was since the last call
    /// (status bar notice; a running take ends there).
    pub fn take_rebuild_notice(&mut self) -> Option<Tick> {
        self.rebuild_notice.take()
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
        self.synced = None;
        self.fast = None;
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
    use crate::app::App;
    use komponist_model::edits::{self, ChannelProp, TrackProp};
    use komponist_model::fixtures::demo_project;
    use std::time::Instant;

    fn test_song(host: &dyn DeviceHost) -> Project {
        let mut project = demo_project();
        for (index, plugin) in [
            (0, "org.klang-studio.komponist.sampler"),
            (1, "org.klang-studio.komponist.hexa"),
            (2, "org.klang-studio.komponist.hexa"),
        ] {
            let instrument = host
                .catalog()
                .entries()
                .iter()
                .find(|entry| entry.plugin.id == plugin)
                .unwrap_or_else(|| {
                    panic!("build fixture {plugin} with scripts/build-test-plugins.sh")
                });
            let device = &mut Arc::make_mut(&mut project.tracks[index]).channel.devices[0];
            Arc::make_mut(device).plugin = instrument.plugin.clone();
        }
        project
    }

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
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        assert!(matches!(sync.audio_state(), AudioState::Running { .. }));
        let doc = Document::new(test_song(sync.host()));
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

    /// #210: an engine whose `process` unwound is not given back to the stream; a new one is
    /// built at the same rate and worker setting, the document is compiled into it from scratch,
    /// its transport stays stopped at the old playhead, the old engine's queued events still
    /// arrive, and the app is told once (with that playhead).
    #[test]
    fn a_poisoned_engine_is_rebuilt_from_the_document() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        // Single-threaded: the panic must happen on this thread (a worker runs its jobs inside
        // `assert_no_alloc`, where the panic's allocation aborts in debug builds).
        assert_eq!(sync.set_workers(Some(0)).unwrap().unwrap().threads, 0);
        let doc = Document::new(test_song(sync.host()));
        sync.sync(&doc);
        let lead = doc.project().tracks[2].channel.devices[0].id;
        let slot = sync.program().unwrap().slots.slot(lead).unwrap();
        let sent = sync.programs_sent();
        sync.hold_engine().unwrap();
        // Play a while, so the crash happens away from 0.
        sync.send(Command::Play);
        assert!(sync.run_held(48_000, &[], 0));
        let at = sync.playhead();
        assert!(at > Tick::ZERO);
        sync.send(Command::InsertProcessor {
            slot,
            processor: Box::new(komponist_engine::testing::TestPanic),
        });
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sync.run_held(256, &[], 0);
        }));
        assert!(caught.is_err(), "the processor ran");
        assert!(sync.held.as_ref().unwrap().engine.is_poisoned());
        // Back to the stream: refused, rebuilt at the next poll.
        sync.release_engine();
        assert_eq!(sync.take_rebuild_notice(), None);
        let events = sync.poll_engine_events();
        assert_eq!(sync.take_rebuild_notice(), Some(at));
        assert_eq!(sync.take_rebuild_notice(), None, "taken once");
        // The old engine's queued garbage stays behind; nothing else was queued here.
        assert!(events.iter().all(|e| !matches!(e, EngineEvent::Retired(_))));
        assert!(sync.program().is_none(), "the old program is forgotten");
        assert_eq!(
            sync.worker_status().unwrap().threads,
            0,
            "the setting stays"
        );
        sync.sync(&doc);
        assert_eq!(sync.programs_sent(), sent + 1);
        assert!(sync.program().unwrap().slots.slot(lead).is_some());
        // Stopped where the old engine was.
        assert!(wait_until(|| {
            sync.poll();
            sync.playhead() == at
        }));
        assert_eq!(sync.transport(), TransportState::Stopped);
        // The new engine runs in the stream and plays the document from there.
        sync.send(Command::Play);
        assert!(wait_until(|| {
            sync.poll();
            sync.playhead() > at
        }));
        sync.hold_engine().unwrap();
        assert!(!sync.held.as_ref().unwrap().engine.is_poisoned());
        sync.release_engine();
        sync.shutdown();
    }

    /// #211: an export worker that died with the engine: a new one is built at the stream's rate,
    /// the commands that waited are dropped (the queue cannot grow while nothing runs), the app
    /// is told once, and the new engine plays the document.
    #[test]
    fn a_lost_engine_is_rebuilt_and_the_waiting_commands_dropped() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let doc = Document::new(test_song(sync.host()));
        sync.sync(&doc);
        let rate = sync.sample_rate();
        let detached = sync.detach().unwrap();
        sync.send(Command::Play);
        assert_eq!(sync.pending_commands(), 1);
        // The worker dies: the engine goes with it.
        drop(detached);
        sync.recover(None);
        assert!(!sync.is_detached());
        assert_eq!(sync.pending_commands(), 0);
        assert_eq!(sync.sample_rate(), rate);
        assert!(sync.take_rebuild_notice().is_some());
        assert_eq!(sync.take_rebuild_notice(), None, "taken once");
        sync.sync(&doc);
        sync.send(Command::Play);
        let at = sync.playhead();
        assert!(wait_until(|| {
            sync.poll();
            sync.playhead() > at
        }));
        sync.shutdown();
    }

    /// #211 review: a replacement engine keeps the worker setting the user chose.
    #[test]
    fn a_replacement_engine_keeps_the_worker_setting() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        sync.set_workers(Some(0)).unwrap();
        let detached = sync.detach().unwrap();
        drop(detached);
        sync.recover(None);
        assert!(wait_until(|| sync.worker_status().is_some()));
        assert_eq!(sync.worker_status().unwrap().threads, 0);
        // A setting changed during the export wins.
        let detached = sync.detach().unwrap();
        sync.set_workers(Some(2)).unwrap();
        drop(detached);
        sync.recover(None);
        assert!(wait_until(|| sync.worker_status().is_some()));
        assert_eq!(sync.worker_status().unwrap().threads, 2);
        sync.shutdown();
    }

    /// #211 review: a `Locate` sent after a rebuild beats the position the rebuilt engine returns
    /// to (also for #210's rebuild after a stream panic: both set `locate_after_rebuild`).
    #[test]
    fn a_newer_locate_beats_the_rebuild_position() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let doc = Document::new(test_song(sync.host()));
        sync.sync(&doc);
        sync.send(Command::Play);
        assert!(wait_until(|| {
            sync.poll();
            sync.playhead() > Tick(2_000)
        }));
        let detached = sync.detach().unwrap();
        drop(detached);
        sync.recover(None);
        assert!(sync.locate_after_rebuild.is_some());
        sync.send(Command::Stop);
        assert!(
            sync.locate_after_rebuild.is_some(),
            "other commands leave it"
        );
        sync.send(Command::Locate(Tick(100)));
        assert_eq!(sync.locate_after_rebuild, Some(Tick(100)));
        sync.sync(&doc);
        assert!(wait_until(|| {
            sync.poll();
            // The position is rounded to a frame.
            (Tick(95)..=Tick(100)).contains(&sync.playhead())
        }));
        assert_eq!(sync.locate_after_rebuild, None);
        sync.shutdown();
    }

    /// #211 review: the worker start of a replacement engine is not reported (the crash message
    /// outranks it); the next stream start's is again.
    #[test]
    fn a_replacement_engines_worker_notices_are_quiet_until_it_prepared() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let detached = sync.detach().unwrap();
        drop(detached);
        sync.recover(None);
        sync.worker_notice.store(true, Ordering::Relaxed);
        sync.workgroup_notice.store(true, Ordering::Relaxed);
        assert!(!sync.take_worker_notice());
        assert!(!sync.take_workgroup_notice() || sync.worker_status().is_none());
        assert!(wait_until(|| {
            sync.worker_status().is_some() && {
                sync.take_worker_notice();
                sync.take_workgroup_notice();
                !sync.quiet_worker_notices.get()
            }
        }));
        sync.worker_notice.store(true, Ordering::Relaxed);
        assert!(sync.take_worker_notice());
        sync.shutdown();
    }

    /// #92: the diagnostics of the program last sent, and a notice only when a sync leaves out
    /// something new.
    #[test]
    fn diagnostics_follow_the_sent_program_and_notice_only_new_ones() {
        use komponist_engine::MAX_TRACKS;
        use komponist_engine::compile::{Reason, Subject};
        let add = |project: &Project| {
            let spec = edits::NewTrack {
                name: None,
                hue: None,
                instrument: None,
                index: None,
                arm_exclusive: false,
            };
            edits::add_track(project, spec).unwrap().0
        };
        let mut project = Project::new("Full");
        for _ in 0..MAX_TRACKS {
            project = add(&project);
        }
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let mut doc = Document::new(project);
        sync.sync(&doc);
        assert_eq!(sync.diagnostics(), []);
        assert!(!sync.take_diagnostics_notice());

        doc.apply_with("Add track", |p| Ok((add(p), ()))).unwrap();
        sync.sync(&doc);
        let last = doc.project().tracks[MAX_TRACKS].id;
        assert_eq!(sync.diagnostics().len(), 1);
        assert_eq!(sync.diagnostics()[0].reason, Reason::TrackLimit);
        assert_eq!(
            sync.diagnostics()[0].subject,
            Subject::Track {
                track: last,
                index: MAX_TRACKS
            }
        );
        assert!(sync.take_diagnostics_notice());
        assert!(!sync.take_diagnostics_notice(), "taken once");

        // A change that leaves out nothing new says nothing.
        doc.apply_with("Rename", |p| {
            Ok((
                edits::set_track_prop(p, &[last], TrackProp::Name("Extra".into()))?,
                (),
            ))
        })
        .unwrap();
        sync.sync(&doc);
        assert_eq!(sync.diagnostics().len(), 1);
        assert!(!sync.take_diagnostics_notice());

        // Removing the extra track clears the diagnostics without a notice.
        doc.apply_with("Remove", |p| Ok((edits::remove_tracks(p, &[last])?, ())))
            .unwrap();
        sync.sync(&doc);
        assert_eq!(sync.diagnostics(), []);
        assert!(!sync.take_diagnostics_notice());

        // Another project (open, new) that leaves out the same track at the same position: its
        // diagnostic is new, though equal to one the previous project had.
        doc.apply_with("Add track", |p| Ok((add(p), ()))).unwrap();
        sync.sync(&doc);
        assert!(sync.take_diagnostics_notice());
        let before = sync.diagnostics().to_vec();
        sync.reset();
        assert_eq!(sync.diagnostics(), []);
        let other = Document::new(doc.project().as_ref().clone());
        sync.sync(&other);
        assert_eq!(sync.diagnostics(), before);
        assert!(sync.take_diagnostics_notice());
        sync.shutdown();
    }

    #[test]
    fn reattaching_at_a_new_rate_recompiles_the_same_document_revision() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let doc = Document::new(test_song(sync.host()));
        sync.sync(&doc);
        let sent = sync.programs_sent;
        let detached = sync.detach().unwrap();
        sync.set_audio(&StreamConfig {
            host: HostKind::Null,
            sample_rate: 44_100,
            ..StreamConfig::default()
        })
        .unwrap();
        sync.attach(detached);
        assert_eq!(sync.sample_rate(), 44_100);
        sync.sync(&doc);
        assert_eq!(sync.programs_sent, sent + 1);
    }

    /// WP-65: the stream starts the automatic worker pool; the setting changes it (with the
    /// engine held, during an export and in the stream) and playback goes on.
    #[test]
    fn worker_threads_follow_the_setting() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
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
        let doc = Document::new(test_song(sync.host()));
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
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let mut doc = Document::new(test_song(sync.host()));
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
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let doc = Document::new(test_song(sync.host()));
        sync.sync(&doc);
        let first = sync.program().unwrap().tracks[0].clone();
        sync.sync(&doc);
        assert!(Arc::ptr_eq(&first, &sync.program().unwrap().tracks[0]));
    }

    #[test]
    fn the_compiler_uses_the_authored_clap_parameter_ids() {
        let host = App::headless_plugin_host();
        let prepared = host
            .manager()
            .prepare_bundled_device(
                "org.klang-studio.komponist.hexa",
                Some("E-Piano"),
                &[],
                &["feedback", "volume", "spread"],
            )
            .expect("build the Hexa fixture with scripts/build-test-plugins.sh");
        let (mut project, track) = edits::add_track(
            &Project::new("Native automation"),
            edits::NewTrack {
                name: None,
                hue: None,
                instrument: Some(prepared.plugin.clone()),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let targets = prepared.targets.clone();
        let index = project.track_index(track).unwrap();
        let entry = &mut Arc::make_mut(&mut project.tracks[index]).channel.devices[0];
        let device = entry.id;
        *entry = Arc::new(prepared.into_device(device));
        let doc = Document::new(project);
        let mut sync = EngineSync::null(Box::new(host));
        sync.sync(&doc);
        assert!(wait_until(|| {
            sync.poll();
            sync.host().params(device).is_some()
        }));
        let model = doc.project().find_device(device).unwrap().2;
        let env = Env {
            midi: None,
            host: sync.host(),
            rate: 48_000.0,
        };
        for (name, (id, normalized)) in targets {
            let parameter = env
                .automation_parameter(model, &id)
                .unwrap_or_else(|| panic!("{name}"));
            assert_eq!(parameter.wire_id, id.plugin_id().unwrap());
            let default = sync.host().param_base(device, parameter.wire_id).unwrap();
            assert!((default - parameter.base).abs() < 1e-6, "{name}");
            assert!(
                (parameter.map.plain(normalized) - parameter.base).abs() < 1e-6,
                "{name}"
            );
        }
    }

    #[test]
    fn removed_devices_leave_the_host_and_new_ones_get_processors() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let mut doc = Document::new(test_song(sync.host()));
        sync.sync(&doc);
        let lead_synth = doc.project().tracks[2].channel.devices[0].id;
        assert!(wait_until(|| {
            sync.poll();
            sync.device_status(lead_synth) == DeviceStatus::Ok
        }));
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
    fn sync_reuses_full_processor_slots_after_retiring_the_old_device() {
        use edits::NewDevice;
        use komponist_model::automation::{AutomationOwner, AutomationTarget};
        use komponist_model::{ChannelRef, DeviceRole};

        const LIMITER: &str = "org.klang-studio.komponist.limiter";
        let host = App::headless_plugin_host();
        let prepare = |plugin: &str, edits: &[(&str, f64)], targets: &[&str]| {
            host.manager()
                .prepare_bundled_device(plugin, None, edits, targets)
                .unwrap_or_else(|error| panic!("{error}; build the bundles for the tests"))
        };
        let new_device =
            |prepared: komponist_plugins::bundled_initial_state::InitialDevice| NewDevice {
                role: DeviceRole::Effect,
                plugin: prepared.plugin,
                state: prepared.state,
                sidechain: None,
                preset: None,
            };
        let eq = new_device(prepare("org.klang-studio.komponist.eq", &[], &[]));
        let limiter = new_device(prepare(LIMITER, &[], &[]));
        let owner = AutomationOwner::Master;
        let gain = prepare(LIMITER, &[], &["gain"]).targets["gain"].0.clone();
        let parameter = |device| AutomationTarget::Param {
            device,
            param: gain.clone(),
        };
        let zero_latency_limiter = new_device(prepare(LIMITER, &[("lookahead", 0.0)], &[]));
        // With one initial spare, the first new limiter causes a latency recompile while
        // the second limiter still needs to wait for the removed device's slot.
        for spare in [0, 1] {
            let (mut project, removed) = edits::insert_device(
                &Project::new("Full processor table"),
                ChannelRef::Master,
                0,
                zero_latency_limiter.clone(),
            )
            .unwrap();
            for index in 1..MAX_SLOTS - spare {
                (project, _) =
                    edits::insert_device(&project, ChannelRef::Master, index, eq.clone()).unwrap();
            }
            (project, _) =
                edits::automation::add_lane(&project, owner, parameter(removed), "Gain".into())
                    .unwrap();
            let mut doc = Document::new(project);
            let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
            sync.hold_engine().unwrap();
            sync.sync(&doc);
            let original = sync.program().unwrap();
            let old_slot = original.slots.slot(removed).unwrap();
            let old_target = original
                .automation_target(owner, &parameter(removed))
                .unwrap();
            // Finish all initial insertions, including any program install waiting for room
            // in the command queue.
            sync.held.as_mut().unwrap().engine.settle();
            sync.poll();
            sync.held.as_mut().unwrap().engine.settle();
            let initial_programs = sync.programs_sent();

            let replacements = doc
                .apply_with("Replace devices", |project| {
                    let mut project = edits::remove_device(project, removed)?;
                    let mut replacements = Vec::new();
                    for index in 0..=spare {
                        let id;
                        (project, id) = edits::insert_device(
                            &project,
                            ChannelRef::Master,
                            index,
                            limiter.clone(),
                        )?;
                        replacements.push(id);
                    }
                    let (project, _) = edits::automation::add_lane(
                        &project,
                        owner,
                        parameter(replacements[spare]),
                        "Gain".into(),
                    )?;
                    Ok((project, replacements))
                })
                .unwrap();
            sync.sync(&doc);
            sync.sync(&doc);
            assert_eq!(sync.programs_sent(), initial_programs + 2);
            sync.held.as_mut().unwrap().engine.settle();
            let program = sync.program().unwrap();
            assert_eq!(program.slots.entries().len(), MAX_SLOTS);
            assert_eq!(program.slots.slot(removed), None);
            assert_eq!(program.slots.slot(replacements[spare]), Some(old_slot));
            assert!(!sync.plugin_parameter_slot_matches(old_slot, removed));
            assert!(sync.plugin_parameter_slot_matches(old_slot, replacements[spare]));
            let replacement_target = parameter(replacements[spare]);
            let new_target = program
                .automation_target(owner, &replacement_target)
                .unwrap();
            assert_eq!(program.automation_model_target(owner, old_target), None);
            assert_eq!(
                program.automation_model_target(owner, new_target),
                Some(&replacement_target)
            );
            let mut total_latency = 0;
            for replacement in replacements {
                let slot = program.slots.slot(replacement).unwrap();
                let latency = sync.host.latency(replacement);
                assert!(latency > 0);
                assert_eq!(
                    sync.held.as_ref().unwrap().engine.processor_latency(slot),
                    latency
                );
                total_latency += latency;
            }
            assert_eq!(program.master.chain_latency, total_latency);
        }
    }

    #[test]
    fn reset_starts_the_next_project_from_scratch() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let first = Document::new(test_song(sync.host()));
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
        let second = Document::new(test_song(sync.host()));
        assert_eq!(second.revision(), first.revision());
        sync.sync(&second);
        // The verified Sampler reloads in the new document.
        assert!(wait_until(|| {
            sync.poll();
            sync.device_status(drums) == DeviceStatus::Ok
        }));
        // The compressor behind it is not installed: it stays missing.
        let compressor = second.project().tracks[0].channel.devices[1].id;
        assert_eq!(sync.device_status(compressor), DeviceStatus::Missing);
        // The same document again without reset: nothing is recompiled.
        let program = sync.program().expect("compiled again");
        assert_eq!(program.tracks.len(), second.project().tracks.len());
        let tracks = Arc::clone(&program.tracks[0]);
        sync.sync(&second);
        assert!(Arc::ptr_eq(&tracks, &sync.program().unwrap().tracks[0]));
    }

    #[test]
    fn fast_path_defers_the_recompile_to_the_gesture_end() {
        let mut sync = EngineSync::null(Box::new(App::headless_plugin_host()));
        let mut doc = Document::new(test_song(sync.host()));
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
