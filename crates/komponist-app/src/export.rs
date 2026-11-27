//! The WAV export flow (S08, ARCHITECTURE §6.7): from "Exportieren" to the status bar message.
//! The dialog itself is [`crate::dialogs::export`]; the render is `komponist_engine::export`.
//!
//! 1. **Start** (`ProjectAction::StartExport`): the settings become the last used ones (saved in
//!    the settings file), the transport stops (a running take is committed) and the job is
//!    computed: the range ("Ganzer Song" = tick 0 to the end of the last clip, the loop, the
//!    selection) in frames at the export rate, the tail, the target `<folder>/<name>.wav`.
//! 2. **Devices** (PLAN WP-45: a device still loading renders silence): while a device of the
//!    project is not `Ok`, the dialog lists it ("lädt …", "fehlt", "abgestürzt") and the export
//!    waits; it goes on by itself when all run, or at once with "Ohne sie exportieren". Linked sample files that
//!    cannot be loaded stop the export and are named in the status message.
//! 3. **Detach**: the document is synced, the engine leaves the stream together with its handle
//!    ([`EngineSync::detach`](crate::engine_sync::EngineSync::detach)), the bridges switch to
//!    offline processing ([`DeviceHost::set_render_mode`](crate::device_host::DeviceHost)) and a
//!    worker thread takes engine, program and job.
//! 4. **Worker**: sets the export rate when it differs from the device's
//!    (`Engine::set_sample_rate` re-activates every processor) and compiles the program again
//!    against the processors in the engine (`komponist_engine::export::recompile`: delay
//!    compensation and routing latency in frames of the export rate, from the latencies the export
//!    skips), then waits for the UI. Direct CLAP processors hand back obsolete activations
//!    without processing audio while the worker waits. A bridge
//!    re-activates through the plugin manager's poll on the UI thread (and for the switch to
//!    offline processing in step 3) and renders its silent fallback until the bridge confirms;
//!    its status is `Loading` from the moment the processor or the render mode asked for it until
//!    the bridge confirmed and the warm-up (`komponist_plugins::WARM_UP`) passed. A render of only fallback blocks takes milliseconds, so the UI holds the worker
//!    back, frame by frame without blocking, until no device of the project is `Loading` (also
//!    right after a previous export at another rate, whose re-activation back to the device rate
//!    may still run), at most [`READY_TIMEOUT`]. Then the worker runs `export` (progress and peak
//!    through shared atomics), restores the device rate and sends the engine back.
//! 5. **Finish** (UI): the engine goes back into the stream (`EngineSync::attach`), the bridges
//!    back to realtime, the engine's events are drained (a `ProcessorFailed` from the render, or a
//!    processor that failed to re-activate, names a device that dropped out), the dialog closes
//!    and the status bar says „Night Drive.wav exportiert · 00:33 · 24 bit“ with „Im Ordner
//!    zeigen“ (the folder opens with `xdg-open`, on Windows `explorer`, local only). The file joins Projekt › Exporte
//!    (this session's exports of the open project).
//!
//! **Cancel** (Esc, "Abbrechen"): while waiting for devices the dialog closes at once; while
//! rendering the flag stops the render at its next check, the engine comes back as above, and no
//! file is left (`komponist_engine::export` writes a hidden part file and renames it only when
//! complete).
//!
//! Thread: UI thread, except the worker.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use komponist_engine::export::{
    BitDepth as EngineBitDepth, ExportError, ExportJob, ExportReport, NORMALIZE_TARGET_DB,
    Progress, TrackExport, export, export_track, recompile,
};
use komponist_engine::{Command, Program, SlotId, TransportState};
use komponist_ipc::RenderMode;
use komponist_model::format::fmt_bar_pos;
use komponist_model::{ClipId, DeviceId, Project, Tick, TickRange};
use komponist_plugin_api::device_assets::{AssetKind, AssetStatus, OfflineRequest, OfflineStatus};
use komponist_plugins::{DeviceStatus, FailReason};

use crate::actions::{Action, BitDepth, ExportRange, ExportSettings, ProjectAction, UiAction};
use crate::app::App;
use crate::dialogs::Overlay;
use crate::dialogs::export::{
    self as view, DeviceRow, DeviceState, Output, ProgressView, SettingsView, Stage,
};
use crate::dialogs::file::{FileBrowser, Purpose};
use crate::engine_sync::DetachedEngine;
use crate::ui_state::{
    Dialog, ExportDialog, ExportProgress, ExportStage, StatusKind, StatusMessage,
};

mod flatten;
use flatten::Flatten;

/// How long the export waits for devices re-activating at the export rate before it renders
/// without them (fallback audio).
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens a folder for "Im Ordner zeigen".
pub type FolderOpener = Box<dyn FnMut(&Path) -> io::Result<()>>;

/// The app's export state (not view state: the dialog's settings and progress are in
/// [`crate::ui_state::ExportDialog`]).
pub(crate) struct ExportController {
    /// The export in progress, from "Exportieren" until the engine is back.
    run: Option<Run>,
    /// This session's exports of the open project, oldest first (Projekt › Exporte).
    done: Vec<PathBuf>,
    /// How "Im Ordner zeigen" opens a folder.
    opener: FolderOpener,
    /// Engine events are being drained after an export: failed slots go into `failed`.
    collecting: bool,
    /// Slots that failed during the export.
    failed: Vec<SlotId>,
    next_offline_id: u32,
}

impl Default for ExportController {
    fn default() -> Self {
        Self {
            run: None,
            done: Vec::new(),
            opener: Box::new(open_folder),
            collecting: false,
            failed: Vec::new(),
            next_offline_id: 0,
        }
    }
}

impl ExportController {
    /// Whether an export runs (devices, render).
    pub(crate) fn is_active(&self) -> bool {
        self.run.is_some()
    }

    /// Whether a render is preparing to replace a track of the current project.
    pub(crate) fn is_flattening(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.flatten.is_some())
    }

    /// Records a failed slot while the events of an export are drained.
    pub(crate) fn processor_failed(&mut self, slot: SlotId) {
        if self.collecting {
            self.failed.push(slot);
        }
    }

    /// Forgets the exports of the project that was open.
    pub(crate) fn forget(&mut self) {
        self.done.clear();
    }

    pub(crate) fn done(&self) -> &[PathBuf] {
        &self.done
    }

    fn remember(&mut self, path: PathBuf) {
        self.done.retain(|exported| *exported != path);
        self.done.push(path);
    }

    fn begin(&mut self, job: ExportJob, flatten: Option<Flatten>) {
        self.run = Some(Run {
            seconds: job.frames() as f64 / f64::from(job.sample_rate),
            job,
            phase: Phase::Devices,
            flatten,
            diagnostics: Vec::new(),
            offline_devices: Vec::new(),
        });
    }

    /// Cancels the worker or takes a run that has yet to detach the engine.
    fn cancel(&mut self) -> Option<Run> {
        let run = self.run.as_mut()?;
        match &mut run.phase {
            Phase::Devices => self.run.take(),
            Phase::Running(worker) => {
                worker.cancel.store(true, Ordering::Relaxed);
                if let Some(go) = worker.go.take() {
                    let _ = go.send(Err(ExportError::Cancelled));
                }
                None
            }
        }
    }

    /// Consumes worker preparation locally; the app handles completion and engine recovery.
    fn next_worker_message(&mut self) -> Option<Result<WorkerCompletion, TryRecvError>> {
        let Some(Run {
            phase: Phase::Running(worker),
            ..
        }) = &mut self.run
        else {
            return None;
        };
        loop {
            match worker.messages.try_recv() {
                Ok(Message::Prepared) => worker.prepared = Some(Instant::now()),
                Ok(Message::Completed(completion)) => return Some(Ok(completion)),
                Err(error) => return Some(Err(error)),
            }
        }
    }

    fn offline_request(&mut self, sample_rate: u32, tempo: f64) -> OfflineRequest {
        self.next_offline_id = self.next_offline_id.wrapping_add(1).max(1);
        OfflineRequest {
            id: self.next_offline_id,
            sample_rate: f64::from(sample_rate),
            tempo,
        }
    }

    fn collect_failures(&mut self, failed: Vec<SlotId>) {
        self.failed = failed;
        self.collecting = true;
    }

    fn take_failures(&mut self) -> Vec<SlotId> {
        self.collecting = false;
        std::mem::take(&mut self.failed)
    }
}

/// `xdg-open <folder>` (Windows: `explorer <folder>`), detached; the child is reaped by a thread.
/// (`explorer` reports success with exit code 1; nobody looks at the code.)
fn open_folder(folder: &Path) -> io::Result<()> {
    let program = if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(folder)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// One export from "Exportieren" until the engine is back.
struct Run {
    /// What the engine renders.
    job: ExportJob,
    /// The rendered length (range + tail) in seconds.
    seconds: f64,
    phase: Phase,
    /// A project audio asset and a pending track replacement, for a single-track render.
    flatten: Option<Flatten>,
    /// What the rendered program leaves out of the project (#92), taken with the program at
    /// launch: the document may change while the worker renders.
    diagnostics: Vec<komponist_engine::compile::Diagnostic>,
    /// Devices that negotiated explicit preparation before the engine was detached.
    offline_devices: Vec<DeviceId>,
}

enum Phase {
    /// Waiting until every device is `Ok`.
    Devices,
    /// The worker has the engine.
    Running(Worker),
}

/// The worker thread and what the UI shares with it.
struct Worker {
    thread: Option<JoinHandle<()>>,
    messages: Receiver<Message>,
    /// Go on after re-activation, or stop with the reason reported by the UI.
    go: Option<Sender<Result<(), ExportError>>>,
    /// When the worker reported `Prepared` (the UI waits for re-activating devices).
    prepared: Option<Instant>,
    offline_request: OfflineRequest,
    offline_posted: bool,
    cancel: Arc<AtomicBool>,
    progress: Arc<SharedProgress>,
    /// When rendering started (speed, time left).
    started: Instant,
}

/// What the worker tells the UI.
enum Message {
    /// The engine runs at the export rate; the worker waits for the bridges (module docs, step 4).
    Prepared,
    Completed(WorkerCompletion),
}

enum WorkerCompletion {
    /// The worker panicked (export code, an in-process device, the engine's `render`); the engine
    /// comes back only to be replaced ([`EngineSync::recover`]), its state is unknown.
    ///
    /// `finished` is the export's report if it was complete before the panic (the closing
    /// one-frame render, the device rate): a WAV file that was written stays listed.
    Crashed {
        engine: DetachedEngine,
        finished: Option<ExportReport>,
    },
    /// The export ended; the engine comes back.
    Done {
        engine: DetachedEngine,
        result: Result<ExportReport, ExportError>,
        /// Slots whose processor failed to re-activate (at the export rate or back).
        failed: Vec<SlotId>,
    },
}

/// Progress and peak as f32 bits.
struct SharedProgress {
    fraction: AtomicU32,
    peak_db: AtomicU32,
}

impl SharedProgress {
    fn new() -> SharedProgress {
        SharedProgress {
            fraction: AtomicU32::new(0.0_f32.to_bits()),
            peak_db: AtomicU32::new(klang_dsp::units::gain_to_db(0.0).to_bits()),
        }
    }

    fn store(&self, p: Progress) {
        self.fraction.store(p.fraction.to_bits(), Ordering::Relaxed);
        self.peak_db.store(p.peak_db.to_bits(), Ordering::Relaxed);
    }

    fn load(&self) -> (f32, f32) {
        (
            f32::from_bits(self.fraction.load(Ordering::Relaxed)),
            f32::from_bits(self.peak_db.load(Ordering::Relaxed)),
        )
    }
}

/// What the worker renders: the document's project and the program the engine plays.
struct Input {
    project: Arc<Project>,
    program: Program,
    track: Option<Arc<TrackExport>>,
}

/// The worker's side of its conversation with the UI thread.
struct Link<'a> {
    to_ui: &'a Sender<Message>,
    go: &'a Receiver<Result<(), ExportError>>,
    /// Set when the export itself is over, before the steps a panic could still interrupt.
    finished: &'a mut Option<ExportReport>,
}

/// The worker: export rate, wait for the UI, render, device rate. The caller sends the engine
/// back with the outcome, also after a panic ([`run_worker`]).
fn work(
    engine: &mut DetachedEngine,
    input: Input,
    job: ExportJob,
    cancel: &AtomicBool,
    progress: &SharedProgress,
    link: Link<'_>,
) -> (Result<ExportReport, ExportError>, Vec<SlotId>) {
    let Link {
        to_ui,
        go,
        finished,
    } = link;
    let mut failed = Vec::new();
    let device_rate = engine.engine().sample_rate();
    let export_rate = job.sample_rate as f32;
    let changed = device_rate != export_rate;
    if let Err(errors) = engine.engine().prepare_offline(export_rate) {
        failed.extend(errors.into_iter().map(|(slot, _)| slot));
    }
    let Input {
        project,
        program,
        track,
    } = input;
    let flattening = track.is_some();
    let _ = to_ui.send(Message::Prepared);
    let ready = loop {
        match go.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => break result,
            Err(RecvTimeoutError::Disconnected) => break Err(ExportError::Cancelled),
            Err(RecvTimeoutError::Timeout) => {
                // The host can finish preparing an earlier revision while we request a new
                // sample rate. Return such an activation too, while the engine is detached.
                if let Err(errors) = engine.engine().prepare_offline(export_rate) {
                    failed.extend(errors.into_iter().map(|(slot, _)| slot));
                }
            }
        }
    };
    let result = ready.and_then(|()| {
        // The UI waited until the devices run at the export rate: now the compensation and the
        // skip come from the same processors, in frames of the export rate.
        if let Some(track) = track {
            let (e, h) = engine.parts();
            export_track(e, h, &program, (&track, &project), &job, cancel, |p| {
                progress.store(p)
            })
        } else {
            let export_program = recompile(engine.engine(), &project, &program);
            let (e, h) = engine.parts();
            export(e, h, &export_program, &job, cancel, |p| progress.store(p))
        }
    });
    // The export itself is over: a panic below, while the devices go back to the device rate or
    // in the closing render, keeps its outcome ([`run_worker`]).
    *finished = result.as_ref().ok().copied();
    if changed && let Err(errors) = engine.engine().set_sample_rate(device_rate) {
        failed.extend(errors.into_iter().map(|(slot, _)| slot));
    }
    if !flattening {
        // Full export queued the program at the export rate; restore the original program.
        let _ = engine
            .parts()
            .1
            .send(Command::SetProgram(Box::new(program)));
    }
    // `export` queued Stop, the program and the old playhead; one frame lets them take
    // effect here, so the transport and the playhead the UI reads are right as soon as the
    // engine is back (else a Play right after the export could see the export's "playing" and
    // stop instead).
    engine.engine().render(1);
    (result, failed)
}

/// The worker thread's body: [`work`] under `catch_unwind`, so the engine always goes back to
/// the UI thread. After a panic it comes as [`WorkerCompletion::Crashed`]: whatever unwound (a device
/// inside `render` poisons the engine, #210; export code leaves the rate and the program
/// half-changed) its state cannot be trusted, and the UI thread rebuilds it.
fn run_worker(
    mut engine: DetachedEngine,
    input: Input,
    job: ExportJob,
    cancel: Arc<AtomicBool>,
    progress: Arc<SharedProgress>,
    to_ui: Sender<Message>,
    go: Receiver<Result<(), ExportError>>,
) {
    let mut finished = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        work(
            &mut engine,
            input,
            job,
            &cancel,
            &progress,
            Link {
                to_ui: &to_ui,
                go: &go,
                finished: &mut finished,
            },
        )
    }));
    let completion = match outcome {
        Ok((result, failed)) => WorkerCompletion::Done {
            engine,
            result,
            failed,
        },
        Err(payload) => {
            // The payload may own device state; the UI thread must not wait for it.
            std::mem::forget(payload);
            WorkerCompletion::Crashed { engine, finished }
        }
    };
    let _ = to_ui.send(Message::Completed(completion));
}

/// The engine's bit depth for the app's.
fn engine_bits(bits: BitDepth) -> EngineBitDepth {
    match bits {
        BitDepth::Int16 => EngineBitDepth::Int16,
        BitDepth::Int24 => EngineBitDepth::Int24,
        BitDepth::Float32 => EngineBitDepth::Float32,
    }
}

/// The bar number of `tick` (1-based, `exportView`: `loopS/16+1`).
fn bar_number(tick: Tick, ticks_per_bar: i64) -> i64 {
    tick.0.div_euclid(ticks_per_bar.max(1)) + 1
}

/// "~/Musik/Exporte" for `folder` under the home folder.
fn folder_label(folder: &Path) -> String {
    crate::dialogs::format::tilde(folder)
}

impl App {
    /// This session's exports of the open project, oldest first (Projekt › Exporte).
    pub fn exported(&self) -> &[PathBuf] {
        self.export.done()
    }

    /// Whether an export runs (waiting for devices or rendering).
    pub fn is_exporting(&self) -> bool {
        self.export.is_active()
    }

    /// Replaces how "Im Ordner zeigen" opens a folder (default: `xdg-open`, `explorer` on Windows; tests record).
    pub fn with_folder_opener(mut self, opener: FolderOpener) -> App {
        self.export.opener = opener;
        self
    }

    /// The tick range of `range` now, if there is one: the song from tick 0 to the end of the
    /// last clip, the loop while it is on, the selection's time range.
    pub(crate) fn export_ticks(&self, range: ExportRange) -> Option<TickRange> {
        let p = self.project();
        match range {
            ExportRange::Song => {
                let end = self.engine.program().map_or_else(
                    || {
                        p.tracks
                            .iter()
                            .flat_map(|t| t.clips.iter().map(|c| c.end()))
                            .max()
                            .unwrap_or(Tick::ZERO)
                    },
                    |program| program.song_end,
                );
                Some(TickRange::new(Tick::ZERO, end))
            }
            ExportRange::Loop => {
                (p.loop_on && p.loop_range.end > p.loop_range.start).then_some(p.loop_range)
            }
            ExportRange::Selection => self.ui.selection.time_range.filter(|r| r.end > r.start),
        }
    }

    /// The settings the export dialog opens with: the last used ones, the file named after the
    /// project, a range that exists now (else the song).
    pub(crate) fn export_dialog_settings(&self) -> ExportSettings {
        let folder = self
            .project_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(crate::dirs::start_dir)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let mut s = self
            .settings
            .export
            .clone()
            .unwrap_or_else(|| crate::settings::default_export(&self.project().name, folder));
        s.file_name = format!("{}.wav", self.project().name);
        if self.export_ticks(s.range).is_none() {
            s.range = ExportRange::Song;
        }
        if !view::RATES.contains(&s.sample_rate) {
            s.sample_rate = crate::settings::default_export("", PathBuf::new()).sample_rate;
        }
        s
    }

    /// Ctrl Shift E / "⋯ › WAV exportieren": stops the transport and opens S08 with the last
    /// settings; an empty project says so instead.
    pub(crate) fn open_export_dialog(&mut self) {
        if self.export.is_active() {
            return;
        }
        if self.project().tracks.is_empty() {
            self.set_status(self.lang.export_empty(), StatusKind::Error);
            return;
        }
        if !self.stop_for_export() {
            return;
        }
        let settings = self.export_dialog_settings();
        self.open_dialog(Dialog::Export(ExportDialog {
            settings,
            progress: None,
            stage: ExportStage::Settings,
        }));
    }

    /// The transport stops during an export (ARCHITECTURE §6.7); a take is committed first.
    fn stop_for_export(&mut self) -> bool {
        if self.transport() != TransportState::Stopped {
            self.dispatch(Action::Transport(crate::actions::TransportAction::Stop));
        }
        self.await_audio_take()
    }

    /// "Exportieren": see the module docs (steps 1 and 2).
    pub(crate) fn start_export(&mut self, settings: ExportSettings) {
        if self.export.is_active() {
            return;
        }
        self.settings.export = Some(settings.clone());
        self.save_settings();
        if !self.stop_for_export() {
            return;
        }
        // The program must be the document's for the song end and the render.
        self.engine.sync(&self.doc);
        let Some(file) = view::file_name(&settings.file_name) else {
            return;
        };
        let Some(ticks) = self.export_ticks(settings.range) else {
            self.set_status(self.lang.export_empty(), StatusKind::Error);
            return;
        };
        let Some(program) = self.engine.program() else {
            return;
        };
        let rate = settings.sample_rate;
        let range = ExportJob::frames_of(&program.tempo_map, ticks, rate);
        let tail = ExportJob::tail_frames(settings.tail_seconds, rate);
        if range.is_empty() && tail == 0 {
            self.set_status(self.lang.export_empty(), StatusKind::Error);
            return;
        }
        let job = ExportJob {
            path: settings.folder.join(&file),
            sample_rate: rate,
            range,
            tail,
            bit_depth: engine_bits(settings.bit_depth),
            normalize: settings.normalize.then_some(NORMALIZE_TARGET_DB),
            dither: settings.dither && settings.bit_depth != BitDepth::Float32,
        };
        if !matches!(self.ui.dialog, Some(Dialog::Export(_))) {
            self.open_dialog(Dialog::Export(ExportDialog {
                settings: settings.clone(),
                progress: None,
                stage: ExportStage::Devices,
            }));
        }
        if let Some(Dialog::Export(dialog)) = &mut self.ui.dialog {
            dialog.settings = settings.clone();
            dialog.stage = ExportStage::Devices;
        }
        self.export.begin(job, None);
        self.poll_export();
    }

    /// A linked sample that cannot be played, with the device and sample named for the user.
    fn unavailable_render_audio_asset(&self, id: DeviceId) -> Option<String> {
        let host = self.engine.host();
        let (_, _, device) = self.project().find_device(id)?;
        let assets = host.asset_slots(id)?;
        let (slot, link) = assets
            .slots
            .iter()
            .filter(|slot| {
                slot.kind == AssetKind::AudioSample
                    && matches!(slot.status, AssetStatus::Missing | AssetStatus::Failed(_))
            })
            .find_map(|slot| device.asset_links.get(&slot.id).map(|link| (slot, link)))?;
        let file = link
            .name()
            .map_or_else(|| slot.label.clone(), std::borrow::Cow::into_owned);
        let name = self.device_name(id);
        let pad = host
            .native_layout(id)
            .and_then(|layout| layout.pad_bank.as_ref())
            .and_then(|bank| bank.pads.iter().position(|pad| pad.asset_slot == slot.id));
        Some(if let Some(pad) = pad {
            self.lang.flatten_pad_failed(name, pad + 1, file)
        } else {
            self.lang.flatten_sampler_failed(name, file)
        })
    }

    /// The first unavailable linked sample on an active export device, in document order.
    fn unavailable_export_asset(&self) -> Option<(DeviceId, String)> {
        let project = self.project();
        project
            .tracks
            .iter()
            .flat_map(|track| &track.channel.devices)
            .chain(&project.master.devices)
            .filter(|device| !device.bypass)
            .find_map(|device| {
                self.unavailable_render_audio_asset(device.id)
                    .map(|problem| (device.id, problem))
            })
    }

    /// Active devices that are not ready to render, in document order (master last).
    pub(crate) fn devices_not_ready(&self) -> Vec<DeviceRow> {
        let p = self.project();
        let chains = p
            .tracks
            .iter()
            .map(|t| (t.name.as_str(), &t.channel.devices))
            .chain(std::iter::once((
                self.lang.dialog_master(),
                &p.master.devices,
            )));
        let mut rows = Vec::new();
        for (track, devices) in chains {
            for d in devices.iter().filter(|device| !device.bypass) {
                let state = match self.engine.device_status(d.id) {
                    DeviceStatus::Ok if self.engine.host().render_assets_pending(d) => {
                        DeviceState::Loading
                    }
                    DeviceStatus::Ok => continue,
                    DeviceStatus::Loading => DeviceState::Loading,
                    DeviceStatus::Missing => DeviceState::Missing,
                    DeviceStatus::Crashed(FailReason::Exited { .. }) => DeviceState::Crashed,
                    DeviceStatus::Crashed(FailReason::LoadFailed(_)) => DeviceState::LoadFailed,
                    DeviceStatus::Crashed(FailReason::Unresponsive) => DeviceState::Unresponsive,
                };
                rows.push(DeviceRow {
                    name: self.device_name(d.id),
                    track: track.to_owned(),
                    state,
                });
            }
        }
        rows
    }

    /// Esc, "Abbrechen", × during an export: see the module docs.
    pub(crate) fn cancel_export(&mut self) {
        let Some(run) = self.export.cancel() else {
            return;
        };
        self.close_export_dialog();
        if run.flatten.is_some() {
            self.flatten_error(ExportError::Cancelled);
        } else {
            self.set_status(self.lang.export_cancelled(), StatusKind::Info);
        }
    }

    /// Advances the export: starts it when the devices are ready, shows the progress, takes
    /// the worker's messages. Called every frame.
    pub(crate) fn poll_export(&mut self) {
        self.poll_export_step();
        self.resume_after_flatten();
    }

    fn poll_export_step(&mut self) {
        let Some(run) = &self.export.run else {
            return;
        };
        if matches!(run.phase, Phase::Devices) {
            if let Some(flatten) = &run.flatten {
                match flatten.unavailable(self) {
                    None => self.launch_export(),
                    Some((_, DeviceStatus::Loading))
                        if flatten.requested.elapsed() < READY_TIMEOUT => {}
                    Some((id, _)) => {
                        self.export.run = None;
                        self.close_export_dialog();
                        self.flatten_error(ExportError::DeviceFailed(id));
                    }
                }
            } else if self.devices_not_ready().is_empty() {
                self.launch_export();
            }
            return;
        }
        match self.export.next_worker_message() {
            Some(Ok(WorkerCompletion::Done {
                engine,
                result,
                failed,
            })) => {
                self.finish_export(engine, result, failed);
            }
            Some(Ok(WorkerCompletion::Crashed { engine, finished })) => {
                self.finish_crashed(Some(engine), finished);
            }
            Some(Err(TryRecvError::Empty)) => {
                self.release_when_ready();
                self.update_progress();
            }
            // The worker is gone without a word and the engine went with it.
            Some(Err(TryRecvError::Disconnected)) => self.finish_crashed(None, None),
            None => {}
        }
    }

    /// "Ohne sie exportieren": go on with the devices that are not ready.
    fn export_without(&mut self) {
        if matches!(
            self.export.run,
            Some(Run {
                phase: Phase::Devices,
                flatten: None,
                ..
            })
        ) {
            self.launch_export();
        }
    }

    /// Step 3 of the module docs.
    fn launch_export(&mut self) {
        let Some(mut run) = self.export.run.take() else {
            return;
        };
        let flattening = run.flatten.is_some();
        let fail = |app: &mut App, reason: &dyn std::fmt::Display| {
            app.close_export_dialog();
            if flattening {
                app.flatten_failed(reason);
            } else {
                app.set_status(app.lang.export_failed(reason), StatusKind::Error);
            }
        };
        if !flattening && let Some((_, problem)) = self.unavailable_export_asset() {
            fail(self, &problem);
            return;
        }
        let job = run.job.clone();
        if let Some(folder) = job.path.parent()
            && let Err(e) = std::fs::create_dir_all(folder)
        {
            fail(self, &e);
            return;
        }
        if let Some(flatten) = &mut run.flatten
            && let Err(error) = flatten.prepare(self)
        {
            self.close_export_dialog();
            self.flatten_failed(error);
            return;
        }
        self.engine.sync(&self.doc);
        let Some(program) = self.engine.program().cloned() else {
            fail(self, &self.lang.flatten_worker_failed());
            return;
        };
        run.diagnostics = self.engine.diagnostics().to_vec();
        run.offline_devices = self
            .project()
            .tracks
            .iter()
            .flat_map(|track| &track.channel.devices)
            .chain(&self.project().master.devices)
            .filter(|device| {
                !device.bypass
                    && run
                        .flatten
                        .as_ref()
                        .is_none_or(|flatten| flatten.scope.devices().any(|id| id == device.id))
                    && self.engine.host().status(device.id) == DeviceStatus::Ok
                    && self.engine.host().supports_offline_preparation(device.id)
            })
            .map(|device| device.id)
            .collect();
        let offline_request = self
            .export
            .offline_request(job.sample_rate, self.doc.project().tempo.bpm);
        let project = Arc::clone(self.doc.project());
        let track = run
            .flatten
            .as_ref()
            .map(|flatten| Arc::clone(&flatten.scope));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(SharedProgress::new());
        let (to_ui, messages) = mpsc::channel();
        let (go, from_ui) = mpsc::channel();
        // Spawn before detaching, so a failed thread start never takes the engine with it.
        let (send_engine, receive_engine) = mpsc::channel();
        let thread = {
            let (cancel, progress) = (Arc::clone(&cancel), Arc::clone(&progress));
            std::thread::Builder::new()
                .name("komponist-export".into())
                .spawn(move || {
                    if let Ok(detached) = receive_engine.recv() {
                        let input = Input {
                            project,
                            program,
                            track,
                        };
                        run_worker(detached, input, job, cancel, progress, to_ui, from_ui);
                    }
                })
        };
        let thread = match thread {
            Ok(t) => t,
            Err(e) => {
                fail(self, &e);
                return;
            }
        };
        let detached = match self.engine.detach() {
            Ok(d) => d,
            Err(e) => {
                fail(self, &e);
                return;
            }
        };
        self.set_export_render_mode(&run, RenderMode::Offline);
        if let Err(error) = send_engine.send(detached) {
            self.set_export_render_mode(&run, RenderMode::Realtime);
            self.engine.attach(error.0);
            fail(self, &self.lang.flatten_worker_failed());
            return;
        }
        if let Some(Dialog::Export(dialog)) = &mut self.ui.dialog {
            dialog.stage = ExportStage::Rendering;
        }
        run.phase = Phase::Running(Worker {
            thread: Some(thread),
            messages,
            go: Some(go),
            prepared: None,
            offline_request,
            offline_posted: false,
            cancel,
            progress,
            started: Instant::now(),
        });
        self.export.run = Some(run);
        self.update_progress();
    }

    /// The worker is ready (`Prepared`): lets it render once no device of the project is
    /// `Loading` any more (re-activation at the export rate, warm-up), or after
    /// [`READY_TIMEOUT`] (such a device renders its fallback). Module docs, step 4.
    fn release_when_ready(&mut self) {
        let Some(offline_ready) = self.prepare_export_devices() else {
            return;
        };
        let Some(Run {
            phase: Phase::Running(worker),
            flatten,
            ..
        }) = &self.export.run
        else {
            return;
        };
        let Some(prepared) = worker.prepared else {
            return;
        };
        if worker.go.is_none() {
            return;
        }
        let ready = if offline_ready.is_err() {
            offline_ready
        } else if let Some(flatten) = flatten {
            match flatten.unavailable(self) {
                Some((_, DeviceStatus::Loading)) if prepared.elapsed() < READY_TIMEOUT => return,
                Some((id, _)) => Err(ExportError::DeviceFailed(id)),
                None => flatten
                    .unavailable_audio_asset(self)
                    .map_or(Ok(()), |(id, _)| Err(ExportError::DeviceFailed(id))),
            }
        } else if let Some((id, _)) = self.unavailable_export_asset() {
            Err(ExportError::DeviceFailed(id))
        } else {
            let loading = self
                .devices_not_ready()
                .iter()
                .any(|row| row.state == DeviceState::Loading);
            if loading && prepared.elapsed() < READY_TIMEOUT {
                return;
            }
            Ok(())
        };
        if let Some(Run {
            phase: Phase::Running(worker),
            ..
        }) = &mut self.export.run
            && let Some(go) = worker.go.take()
        {
            let go_on = if worker.cancel.load(Ordering::Relaxed) {
                Err(ExportError::Cancelled)
            } else {
                ready
            };
            let _ = go.send(go_on);
            worker.started = Instant::now();
        }
    }

    /// Starts one request per detached render and waits for each selected device to acknowledge it.
    fn prepare_export_devices(&mut self) -> Option<Result<(), ExportError>> {
        let Run {
            phase: Phase::Running(worker),
            offline_devices,
            ..
        } = self.export.run.as_mut()?
        else {
            return None;
        };
        let prepared = worker.prepared?;
        if worker.go.is_none() || offline_devices.is_empty() {
            return Some(Ok(()));
        }
        let host = self.engine.host_mut();
        for &device in offline_devices.iter() {
            match host.status(device) {
                DeviceStatus::Ok => {}
                DeviceStatus::Loading if prepared.elapsed() < READY_TIMEOUT => return None,
                _ => return Some(Err(ExportError::DeviceFailed(device))),
            }
        }
        if !worker.offline_posted {
            for &device in offline_devices.iter() {
                host.set_offline_preparation(device, Some(worker.offline_request));
            }
            worker.offline_posted = true;
        }
        for &device in offline_devices.iter() {
            match host.offline_preparation_status(device) {
                Some(OfflineStatus::Ready) => {}
                Some(OfflineStatus::Failed) => return Some(Err(ExportError::DeviceFailed(device))),
                _ if prepared.elapsed() < READY_TIMEOUT => return None,
                _ => return Some(Err(ExportError::DeviceFailed(device))),
            }
        }
        Some(Ok(()))
    }

    /// The dialog's progress from the worker's.
    fn update_progress(&mut self) {
        let Some(Run {
            phase: Phase::Running(worker),
            ..
        }) = &self.export.run
        else {
            return;
        };
        let (fraction, peak_db) = worker.progress.load();
        if let Some(Dialog::Export(dialog)) = &mut self.ui.dialog {
            dialog.progress = Some(ExportProgress { fraction, peak_db });
        }
    }

    /// The worker panicked or vanished: the engine, if it came back, is replaced by a new one
    /// built from the document ([`EngineSync::recover`], the rebuild of #210), the bridges render
    /// in realtime again and the status says what happened (it takes the place of the rebuild
    /// notice, which would otherwise replace it next frame).
    fn finish_crashed(&mut self, engine: Option<DetachedEngine>, finished: Option<ExportReport>) {
        let Some(mut run) = self.export.run.take() else {
            return;
        };
        if let Phase::Running(worker) = &mut run.phase
            && let Some(thread) = worker.thread.take()
        {
            let _ = thread.join();
        }
        self.set_export_render_mode(&run, RenderMode::Realtime);
        self.engine.recover(engine);
        let _ = self.engine.take_rebuild_notice();
        self.export.take_failures();
        self.dialogs.after_flatten = None;
        self.close_export_dialog();
        let reason = self.lang.export_worker_crashed();
        if run.flatten.is_some() {
            self.flatten_failed(reason);
        } else if finished.is_some() {
            // The file was written before the panic: it is listed, the rebuild is reported.
            let path = run.job.path.clone();
            let file = path
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
            self.export.remember(path);
            self.set_status(
                self.lang.export_finished_worker_crashed(file),
                StatusKind::Error,
            );
        } else {
            self.set_status(self.lang.export_failed(reason), StatusKind::Error);
        }
    }

    /// Step 5 of the module docs.
    fn finish_export(
        &mut self,
        engine: DetachedEngine,
        result: Result<ExportReport, ExportError>,
        failed: Vec<SlotId>,
    ) {
        let Some(mut run) = self.export.run.take() else {
            return;
        };
        let cancelled = run.flatten.is_some()
            && matches!(&run.phase, Phase::Running(worker)
            if worker.cancel.load(Ordering::Relaxed));
        if let Phase::Running(worker) = &mut run.phase
            && let Some(thread) = worker.thread.take()
        {
            let _ = thread.join();
        }
        let result = result.and_then(|report| {
            if cancelled {
                return Err(ExportError::Cancelled);
            }
            run.flatten
                .as_ref()
                .and_then(|flatten| flatten.unavailable(self))
                .map_or(Ok(report), |(id, _)| Err(ExportError::DeviceFailed(id)))
        });
        self.set_export_render_mode(&run, RenderMode::Realtime);
        self.engine.attach(engine);
        self.export.collect_failures(failed);
        self.drain_events();
        let failed = self.export.take_failures();
        let dropped = self.dropped_devices(failed);
        self.close_export_dialog();
        if let Some(flatten) = run.flatten.take() {
            flatten.finish(self, result);
            return;
        }
        match result {
            Ok(_report) => {
                let path = run.job.path.clone();
                let file = path
                    .file_name()
                    .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
                let text = self.lang.export_done(
                    &file,
                    view::minutes_seconds(run.seconds),
                    run.job.bit_depth.bits(),
                ) + &view::dropped_text(self.lang, &dropped)
                    + &view::not_played_text(self.lang, &run.diagnostics);
                let kind = if dropped.is_empty() && run.diagnostics.is_empty() {
                    StatusKind::Success
                } else {
                    StatusKind::Error
                };
                let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.ui.status = Some(StatusMessage {
                    text,
                    kind,
                    since: Instant::now(),
                    link: Some((
                        self.lang.export_show_in_folder().to_owned(),
                        Box::new(Action::Project(ProjectAction::ShowInFolder(folder))),
                    )),
                });
                self.export.remember(path);
            }
            Err(ExportError::Cancelled) => {
                self.set_status(self.lang.export_cancelled(), StatusKind::Info)
            }
            Err(ExportError::WavTooLarge) => self.set_status(
                self.lang.export_failed(self.lang.export_wav_too_large()),
                StatusKind::Error,
            ),
            Err(ExportError::MissingAudio(path)) => self.set_status(
                self.lang.export_missing_audio(path.display()),
                StatusKind::Error,
            ),
            Err(ExportError::Stretch { clip, reason }) => {
                let (name, why) = self.warp_error(clip, reason);
                self.set_status(self.lang.export_warp_failed(name, why), StatusKind::Error);
            }
            Err(error @ ExportError::DeviceFailed(id)) => {
                let reason = self
                    .unavailable_render_audio_asset(id)
                    .unwrap_or_else(|| error.to_string());
                self.set_status(self.lang.export_failed(reason), StatusKind::Error);
            }
            Err(e) => self.set_status(self.lang.export_failed(e), StatusKind::Error),
        }
    }

    fn warp_error(
        &self,
        clip: ClipId,
        reason: komponist_engine::audio::StretchError,
    ) -> (String, &'static str) {
        let name = self
            .project()
            .find_clip(clip)
            .map(|(_, clip)| &clip.name)
            .filter(|name| !name.trim().is_empty())
            .map_or_else(|| clip.0.to_string(), String::clone);
        (name, crate::audio_error::reason(self.lang, reason))
    }

    fn set_export_render_mode(&mut self, run: &Run, mode: RenderMode) {
        let host = self.engine.host_mut();
        if mode == RenderMode::Realtime {
            for &device in &run.offline_devices {
                host.set_offline_preparation(device, None);
            }
        }
        if let Some(flatten) = &run.flatten {
            let ids: Vec<_> = flatten.scope.devices().collect();
            host.set_selected_render_mode(&ids, mode);
        } else {
            host.set_render_mode(mode);
        }
    }

    /// The plugin names of the devices in `slots`, without repeats.
    fn dropped_devices(&self, slots: Vec<SlotId>) -> Vec<String> {
        let Some(program) = self.engine.program() else {
            return Vec::new();
        };
        let mut names: Vec<String> = Vec::new();
        for slot in slots {
            let Some(&(device, _)) = program.slots.entries().iter().find(|(_, s)| *s == slot)
            else {
                continue;
            };
            let name = self.device_name(device);
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// Closes the export dialog (and gives the focus back) once no export runs.
    fn close_export_dialog(&mut self) {
        if matches!(self.ui.dialog, Some(Dialog::Export(_) | Dialog::Flatten))
            && !self.export.is_active()
        {
            self.close_modal_dialog();
        }
    }

    /// `ProjectAction::ShowInFolder`: opens `folder` in the file manager; says why if it fails.
    pub(crate) fn show_in_folder(&mut self, folder: &Path) {
        if let Err(e) = (self.export.opener)(folder) {
            self.set_status(self.lang.export_open_failed(e), StatusKind::Error);
        }
    }

    /// The export dialog's model for this frame.
    fn export_model(&self, dialog: &ExportDialog) -> view::Model {
        let s = &dialog.settings;
        let stage = match (dialog.stage, &self.export.run) {
            (ExportStage::Devices, Some(_)) => Stage::Devices(self.devices_not_ready()),
            (
                ExportStage::Rendering,
                Some(Run {
                    phase: Phase::Running(worker),
                    seconds,
                    ..
                }),
            ) => {
                let p = dialog.progress.unwrap_or(ExportProgress {
                    fraction: 0.0,
                    peak_db: klang_dsp::units::gain_to_db(0.0),
                });
                let fraction = f64::from(p.fraction.clamp(0.0, 1.0));
                let elapsed = worker.started.elapsed().as_secs_f64();
                let known = fraction > 0.0 && elapsed > 0.0 && worker.go.is_none();
                Stage::Progress(ProgressView {
                    fraction: p.fraction,
                    done: view::tenths(fraction * seconds),
                    total: view::tenths(*seconds),
                    speed: known.then(|| fraction * seconds / elapsed),
                    peak_db: p.peak_db,
                    remaining: known.then(|| elapsed * (1.0 - fraction) / fraction),
                    cancelling: worker.cancel.load(Ordering::Relaxed),
                })
            }
            _ => Stage::Settings(self.settings_view(s)),
        };
        view::Model {
            settings: s.clone(),
            stage,
            keys: self.dialogs.overlay.is_none(),
        }
    }

    /// The settings dialog's view of `s`.
    fn settings_view(&self, s: &ExportSettings) -> SettingsView {
        let p = self.project();
        let bar = p.meter.ticks_per_bar();
        let loop_label = self.export_ticks(ExportRange::Loop).map(|r| {
            self.lang
                .export_loop_bars(bar_number(r.start, bar), bar_number(r.end, bar))
        });
        let ticks = self
            .export_ticks(s.range)
            .unwrap_or(TickRange::new(Tick::ZERO, Tick::ZERO));
        let rate = s.sample_rate.max(1);
        let length = match self.engine.program() {
            Some(program) => {
                let frames = ExportJob::frames_of(&program.tempo_map, ticks, rate);
                (frames.end - frames.start) as f64 / f64::from(rate)
            }
            None => 0.0,
        };
        let tail = ExportJob::tail_frames(s.tail_seconds, rate);
        SettingsView {
            loop_label,
            selection: self.export_ticks(ExportRange::Selection).is_some(),
            from: fmt_bar_pos(ticks.start, p.meter),
            to: fmt_bar_pos(ticks.end, p.meter),
            length: view::tenths(length),
            folder: folder_label(&s.folder),
            can_export: view::file_name(&s.file_name).is_some()
                && (ticks.end > ticks.start || tail > 0),
        }
    }

    /// Draws the export dialog and applies what the user did (the shell calls it each frame).
    pub(crate) fn draw_export(&mut self, ctx: &egui::Context) {
        if let Some(flatten) = self
            .export
            .run
            .as_ref()
            .and_then(|run| run.flatten.as_ref())
        {
            let worker = self.export.run.as_ref().and_then(|run| match &run.phase {
                Phase::Devices => None,
                Phase::Running(worker) => Some(worker),
            });
            let fraction = worker
                .filter(|worker| worker.go.is_none())
                .map(|worker| worker.progress.load().0);
            let cancelling = worker.is_some_and(|worker| worker.cancel.load(Ordering::Relaxed));
            if view::show_flatten(ctx, &flatten.name, fraction, cancelling) {
                self.queue.push(Action::Ui(UiAction::CloseDialog));
            }
            ctx.request_repaint_after(PROGRESS_REPAINT);
            return;
        }
        let Some(Dialog::Export(dialog)) = &self.ui.dialog else {
            return;
        };
        let model = self.export_model(dialog);
        for output in view::show(ctx, &model) {
            match output {
                Output::Settings(settings) => {
                    if let Some(Dialog::Export(dialog)) = &mut self.ui.dialog
                        && !self.export.is_active()
                    {
                        dialog.settings = settings;
                    }
                }
                Output::Export => {
                    if let Some(Dialog::Export(dialog)) = &self.ui.dialog {
                        let settings = dialog.settings.clone();
                        self.queue.push(Action::Project(
                            crate::actions::ProjectAction::StartExport(settings),
                        ));
                    }
                }
                Output::ExportWithout => self.export_without(),
                Output::Cancel => self.queue.push(Action::Ui(UiAction::CloseDialog)),
                Output::ChooseFolder => {
                    let dir = model.settings.folder.clone();
                    let start = if dir.is_dir() {
                        dir
                    } else {
                        crate::dirs::start_dir().unwrap_or_else(crate::dirs::filesystem_root)
                    };
                    self.open_overlay(Overlay::File(FileBrowser::new(
                        self.lang,
                        Purpose::ExportFolder,
                        start,
                        String::new(),
                    )));
                }
            }
        }
        if self.export.is_active() {
            ctx.request_repaint_after(PROGRESS_REPAINT);
        }
    }

    /// The file browser chose the export folder.
    pub(crate) fn export_folder_chosen(&mut self, folder: PathBuf) {
        if let Some(Dialog::Export(dialog)) = &mut self.ui.dialog {
            dialog.settings.folder = folder;
        }
    }

    /// App exit during an export: stops the render and waits for the engine.
    pub(crate) fn shutdown_export(&mut self) {
        if !self.export.is_active() {
            return;
        }
        self.cancel_export();
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        while self.export.is_active() && Instant::now() < deadline {
            self.poll_export();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// How often the progress dialog repaints.
const PROGRESS_REPAINT: Duration = Duration::from_millis(50);

/// How long app exit waits for a cancelled export (a bridge may take its 2 s offline deadline).
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
