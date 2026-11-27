//! Audio stream, MIDI ports, engine queues and ownership transfers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_audio::{
    AudioError, AudioProcess, AudioState, AudioSystem, CallbackInfo, HostKind, MidiIn,
    MidiInputHub, MidiPortInfo, MidiReceiver, OutputBuffer, StreamConfig, StreamParams,
    StreamStats,
};
use komponist_engine::workers::{
    WorkerRt, WorkerStats, WorkerStatus, auto_workers, cap_to_workgroup,
};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, LiveInput, MAX_BLOCK, MidiInEvent,
    MidiPort, Telemetry, TransportState,
};
use komponist_model::Tick;
use komponist_plugins::ProcessorSetup;
use komponist_processor::MAX_EVENTS_PER_BLOCK;

/// How long [`super::EngineSync::shutdown`] lets the engine drain its last commands.
const SHUTDOWN_DRAIN: Duration = Duration::from_millis(30);

/// How often I/O checks for MIDI devices that appeared or vanished (hot-plug).
pub const MIDI_SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// How long [`super::EngineSync::hold_engine`] waits for the stream to hand the engine over.
const HOLD_TIMEOUT: Duration = Duration::from_secs(2);

/// Frames per `Engine::process` call of [`super::EngineSync::run_held`]: the null backend's default
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
    /// ([`super::EngineSync::take_worker_notice`]).
    worker_notice: Arc<AtomicBool>,
    /// Likewise for workers that could not join the audio workgroup
    /// ([`super::EngineSync::take_workgroup_notice`]).
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

/// Stream and MIDI changes relevant to the program compiler.
pub(super) struct IoChanges {
    pub(super) rebuilt_at: Option<Tick>,
    pub(super) compile_environment_changed: bool,
}

pub(super) struct EngineIo {
    audio: AudioSystem<EngineProcess>,
    // Absent while an export owns the handle together with the process.
    handle: Option<EngineHandle>,
    midi: Option<MidiInputHub>,
    next_midi_scan: Instant,
    midi_changes: Vec<MidiChange>,
    pending: VecDeque<Command>,
    rate: Arc<AtomicU32>,
    telemetry: Arc<Telemetry>,
    held: Option<Box<EngineProcess>>,
    // Position shown by the app while the export renders with its own transport.
    frozen: Option<Tick>,
    workers_after_export: Option<Option<u32>>,
    worker_setting: Option<u32>,
    quiet_worker_notices: std::cell::Cell<bool>,
    rebuild_notice: Option<Tick>,
    // Events left by the poisoned engine, delivered on the next drain.
    stranded: Vec<EngineEvent>,
    worker_status: Arc<Mutex<Option<WorkerStatus>>>,
    worker_notice: Arc<AtomicBool>,
    workgroup_notice: Arc<AtomicBool>,
    worker_stats: Arc<WorkerStats>,
    #[cfg(test)]
    test_jack_capture_sources: Option<Vec<komponist_audio::JackCaptureChannel>>,
}

impl EngineIo {
    pub(super) fn open(config: &StreamConfig, midi: MidiSetup) -> (Self, Result<(), AudioError>) {
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
        Self::new(config, hub, receiver)
    }

    pub(super) fn new(
        config: &StreamConfig,
        hub: Option<MidiInputHub>,
        receiver: MidiReceiver,
    ) -> (Self, Result<(), AudioError>) {
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
        let mut io = Self {
            audio: AudioSystem::new(Box::new(process), receiver),
            handle: Some(handle),
            midi: hub,
            next_midi_scan: Instant::now() + MIDI_SCAN_INTERVAL,
            midi_changes: Vec::new(),
            pending: VecDeque::new(),
            rate,
            telemetry,
            held: None,
            frozen: None,
            workers_after_export: None,
            worker_setting: None,
            quiet_worker_notices: std::cell::Cell::new(false),
            rebuild_notice: None,
            stranded: Vec::new(),
            worker_status,
            worker_notice,
            workgroup_notice,
            worker_stats,
            #[cfg(test)]
            test_jack_capture_sources: None,
        };
        let started = io.audio.start(config);
        (io, started)
    }

    pub(super) fn maintain(&mut self) -> IoChanges {
        let before = self.sample_rate();
        self.audio.poll();
        let rebuilt_at = self
            .audio
            .take_poisoned()
            .map(|process| self.rebuild_engine(Some(*process)));
        let rate_changed = self.sample_rate() != before;
        let ports_changed =
            self.midi.is_some() && Instant::now() >= self.next_midi_scan && self.scan_midi();
        self.flush_pending();
        IoChanges {
            rebuilt_at,
            compile_environment_changed: rate_changed || ports_changed,
        }
    }

    /// Drains the engine's queues on the control thread, dropping retired items here.
    pub(super) fn drain_events(&mut self) -> Vec<EngineEvent> {
        let mut out = std::mem::take(&mut self.stranded);
        while let Some(event) = self.handle.as_mut().and_then(EngineHandle::poll) {
            if !matches!(event, EngineEvent::Retired(_)) {
                out.push(event);
            }
        }
        out
    }

    pub(super) fn discard_project_events(&mut self) {
        self.send(Command::StopRecording);
        self.stranded.clear();
        // This includes garbage retired by the engine.
        while self.handle.as_mut().and_then(EngineHandle::poll).is_some() {}
    }

    #[cfg(test)]
    pub(super) fn queue_midi_change(&mut self, change: MidiChange) {
        self.midi_changes.push(change);
    }

    #[cfg(test)]
    pub(super) fn held_engine(&self) -> &Engine {
        &self.held.as_ref().expect("held engine").engine
    }

    #[cfg(test)]
    pub(super) fn held_engine_mut(&mut self) -> &mut Engine {
        &mut self.held.as_mut().expect("held engine").engine
    }

    pub(super) fn send(&mut self, command: Command) {
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

    pub(super) fn scan_midi(&mut self) -> bool {
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
        changed
    }

    pub(super) fn set_audio(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        let changed = match self.audio.state() {
            AudioState::Running { config: old, .. } | AudioState::NoDevice { config: old, .. } => {
                old != config
            }
            AudioState::Stopped => false,
        };
        if changed {
            self.send(Command::AbortAudioRecording);
        }
        self.audio.start(config)
    }

    pub(super) fn set_audio_input(
        &mut self,
        config: Option<komponist_audio::InputConfig>,
    ) -> Result<(), AudioError> {
        if self.audio.input() != config.as_ref() {
            self.send(Command::AbortAudioRecording);
        }
        self.audio.set_input(config)
    }

    pub(super) fn set_test_audio_input(
        &mut self,
        channels: u16,
        rate: u32,
    ) -> Result<komponist_audio::TestAudioSource, AudioError> {
        self.send(Command::AbortAudioRecording);
        self.audio.set_test_input(channels, rate)
    }

    pub(super) fn audio_input_config(&self) -> Option<&komponist_audio::InputConfig> {
        self.audio.input()
    }

    pub(super) fn audio_input_error(&self) -> Option<&AudioError> {
        self.audio.input_error()
    }

    pub(super) fn audio_input_channels(&self) -> u16 {
        self.audio.input_channels()
    }

    pub(super) fn hardware_input_available(&self, input: komponist_model::HardwareInput) -> bool {
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

    pub(super) fn jack_playback_connections(&self) -> Option<[bool; 2]> {
        self.audio.jack_playback_connections()
    }

    pub(super) fn jack_capture_sources(&self) -> Option<&[komponist_audio::JackCaptureChannel]> {
        #[cfg(test)]
        if let Some(sources) = self.test_jack_capture_sources.as_deref() {
            return Some(sources);
        }
        self.audio.jack_capture_sources()
    }

    #[cfg(test)]
    pub(super) fn set_test_jack_capture_sources(
        &mut self,
        sources: Vec<komponist_audio::JackCaptureChannel>,
    ) {
        self.test_jack_capture_sources = Some(sources);
    }

    pub(super) fn audio_input_devices(
        &self,
    ) -> Result<Vec<komponist_audio::InputDeviceInfo>, AudioError> {
        let host = match self.audio.state() {
            AudioState::Running { config, .. } | AudioState::NoDevice { config, .. } => config.host,
            AudioState::Stopped => HostKind::Null,
        };
        AudioSystem::<EngineProcess>::input_devices(host)
    }

    pub(super) fn set_workers(
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

    pub(super) fn worker_status(&self) -> Option<WorkerStatus> {
        self.worker_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(super) fn take_worker_notice(&self) -> bool {
        let notice = self.worker_notice.swap(false, Ordering::Relaxed);
        notice && !self.quiet_worker_notices.get()
    }

    pub(super) fn take_workgroup_notice(&self) -> bool {
        let notice = self.workgroup_notice.swap(false, Ordering::Relaxed);
        let quiet = self.quiet_worker_notices.get();
        if quiet && self.worker_status().is_some() {
            self.quiet_worker_notices.set(false);
        }
        notice && !quiet
    }

    pub(super) fn worker_stats(&self) -> &WorkerStats {
        &self.worker_stats
    }

    pub(super) fn shutdown(&mut self) {
        self.send(Command::Stop);
        self.flush_pending();
        std::thread::sleep(SHUTDOWN_DRAIN);
        self.audio.stop();
        while self.handle.as_mut().and_then(EngineHandle::poll).is_some() {}
    }

    pub(super) fn setup(&self) -> ProcessorSetup {
        ProcessorSetup {
            sample_rate: self.sample_rate() as f32,
            max_block: MAX_BLOCK,
        }
    }

    pub(super) fn sample_rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    pub(super) fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub(super) fn playhead(&self) -> Tick {
        self.frozen.unwrap_or_else(|| self.telemetry.playhead())
    }

    pub(super) fn transport(&self) -> TransportState {
        if self.handle.is_none() {
            TransportState::Stopped
        } else {
            self.telemetry.transport()
        }
    }

    pub(super) fn stats(&self) -> &StreamStats {
        self.audio.stats()
    }

    pub(super) fn audio_state(&self) -> &AudioState {
        self.audio.state()
    }

    pub(super) fn midi(&self) -> Option<&MidiInputHub> {
        self.midi.as_ref()
    }

    pub(super) fn midi_mut(&mut self) -> Option<&mut MidiInputHub> {
        self.midi.as_mut()
    }

    pub(super) fn take_rebuild_notice(&mut self) -> Option<Tick> {
        self.rebuild_notice.take()
    }

    pub(super) fn pending_commands(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn hold_engine(&mut self) -> Result<(), AudioError> {
        if self.handle.is_none() {
            return Err(AudioError::Detached);
        }
        if self.held.is_none() {
            self.held = Some(self.audio.detach(HOLD_TIMEOUT)?);
        }
        Ok(())
    }

    pub(super) fn run_held(
        &mut self,
        frames: usize,
        midi: &[MidiInEvent],
        output_latency: u32,
    ) -> bool {
        self.run_held_capture(frames, midi, output_latency)
            .is_some()
    }

    pub(super) fn run_held_capture(
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

    pub(super) fn render_held(&mut self, frames: usize) -> Option<(Vec<f32>, Vec<f32>)> {
        let engine = &mut self.held.as_mut()?.engine;
        let mut out = (Vec::with_capacity(frames), Vec::with_capacity(frames));
        let mut done = 0;
        while done < frames {
            let n = HELD_BLOCK.min(frames - done);
            let (frames_out, _) = engine.render(n).as_chunks::<2>();
            for &[left, right] in frames_out {
                out.0.push(left);
                out.1.push(right);
            }
            done += n;
        }
        Some(out)
    }

    pub(super) fn release_engine(&mut self) {
        if let Some(process) = self.held.take() {
            self.audio.attach(process);
        }
    }

    pub(super) fn detach(&mut self) -> Result<DetachedEngine, AudioError> {
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

    pub(super) fn is_detached(&self) -> bool {
        self.handle.is_none()
    }

    pub(super) fn take_midi_changes(&mut self) -> Vec<MidiChange> {
        std::mem::take(&mut self.midi_changes)
    }

    fn rebuild_engine(&mut self, poisoned: Option<EngineProcess>) -> Tick {
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
        self.rebuild_notice = Some(playhead);
        // `prepare` starts the workers for the running stream.
        self.audio.attach(Box::new(process));
        playhead
    }

    pub(super) fn recover(&mut self, engine: Option<DetachedEngine>) -> Tick {
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
                self.rebuild_engine(Some(*process))
            }
            None => self.rebuild_engine(None),
        }
    }

    pub(super) fn attach(&mut self, engine: DetachedEngine) {
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
}

/// The engine and its handle out of the stream, for an export on a worker thread
/// ([`super::EngineSync::detach`]; `komponist_engine::export` lists what the caller does before and
/// after). Back with [`super::EngineSync::attach`].
pub struct DetachedEngine {
    process: Box<EngineProcess>,
    handle: EngineHandle,
    /// Taken from [`super::EngineSync::hold_engine`]'s hold.
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

impl Drop for EngineIo {
    fn drop(&mut self) {
        self.audio.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_sync::tests::wait_until;
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

    /// #211 review: the worker start of a replacement engine is not reported (the crash message
    /// outranks it); the next stream start's is again.
    #[test]
    fn a_replacement_engines_worker_notices_are_quiet_until_it_prepared() {
        let config = StreamConfig {
            host: HostKind::Null,
            ..StreamConfig::default()
        };
        let (mut sync, started) = EngineIo::new(&config, None, MidiReceiver::disconnected());
        started.unwrap();
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
}
