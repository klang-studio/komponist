//! The WAV export flow (S08, ARCHITECTURE §6.7): from "Exportieren" to the status bar message.
//! The dialog itself is [`crate::dialogs::export`]; the render is `komponist_engine::export`.
//!
//! 1. **Start** (`ProjectAction::StartExport`): the settings become the last used ones (saved in
//!    the settings file), the transport stops (a running take is committed) and the job is
//!    computed: the range ("Ganzer Song" = tick 0 to the end of the last clip, the loop, the
//!    selection) in frames at the export rate, the tail, the target `<folder>/<name>.wav`.
//! 2. **Devices** (PLAN WP-45: a device still loading renders silence): while a device of the
//!    project is not `Ok`, the dialog lists it ("lädt …", "fehlt", "abgestürzt") and the export
//!    waits; it goes on by itself when all run, or at once with "Ohne sie exportieren".
//! 3. **Detach**: the document is synced, the engine leaves the stream together with its handle
//!    ([`EngineSync::detach`](crate::engine_sync::EngineSync::detach)), the bridges switch to
//!    offline processing ([`DeviceHost::set_render_mode`](crate::device_host::DeviceHost)) and a
//!    worker thread takes engine, program and job.
//! 4. **Worker**: sets the export rate when it differs from the device's
//!    (`Engine::set_sample_rate` re-activates every processor) and compiles the program again
//!    against the processors in the engine (`komponist_engine::export::recompile`: delay
//!    compensation and routing latency in frames of the export rate, from the latencies the export
//!    skips), then waits for the UI. A bridge
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
//!    and the status bar says „Nachtfahrt.wav exportiert · 00:33 · 24 bit“ with „Im Ordner
//!    zeigen“ (the folder opens with `xdg-open`, local only). The file joins Projekt › Exporte
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
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use komponist_engine::export::{
    BitDepth as EngineBitDepth, ExportError, ExportJob, ExportReport, NORMALIZE_TARGET_DB,
    Progress, export, recompile,
};
use komponist_engine::{Command, Program, SlotId, TransportState};
use komponist_ipc::RenderMode;
use komponist_model::format::fmt_bar_pos;
use komponist_model::{Project, Tick, TickRange};
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

/// How long the export waits for devices re-activating at the export rate before it renders
/// without them (fallback audio).
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens a folder for "Im Ordner zeigen".
pub type FolderOpener = Box<dyn FnMut(&Path) -> io::Result<()>>;

/// The app's export state (not view state: the dialog's settings and progress are in
/// [`crate::ui_state::ExportDialog`]).
pub(crate) struct Exports {
    /// The export in progress, from "Exportieren" until the engine is back.
    run: Option<Run>,
    /// This session's exports of the open project, oldest first (Projekt › Exporte).
    pub(crate) done: Vec<PathBuf>,
    /// How "Im Ordner zeigen" opens a folder.
    opener: FolderOpener,
    /// Engine events are being drained after an export: failed slots go into `failed`.
    collecting: bool,
    /// Slots that failed during the export.
    failed: Vec<SlotId>,
}

impl Default for Exports {
    fn default() -> Exports {
        Exports {
            run: None,
            done: Vec::new(),
            opener: Box::new(xdg_open),
            collecting: false,
            failed: Vec::new(),
        }
    }
}

impl Exports {
    /// Whether an export runs (devices, render).
    pub(crate) fn is_active(&self) -> bool {
        self.run.is_some()
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
}

/// `xdg-open <folder>`, detached; the child is reaped by a thread.
fn xdg_open(folder: &Path) -> io::Result<()> {
    let mut child = std::process::Command::new("xdg-open")
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
    /// Go on after the re-activation (`true`) or stop (`false`).
    go: Option<Sender<bool>>,
    /// When the worker reported `Prepared` (the UI waits for re-activating devices).
    prepared: Option<Instant>,
    cancel: Arc<AtomicBool>,
    progress: Arc<SharedProgress>,
    /// When rendering started (speed, time left).
    started: Instant,
}

/// What the worker tells the UI.
enum Message {
    /// The engine runs at the export rate; the worker waits for the bridges (module docs, step 4).
    Prepared,
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
}

/// The worker: export rate, wait for the UI, render, device rate, engine back.
fn work(
    mut engine: DetachedEngine,
    input: Input,
    job: ExportJob,
    cancel: Arc<AtomicBool>,
    progress: Arc<SharedProgress>,
    to_ui: Sender<Message>,
    go: Receiver<bool>,
) {
    let mut failed = Vec::new();
    let device_rate = engine.engine().sample_rate();
    let export_rate = job.sample_rate as f32;
    let changed = device_rate != export_rate;
    if changed && let Err(errors) = engine.engine().set_sample_rate(export_rate) {
        failed.extend(errors.into_iter().map(|(slot, _)| slot));
    }
    let Input { project, program } = input;
    let _ = to_ui.send(Message::Prepared);
    let go_on = go.recv().unwrap_or(false);
    let result = if go_on {
        // The UI waited until the devices run at the export rate: now the compensation and the
        // skip come from the same processors, in frames of the export rate.
        let export_program = recompile(engine.engine(), &project, &program);
        let (e, h) = engine.parts();
        export(e, h, &export_program, &job, &cancel, |p| progress.store(p))
    } else {
        Err(ExportError::Cancelled)
    };
    if changed && let Err(errors) = engine.engine().set_sample_rate(device_rate) {
        failed.extend(errors.into_iter().map(|(slot, _)| slot));
    }
    // `export` queued the recompiled program behind its Stop: the stream plays the program the
    // app sent, as before the export (its slots are the same).
    let _ = engine
        .parts()
        .1
        .send(Command::SetProgram(Box::new(program)));
    // `export` queued Stop, the program and the old playhead; one frame lets them take
    // effect here, so the transport and the playhead the UI reads are right as soon as the
    // engine is back (else a Play right after the export could see the export's "playing" and
    // stop instead).
    engine.engine().render(1);
    let _ = to_ui.send(Message::Done {
        engine,
        result,
        failed,
    });
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

/// "~/Musik/Exporte" for `folder` under `$HOME`.
fn folder_label(folder: &Path) -> String {
    crate::dialogs::format::tilde(folder)
}

impl App {
    /// This session's exports of the open project, oldest first (Projekt › Exporte).
    pub fn exported(&self) -> &[PathBuf] {
        &self.export.done
    }

    /// Whether an export runs (waiting for devices or rendering).
    pub fn is_exporting(&self) -> bool {
        self.export.is_active()
    }

    /// Replaces how "Im Ordner zeigen" opens a folder (default: `xdg-open`; tests record).
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
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
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
        self.stop_for_export();
        let settings = self.export_dialog_settings();
        self.open_dialog(Dialog::Export(ExportDialog {
            settings,
            progress: None,
            stage: ExportStage::Settings,
        }));
    }

    /// The transport stops during an export (ARCHITECTURE §6.7); a take is committed first.
    fn stop_for_export(&mut self) {
        if self.transport() != TransportState::Stopped {
            self.dispatch(Action::Transport(crate::actions::TransportAction::Stop));
        }
    }

    /// "Exportieren": see the module docs (steps 1 and 2).
    pub(crate) fn start_export(&mut self, settings: ExportSettings) {
        if self.export.is_active() {
            return;
        }
        self.settings.export = Some(settings.clone());
        self.save_settings();
        self.stop_for_export();
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
        let seconds = job.frames() as f64 / f64::from(rate);
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
        self.export.run = Some(Run {
            job,
            seconds,
            phase: Phase::Devices,
        });
        self.poll_export();
    }

    /// The devices of the project that are not `Ok`, in document order (master last).
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
            for d in devices {
                let state = match self.engine.device_status(d.id) {
                    DeviceStatus::Ok => continue,
                    DeviceStatus::Loading => DeviceState::Loading,
                    DeviceStatus::Missing => DeviceState::Missing,
                    DeviceStatus::Crashed(FailReason::Exited { .. }) => DeviceState::Crashed,
                    DeviceStatus::Crashed(FailReason::LoadFailed(_)) => DeviceState::LoadFailed,
                    DeviceStatus::Crashed(FailReason::Unresponsive) => DeviceState::Unresponsive,
                };
                rows.push(DeviceRow {
                    name: d.plugin.name.clone(),
                    track: track.to_owned(),
                    state,
                });
            }
        }
        rows
    }

    /// Esc, "Abbrechen", × during an export: see the module docs.
    pub(crate) fn cancel_export(&mut self) {
        let Some(run) = &mut self.export.run else {
            return;
        };
        match &mut run.phase {
            Phase::Devices => {
                self.export.run = None;
                self.close_export_dialog();
                self.set_status(self.lang.export_cancelled(), StatusKind::Info);
            }
            Phase::Running(worker) => {
                worker.cancel.store(true, Ordering::Relaxed);
                if let Some(go) = worker.go.take() {
                    let _ = go.send(false);
                }
            }
        }
    }

    /// Advances the export: starts it when the devices are ready, shows the progress, takes
    /// the worker's messages. Called every frame.
    pub(crate) fn poll_export(&mut self) {
        let Some(run) = &self.export.run else {
            return;
        };
        if matches!(run.phase, Phase::Devices) {
            if self.devices_not_ready().is_empty() {
                self.launch_export();
            }
            return;
        }
        loop {
            let Some(Run {
                phase: Phase::Running(worker),
                ..
            }) = &mut self.export.run
            else {
                return;
            };
            match worker.messages.try_recv() {
                Ok(Message::Prepared) => worker.prepared = Some(Instant::now()),
                Ok(Message::Done {
                    engine,
                    result,
                    failed,
                }) => {
                    self.finish_export(engine, result, failed);
                    return;
                }
                Err(TryRecvError::Empty) => {
                    self.release_when_ready();
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    // The worker panicked; the engine is gone with it.
                    self.export.run = None;
                    self.close_export_dialog();
                    self.set_status(self.lang.export_failed("worker stopped"), StatusKind::Error);
                    return;
                }
            }
        }
        self.update_progress();
    }

    /// "Ohne sie exportieren": go on with the devices that are not ready.
    fn export_without(&mut self) {
        if matches!(
            self.export.run,
            Some(Run {
                phase: Phase::Devices,
                ..
            })
        ) {
            self.launch_export();
        }
    }

    /// Step 3 of the module docs.
    fn launch_export(&mut self) {
        let Some(run) = &self.export.run else {
            return;
        };
        let job = run.job.clone();
        if let Some(folder) = job.path.parent()
            && let Err(e) = std::fs::create_dir_all(folder)
        {
            self.export.run = None;
            self.close_export_dialog();
            self.set_status(self.lang.export_failed(e), StatusKind::Error);
            return;
        }
        self.engine.sync(&self.doc);
        let Some(program) = self.engine.program().cloned() else {
            return;
        };
        let project = Arc::clone(self.doc.project());
        let detached = match self.engine.detach() {
            Ok(d) => d,
            Err(e) => {
                self.export.run = None;
                self.close_export_dialog();
                self.set_status(self.lang.export_failed(e), StatusKind::Error);
                return;
            }
        };
        self.engine.host_mut().set_render_mode(RenderMode::Offline);
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(SharedProgress::new());
        let (to_ui, messages) = mpsc::channel();
        let (go, from_ui) = mpsc::channel();
        let thread = {
            let (cancel, progress) = (Arc::clone(&cancel), Arc::clone(&progress));
            std::thread::Builder::new()
                .name("komponist-export".into())
                .spawn(move || {
                    let input = Input { project, program };
                    work(detached, input, job, cancel, progress, to_ui, from_ui)
                })
        };
        let thread = match thread {
            Ok(t) => t,
            Err(e) => {
                // The closure (and with it the engine) is gone; nothing to give back.
                self.export.run = None;
                self.close_export_dialog();
                self.set_status(self.lang.export_failed(e), StatusKind::Error);
                return;
            }
        };
        if let Some(Dialog::Export(dialog)) = &mut self.ui.dialog {
            dialog.stage = ExportStage::Rendering;
        }
        if let Some(run) = &mut self.export.run {
            run.phase = Phase::Running(Worker {
                thread: Some(thread),
                messages,
                go: Some(go),
                prepared: None,
                cancel,
                progress,
                started: Instant::now(),
            });
        }
        self.update_progress();
    }

    /// The worker is ready (`Prepared`): lets it render once no device of the project is
    /// `Loading` any more (re-activation at the export rate, warm-up), or after
    /// [`READY_TIMEOUT`] (such a device renders its fallback). Module docs, step 4.
    fn release_when_ready(&mut self) {
        let Some(Run {
            phase: Phase::Running(worker),
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
        let loading = self
            .devices_not_ready()
            .iter()
            .any(|row| row.state == DeviceState::Loading);
        if loading && prepared.elapsed() < READY_TIMEOUT {
            return;
        }
        if let Some(Run {
            phase: Phase::Running(worker),
            ..
        }) = &mut self.export.run
            && let Some(go) = worker.go.take()
        {
            let go_on = !worker.cancel.load(Ordering::Relaxed);
            let _ = go.send(go_on);
            worker.started = Instant::now();
        }
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
        if let Phase::Running(worker) = &mut run.phase
            && let Some(thread) = worker.thread.take()
        {
            let _ = thread.join();
        }
        self.engine.attach(engine);
        self.engine.host_mut().set_render_mode(RenderMode::Realtime);
        self.export.failed = failed;
        self.export.collecting = true;
        self.drain_events();
        self.export.collecting = false;
        let failed = std::mem::take(&mut self.export.failed);
        let dropped = self.dropped_devices(failed);
        self.close_export_dialog();
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
                ) + &view::dropped_text(self.lang, &dropped);
                let kind = if dropped.is_empty() {
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
                self.export.done.retain(|p| *p != path);
                self.export.done.push(path);
            }
            Err(ExportError::Cancelled) => {
                self.set_status(self.lang.export_cancelled(), StatusKind::Info)
            }
            Err(e) => self.set_status(self.lang.export_failed(e), StatusKind::Error),
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
            let name = self.project().find_device(device).map_or_else(
                || self.lang.status_device_unnamed(device.0),
                |(_, _, d)| d.plugin.name.clone(),
            );
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    /// Closes the export dialog (and gives the focus back) once no export runs.
    fn close_export_dialog(&mut self) {
        if matches!(self.ui.dialog, Some(Dialog::Export(_))) && !self.export.is_active() {
            self.dispatch(Action::Ui(UiAction::CloseDialog));
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
                        std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
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
