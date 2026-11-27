//! Coordinates the UI thread's program sync and its audio/MIDI I/O.
//!
//! Program sync compiles the document and draft against the last program sent. It inserts
//! new processors before publishing a program and removes unused processors afterward.
//! During gestures, `fast_path` can send incremental commands until the gesture ends.
//!
//! I/O owns the stream callback, MIDI ports, command and event queues, and engine transfers
//! for export and tests. Stream and MIDI changes can invalidate the compiled program.

mod io;
mod program;

use std::collections::HashMap;
use std::sync::Arc;

use komponist_audio::{
    AudioError, AudioState, HostKind, MidiInputHub, MidiReceiver, StreamConfig, StreamStats,
};
use komponist_engine::compile;
use komponist_engine::workers::{WorkerStats, WorkerStatus};
use komponist_engine::{
    Command, EngineEvent, MidiInEvent, Program, SlotId, Telemetry, TrackMeter, TransportState,
};
use komponist_model::project::SavedPluginState;
use komponist_model::{DeviceId, Document, Project, Tick, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent, ProcessorSetup};
use komponist_processor::processor::ParameterEditKind;

use crate::device_host::DeviceHost;

use io::EngineIo;
pub use io::{
    DetachedEngine, EngineProcess, MIDI_SCAN_INTERVAL, MidiChange, MidiSetup, lacks_realtime,
    lacks_workgroup, midi_changes,
};
use program::ProgramSync;
pub(crate) use program::{DraftOverlay, MacroValueEdit};

/// What [`EngineSync::poll`] reports to the app (everything but garbage).
#[derive(Debug)]
pub enum SyncEvent {
    /// An engine event (recorded notes, loop wraps, count-in end, failed processors).
    Engine(EngineEvent),
    /// A device host event (status, state snapshot, latency, …).
    Device(PluginManagerEvent),
}

/// The app's control-thread entry point for the engine.
pub struct EngineSync {
    io: EngineIo,
    program: ProgramSync,
}

impl core::fmt::Debug for EngineSync {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineSync")
            .field("state", self.io.audio_state())
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
        let (io, started) = EngineIo::open(config, midi);
        (
            Self {
                io,
                program: ProgramSync::new(host),
            },
            started,
        )
    }

    /// [`EngineSync::new`] with the MIDI queue's receiver given (no hub): a
    /// [`komponist_audio::TestMidiSource`] feeds it in tests.
    pub fn with_receiver(
        config: &StreamConfig,
        hub: Option<MidiInputHub>,
        receiver: MidiReceiver,
        host: Box<dyn DeviceHost>,
    ) -> (EngineSync, Result<(), AudioError>) {
        let (io, started) = EngineIo::new(config, hub, receiver);
        let program = ProgramSync::new(host);
        (Self { io, program }, started)
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

    /// Sync the committed document without a playback draft, for export and headless callers.
    pub fn sync(&mut self, doc: &Document) {
        self.sync_with_draft(doc, DraftOverlay::default());
    }

    /// Sync the document with any completed in-progress automation. A gesture can skip compiling
    /// a revision sent as fast-path commands as long as the overlay is unchanged.
    pub(crate) fn sync_with_draft(&mut self, doc: &Document, draft: DraftOverlay<'_>) {
        // Export renders the committed document. The playback draft stays with the app.
        let draft = if self.io.is_detached() {
            DraftOverlay::default()
        } else {
            draft
        };
        self.program.sync_with_draft(&mut self.io, doc, draft);
    }

    /// Force the next sync to compile again after a change outside the document.
    pub(crate) fn invalidate_project(&mut self) {
        self.program.invalidate_project();
    }

    /// How many programs were compiled and sent so far (tests).
    pub fn programs_sent(&self) -> u64 {
        self.program.programs_sent()
    }

    /// Replaces the processor of `device` with a fresh one from the host (hexa preset
    /// loaded into the device state, "Neu laden"). Nothing happens if the device has no slot.
    pub fn reload_device(&mut self, project: &Project, device: komponist_model::DeviceId) {
        self.program.reload_device(&mut self.io, project, device);
    }

    /// How many fresh processors [`EngineSync::reload_device`] made ("Neu laden", a found plugin,
    /// a hexa state change no card parameter reaches). A knob turn and its undo or redo must not
    /// count here: they reach the running processor as `SetParam`.
    pub fn device_reloads(&self) -> u64 {
        self.program.device_reloads()
    }

    /// Brings the running devices in line with their document state after a change that
    /// replaced states outside the dispatcher's own paths (undo, redo, a preset, a paste over):
    /// every device in both `before` and `after` whose state differs follows `after`.
    /// Plugins restore their opaque saved state through the generic state interface.
    ///
    /// Devices without slot (not in the sent program yet) are skipped: their processor is made
    /// from the document state when the program is sent.
    pub fn sync_device_states(
        &mut self,
        before: &Project,
        after: &Project,
        startup_states: &HashMap<DeviceId, SavedPluginState>,
    ) {
        self.program
            .sync_device_states(&mut self.io, before, after, startup_states);
    }

    /// Refresh the model positions used for state snapshots after a fast-path macro edit.
    pub(crate) fn refresh_snapshot_automation(&mut self, project: &Project) {
        self.program.refresh_snapshot_automation(project);
    }

    /// Starts tracking a manual value edit until the engine publishes its effect.
    pub(crate) fn macro_value_command(
        &mut self,
        track: TrackId,
        index: u8,
        change: MacroValueEdit,
    ) -> Command {
        self.program.macro_value_command(track, index, change)
    }

    pub(crate) fn pending_macro_value(&self, track: TrackId, index: u8, base: f32) -> Option<f32> {
        self.program
            .pending_macro_value(self.io.telemetry(), track, index, base)
    }

    pub(crate) fn discard_pending_macro_overrides(&mut self) {
        self.program.discard_pending_macro_overrides();
    }

    /// A fast-path command for a change the dispatcher just applied inside a gesture
    /// (`SetTrackGain`, `SetTrackPan`, `SetTrackMute`, `SetTrackSolo`, `SetGates`,
    /// `SetMasterGain`):
    /// `before` and `after` are the document revisions around the change. The engine gets the
    /// command now; if the engine was up to date before, the recompile is deferred to the end of
    /// the gesture.
    pub fn fast_path(&mut self, command: Command, before: u64, after: u64) {
        self.program.mark_fast_path(before, after);
        self.send(command);
    }

    /// The fast-path gate table (`Command::SetGates`) of `project`'s mute and solo states for the
    /// program last sent (ARCHITECTURE §6.8.6): with buses, what solo silences follows the signal
    /// path, which only the routing graph knows. `None` before the first program.
    pub fn gates(&self, project: &Project) -> Option<Command> {
        self.program.gates(project)
    }

    /// Queues `command`; if the engine's queue is full (or an export has the engine) it is
    /// retried at the next [`EngineSync::poll`], keeping the order.
    pub fn send(&mut self, command: Command) {
        self.program.note_transport_command(&command);
        self.io.send(command);
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
        self.program.plugin_parameter_slot_matches(slot, device)
    }

    pub(crate) fn plugin_mapping_parameter(&self, device: DeviceId, id: u32) -> bool {
        self.program.plugin_mapping_parameter(device, id)
    }

    pub(crate) fn update_plugin_parameter(
        &mut self,
        device: DeviceId,
        edit: ParameterEditKind,
        temporary: bool,
        learning: bool,
    ) -> Option<f64> {
        self.program
            .update_plugin_parameter(device, edit, temporary, learning)
    }

    pub(crate) fn poll_engine_events(&mut self) -> Vec<EngineEvent> {
        let changes = self.io.maintain();
        if let Some(at) = changes.rebuilt_at {
            self.program.engine_rebuilt(at);
        } else if changes.compile_environment_changed {
            self.program.invalidate_project();
        }
        self.program.refresh_macro_state(self.io.telemetry());
        self.io.drain_events()
    }

    pub(crate) fn poll_device_events(&mut self) -> Vec<PluginManagerEvent> {
        self.program.poll_device_events(&mut self.io)
    }

    /// Looks for MIDI devices that appeared or vanished and connects new ones (hot-plug), then
    /// restarts the [`MIDI_SCAN_INTERVAL`] timer. Returns whether the port list changed; the next
    /// [`EngineSync::sync`] then compiles the project again, because a track that names a device
    /// resolves it to a port at compile time (a device that was absent then receives nothing).
    pub fn scan_midi(&mut self) -> bool {
        let changed = self.io.scan_midi();
        if changed {
            self.program.invalidate_project();
        }
        changed
    }

    /// Tests: a MIDI change and a new compile diagnostic waiting for the app, as a scan and a
    /// sync leave them.
    #[cfg(test)]
    pub(crate) fn queue_notices(&mut self, change: MidiChange, diagnostic: compile::Diagnostic) {
        self.io.queue_midi_change(change);
        self.program.queue_diagnostic(diagnostic);
    }

    /// The devices that appeared or vanished in scans since the last call (the status bar says
    /// so). The scan at start-up is not among them: it runs before the first scan.
    pub fn take_midi_changes(&mut self) -> Vec<MidiChange> {
        self.io.take_midi_changes()
    }

    /// Rebuilds the stream for `config` (S07: driver, device, rate, buffer). The engine survives;
    /// a new rate re-activates the processors.
    pub fn set_audio(&mut self, config: &StreamConfig) -> Result<(), AudioError> {
        let before = self.io.sample_rate();
        let started = self.io.set_audio(config);
        if self.io.sample_rate() != before {
            self.program.invalidate_project();
        }
        started
    }

    /// Explicitly configures capture; absent by default so old projects never open inputs.
    pub fn set_audio_input(
        &mut self,
        config: Option<komponist_audio::InputConfig>,
    ) -> Result<(), AudioError> {
        self.io.set_audio_input(config)
    }

    /// Synthetic capture on the null backend, without enumerating or opening hardware.
    pub fn set_test_audio_input(
        &mut self,
        channels: u16,
        rate: u32,
    ) -> Result<komponist_audio::TestAudioSource, AudioError> {
        self.io.set_test_audio_input(channels, rate)
    }

    /// Requested capture configuration, independent of per-track channel selection.
    pub fn audio_input_config(&self) -> Option<&komponist_audio::InputConfig> {
        self.io.audio_input_config()
    }

    /// Why the capture stream is unavailable while playback is running.
    pub fn audio_input_error(&self) -> Option<&AudioError> {
        self.io.audio_input_error()
    }

    /// Input channels currently available from the configured device.
    pub fn audio_input_channels(&self) -> u16 {
        self.io.audio_input_channels()
    }

    /// Whether the selected hardware lane or stereo pair is available on the open stream.
    pub fn hardware_input_available(&self, input: komponist_model::HardwareInput) -> bool {
        self.io.hardware_input_available(input)
    }

    /// Verified native JACK master links, in left/right order.
    pub fn jack_playback_connections(&self) -> Option<[bool; 2]> {
        self.io.jack_playback_connections()
    }

    /// Source port metadata for the capture channel order of the active JACK route.
    pub fn jack_capture_sources(&self) -> Option<&[komponist_audio::JackCaptureChannel]> {
        self.io.jack_capture_sources()
    }

    #[cfg(test)]
    pub(crate) fn set_test_jack_capture_sources(
        &mut self,
        sources: Vec<komponist_audio::JackCaptureChannel>,
    ) {
        self.io.set_test_jack_capture_sources(sources);
    }

    /// Metadata-only input enumeration for the active host (no stream is opened).
    pub fn audio_input_devices(&self) -> Result<Vec<komponist_audio::InputDeviceInfo>, AudioError> {
        self.io.audio_input_devices()
    }

    /// Parallel track processing (WP-65, settings file `worker_threads`): `None` = automatic
    /// ([`komponist_engine::workers::auto_workers`]), `Some(0)` = off, `Some(n)` = `n` worker threads. Takes the engine out
    /// of the stream for a moment (a short silence) if the count changes; during an export the
    /// change waits for its end. Returns the outcome of the worker start, if one happened.
    pub fn set_workers(
        &mut self,
        setting: Option<u32>,
    ) -> Result<Option<WorkerStatus>, AudioError> {
        self.io.set_workers(setting)
    }

    /// Outcome of the engine's last worker start (thread count, `SCHED_FIFO` refused, …).
    pub fn worker_status(&self) -> Option<WorkerStatus> {
        self.io.worker_status()
    }

    /// True once per stream start whose workers run without realtime priority (the status bar
    /// says so; [`lacks_realtime`]).
    pub fn take_worker_notice(&self) -> bool {
        self.io.take_worker_notice()
    }

    /// True once per stream start whose workers could not join the audio workgroup (the status
    /// bar says so; [`lacks_workgroup`]).
    ///
    /// Called after [`EngineSync::take_worker_notice`]; ends the quiet period of a replacement
    /// engine once it has prepared.
    pub fn take_workgroup_notice(&self) -> bool {
        self.io.take_workgroup_notice()
    }

    /// The engine's worker pool counters (batches, jobs on workers, late waits).
    pub fn worker_stats(&self) -> &WorkerStats {
        self.io.worker_stats()
    }

    /// Stops playback, lets the engine drain, stops the stream (app exit).
    pub fn shutdown(&mut self) {
        self.io.shutdown();
    }

    /// Processor setup at the engine's current rate.
    pub fn setup(&self) -> ProcessorSetup {
        self.io.setup()
    }

    /// The engine's sample rate in Hz (the stream's, once it started).
    pub fn sample_rate(&self) -> u32 {
        self.io.sample_rate()
    }

    /// Transport state, playhead and master meter.
    pub fn telemetry(&self) -> &Telemetry {
        self.io.telemetry()
    }

    /// Playhead. While an export has the engine: the playhead from before (the render moves the
    /// engine's own, but the transport stops during an export, ARCHITECTURE §6.7).
    pub fn playhead(&self) -> Tick {
        self.io.playhead()
    }

    /// Transport state; `Stopped` while an export has the engine.
    pub fn transport(&self) -> TransportState {
        self.io.transport()
    }

    /// CPU load and xruns of the stream.
    pub fn stats(&self) -> &StreamStats {
        self.io.stats()
    }

    /// The audio system's state (running config, `NoDevice`).
    pub fn audio_state(&self) -> &AudioState {
        self.io.audio_state()
    }

    /// The MIDI inputs, if the system's MIDI could be opened.
    pub fn midi(&self) -> Option<&MidiInputHub> {
        self.io.midi()
    }

    /// The MIDI inputs, mutable (S07 switches).
    pub fn midi_mut(&mut self) -> Option<&mut MidiInputHub> {
        self.io.midi_mut()
    }

    /// The program last sent to the engine.
    pub fn program(&self) -> Option<&Program> {
        self.program.program()
    }

    /// The tracks and devices the program last sent leaves out of its project, and why (#92), in
    /// project order; empty before the first sync.
    pub fn diagnostics(&self) -> &[compile::Diagnostic] {
        self.program.diagnostics()
    }

    /// Whether a sync since the last call left out a track or device that the program before it
    /// played (or a known one for another reason, or at another position): the app tells it in
    /// the status bar.
    pub fn take_diagnostics_notice(&mut self) -> bool {
        self.program.take_diagnostics_notice()
    }

    /// The level meter of `track` in the program last sent.
    pub fn track_meter(&self, track: TrackId) -> Option<&Arc<TrackMeter>> {
        self.program.track_meter(track)
    }

    /// The device host.
    pub fn host(&self) -> &dyn DeviceHost {
        self.program.host()
    }

    /// The device host, mutable (parameters, plugin GUIs, scans, state snapshots). Processors are
    /// created and released only through [`EngineSync::sync`], never directly.
    pub fn host_mut(&mut self) -> &mut dyn DeviceHost {
        self.program.host_mut()
    }

    /// Runtime status of a device.
    pub fn device_status(&self, device: komponist_model::DeviceId) -> DeviceStatus {
        self.program.device_status(device)
    }

    /// Replaces the device host (WP-45 plugs in the plugin manager). Forces a full resync: the
    /// next [`EngineSync::sync`] compiles from scratch and every device gets a processor from the
    /// new host. Removes the old processors first.
    pub fn set_host(&mut self, host: Box<dyn DeviceHost>) {
        self.program.set_host(&mut self.io, host);
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
        self.io.discard_project_events();
        self.program.reset_project(&mut self.io);
    }

    /// An export worker panicked or died ([`EngineSync::detach`]): replaces the engine with a new
    /// one built from the document, the rebuild of [`komponist_audio::AudioSystem::take_poisoned`] (#210). `engine`
    /// is what came back from the worker, if anything: its state is unknown, so it is dropped
    /// here, on the UI thread, never reattached; its handle is drained like a poisoned engine's.
    /// Commands that waited for it are dropped, so the queue cannot grow while nothing runs.
    /// Reported through [`EngineSync::take_rebuild_notice`].
    pub fn recover(&mut self, engine: Option<DetachedEngine>) {
        let at = self.io.recover(engine);
        self.program.engine_rebuilt(at);
    }

    /// The playhead at which the engine was rebuilt after a panic, if it was since the last call
    /// (status bar notice; a running take ends there).
    pub fn take_rebuild_notice(&mut self) -> Option<Tick> {
        self.io.take_rebuild_notice()
    }

    /// Commands waiting for room in the engine's queue (diagnostics, tests).
    pub fn pending_commands(&self) -> usize {
        self.io.pending_commands()
    }

    /// Tests: takes the engine out of the stream (which goes on outputting silence), so that it
    /// runs only in [`EngineSync::run_held`] and positions are exact. Waits for the stream to hand
    /// it over. Fails while an export has the engine.
    pub fn hold_engine(&mut self) -> Result<(), AudioError> {
        self.io.hold_engine()
    }

    /// Tests: runs the held engine for `frames` frames in blocks of 256, with live `midi` (frames
    /// counted from the start of this call, sorted) heard `output_latency` frames late, as a
    /// stream would. Returns `false` if no engine is held.
    pub fn run_held(&mut self, frames: usize, midi: &[MidiInEvent], output_latency: u32) -> bool {
        self.io.run_held(frames, midi, output_latency)
    }

    /// [`EngineSync::run_held`] that also returns what the engine rendered (left, right); `None`
    /// if no engine is held. The blocks keep a callback's realtime deadline, so a bridge that is
    /// late on a loaded machine falls back for that block; [`EngineSync::render_held`] waits.
    pub fn run_held_capture(
        &mut self,
        frames: usize,
        midi: &[MidiInEvent],
        output_latency: u32,
    ) -> Option<(Vec<f32>, Vec<f32>)> {
        self.io.run_held_capture(frames, midi, output_latency)
    }

    /// Tests: renders `frames` frames of the held engine offline ([`komponist_engine::Engine::render`], blocks of
    /// 256) and returns them (left, right); `None` if no engine is held. Unlike
    /// [`EngineSync::run_held_capture`], whose blocks keep the realtime deadline of a callback,
    /// bridges get their offline wait here, so a loaded machine never turns a block into the
    /// fallback (#347). No live MIDI: send `Command::LiveNote` instead.
    pub fn render_held(&mut self, frames: usize) -> Option<(Vec<f32>, Vec<f32>)> {
        self.io.render_held(frames)
    }

    /// Tests: gives the held engine back to the stream.
    pub fn release_engine(&mut self) {
        self.io.release_engine();
    }

    /// Export (ARCHITECTURE §6.7): takes the engine out of the stream (which goes on outputting
    /// silence) together with its handle, for a worker thread. Queued commands are sent first,
    /// so the engine gets them before the export's own. Until [`EngineSync::attach`], commands
    /// wait in the pending queue and [`EngineSync::poll`] reports device host events only. A held
    /// engine ([`EngineSync::hold_engine`]) is taken from the hold and goes back there.
    pub fn detach(&mut self) -> Result<DetachedEngine, AudioError> {
        self.io.detach()
    }

    /// Gives a [`DetachedEngine`] back: the handle to the UI thread, the engine to the stream
    /// (which re-activates the processors if the engine's rate differs from the stream's), and
    /// sends the commands that waited.
    pub fn attach(&mut self, engine: DetachedEngine) {
        self.program.invalidate_project();
        self.io.attach(engine);
    }

    /// Whether an export has the engine ([`EngineSync::detach`]).
    pub fn is_detached(&self) -> bool {
        self.io.is_detached()
    }
}

#[cfg(test)]
mod tests;
