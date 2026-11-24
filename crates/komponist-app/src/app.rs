//! The app: document, view state, engine, views, keymap and settings, with the one place that
//! applies [`Action`]s ([`App::dispatch`], ARCHITECTURE §9.3).
//!
//! A frame ([`App::show`]):
//!
//! 1. poll the engine and the device host and handle their events ([`App::device_events`]),
//!    advance the meters;
//! 2. turn this frame's key events into actions through the [`Keymap`] (text fields first, then
//!    the computer-keyboard MIDI layer, then the table) and dispatch them;
//! 3. draw the shell and the views, which queue actions;
//! 4. dispatch the queue, then [`EngineSync::sync`] the document to the engine;
//! 5. persist changed panel settings.
//!
//! [`App::headless`] builds the app on the null audio backend without a window, so tests drive it
//! through `egui_kittest` or call [`App::dispatch`] and [`App::step`] directly.
//!
//! Device host events (step 1): state snapshots go into the
//! document as tracked, coalesced undo steps ([`crate::param_undo`]), a latency change recompiles the
//! program (delay compensation), a finished scan retries the devices that were missing, crashes
//! and the warnings the user can act on go to the status bar (all warnings and errors also to
//! stderr). Saving takes a state snapshot of every bridged device first; [`App::shutdown`] shuts
//! the bridges down.
//!
//! Engine events (step 1) feed the recording ([`crate::record`]): the count-in's end starts a
//! [`Recording`], captured notes and loop wraps grow it, Stop (Leertaste, Stop, R) commits it as one
//! undo step and Esc discards it ([`TransportAction::DiscardTake`]); both drain the events still in
//! flight first.

mod clip_edits;
mod devices;
mod edit_dispatch;
mod presets;
pub(crate) mod recovery;
mod track_edits;
mod track_macro_edits;

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui::{Context, Event, FocusDirection, Ui};
use komponist_audio::{AudioError, AudioState, InputConfig};
use komponist_engine::automation::EngineTarget;
use komponist_engine::{Command, EngineEvent, TransportState};
use komponist_i18n::Lang;
use komponist_ipc::LogLevel;
use komponist_model::automation::AutomationOwner;
use komponist_model::edits::{self, EditResult, NewDevice, NewTrack, ProjectProp, TrackProp};
use komponist_model::project::BUILTIN_HEXA;
use komponist_model::{
    AudioInput, ChannelRef, ClipContent, DeviceId, DeviceRole, Document, EditError, PluginFormat,
    PluginRef, Project, Tick, TickRange, TrackId,
};
use komponist_plugins::manager::STATE_LOG_PREFIX;
use komponist_plugins::{DeviceStatus, FailReason, PluginManagerEvent};

use crate::actions::{
    Action, DropTarget, EditAction, GridTarget, PluginAction, ProjectAction, SelectMode,
    SelectionChange, SettingsAction, TransportAction, UiAction,
};
use crate::device_host::{BuiltinHost, DeviceHost};
use crate::dialogs::DialogState;
use crate::dialogs::new_track::default_names;
use crate::dialogs::settings::midi_label;
use crate::dialogs::settings::{AudioDevices, FixedDevices, SystemDevices};
use crate::engine_sync::{EngineSync, MidiChange};
use crate::keymap::{self, Key, KeyChord, Keymap};
use crate::meters::Meters;
use crate::metrics;
use crate::project_io::{self, RecentList};
use crate::record::Recording;
use crate::settings::{PanelSettings, Settings};
use crate::ui_state::{
    Dialog, DockTab, FocusArea, InspectorRename, Panel, PluginView, PluginWindow, Screen,
    StatusKind, StatusMessage, Tool, UiState,
};
use crate::views::{Requests, Views};
use crate::windows::plugin_frame::{self, PluginFrames};

/// How long a status bar message stays (`ks-view.js` `statusView`: 7000 ms).
pub const STATUS_DURATION: Duration = Duration::from_secs(7);

/// Most entries kept in the dispatch log ([`App::dispatched`]).
const LOG_LIMIT: usize = 512;

/// Velocity of computer-keyboard notes (prototype `noteOn(…, 100, …)`).
const KEYBOARD_VELOCITY: u8 = 100;

/// How often an idle app still polls a device host that has work (bridges loading, running or
/// crashing, a scan): status changes show up within this time.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Check capture availability on an otherwise idle project.
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Delay of the frame a MIDI message requests (the audio callback takes the message first).
const MIDI_REPAINT_DELAY: Duration = Duration::from_millis(10);

/// Frame interval while MIDI input holds keys, or right after a MIDI message.
const HELD_KEYS_INTERVAL: Duration = Duration::from_millis(30);

/// How long saving waits for the state snapshots of all bridged devices together.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);

/// Start of the bridge's `Log` line about its realtime priority (`komponist-bridge`
/// `instance::RT_LOG_PREFIX`); at level `Warning` the priority was refused.
const RT_LOG_PREFIX: &str = "RT priority:";

/// The application.
pub struct App {
    pub(crate) doc: Document,
    pub(crate) ui: UiState,
    pub(crate) engine: EngineSync,
    pub(crate) views: Views,
    pub(crate) keymap: Keymap,
    pub(crate) meters: Meters,
    pub(crate) settings: Settings,
    settings_path: Option<PathBuf>,
    pub(crate) project_path: Option<PathBuf>,
    /// Actions queued by the shell and the views during this frame.
    pub(crate) queue: Vec<Action>,
    log: VecDeque<Action>,
    pub(crate) shell: ShellState,
    /// How device changes are grouped into undo steps ([`crate::param_undo`]).
    pub(crate) param_undo: crate::param_undo::ParamUndo,
    preset_preparations: Vec<presets::PendingPreset>,
    user_presets: presets::UserPresets,
    /// Overlays, pending steps and caches of the dialogs and the start screen (WP-52).
    pub(crate) dialogs: DialogState,
    /// Recent projects (`recent.json` next to the settings file).
    pub(crate) recent: RecentList,
    /// Where the recent list is saved (`None`: never).
    pub(crate) recent_path: Option<PathBuf>,
    /// Drivers and devices for S07.
    pub(crate) audio_devices: Arc<dyn AudioDevices>,
    /// The take being recorded (`record`), from the end of the count-in to Stop or Esc.
    pub(crate) recording: Option<Recording>,
    pub(crate) audio_recording: crate::audio_record::State,
    pub(crate) automation_capture: crate::automation_capture::AutomationCapture,
    pub(crate) automation_controls: Vec<(AutomationOwner, EngineTarget)>,
    pub(crate) automation_stop_pending: bool,
    /// The plugin frame windows (WP-50).
    pub(crate) frames: PluginFrames,
    /// The WAV export in progress and this session's exports (WP-53b, [`crate::export`]).
    pub(crate) export: crate::export::Exports,
    /// The language of the UI (`docs/I18N.md`). [`App::new`] starts in German, the test language;
    /// `main()` sets the resolved language with [`App::with_lang`].
    pub(crate) lang: Lang,
    /// Crash recovery (#227): the copy of the open document and the copies other sessions left.
    /// Off in [`App::new`]; `main()` turns it on with [`App::with_recovery`].
    pub(crate) recovery: crate::recovery::Recovery,
    /// The recovery prompt and what a recovered document needs ([`recovery`]).
    pub(crate) recovery_ui: recovery::RecoveryUi,
}

/// Per-session state of the shell that is not view state.
#[derive(Debug, Default)]
pub(crate) struct ShellState {
    /// The successfully installed application-menu backend; otherwise the local fallback.
    pub(crate) menu_backend: crate::application_menu::Backend,
    /// Focus to restore when the dialog closes.
    focus_before_dialog: Option<FocusArea>,
    /// Computer-keyboard notes held down: physical key, MIDI note and original target track.
    pub(crate) held_notes: Vec<(Key, u8, TrackId)>,
    /// One-shot requests for the views of the next frame.
    pub(crate) requests: Requests,
    /// A UI scale change to apply at the start of the next frame.
    pending_scale: Option<Option<f32>>,
    /// Text for the system clipboard, written at the end of the frame (`ctx.copy_text`).
    pub(crate) pending_clipboard_text: Option<String>,
    /// Width of the arrangement's timeline in the last frame (for "Z: einpassen").
    pub(crate) timeline_width: f32,
    /// Destination captured when the audio-import dialog opens.
    pub(crate) audio_import_target: Option<(Option<komponist_model::TrackId>, Tick)>,
    /// The panel settings last written to the settings file.
    saved_panels: Option<PanelSettings>,
    /// Panel settings whose write failed: not tried again every frame, only once they change or
    /// at shutdown.
    unsaved_panels: Option<PanelSettings>,
    /// A splitter is being dragged this frame: the panel sizes are written once it is released,
    /// not on every frame of the drag (#221).
    pub(crate) panel_drag: bool,
    /// The last edit that failed (diagnostics, tests).
    last_error: Option<EditError>,
    /// The window title last sent ("komponist · Night Drive").
    title: Option<String>,
    /// The device host's events of this frame ([`App::device_events`]).
    device_events: Vec<PluginManagerEvent>,
    /// A text editor had focus at the start of the last frame.
    text_edit_focused: bool,
    /// Where the last recording started (or starts after its count-in).
    record_from: Option<Tick>,
    /// A `Command::Record` was sent and its `CountInDone` has not arrived: only then does the
    /// event start a take. Cleared by everything that ends the recording attempt (cancel, discard,
    /// project open), so a count-in that ends in the audio block running while that happens
    /// cannot start a take in the wrong place.
    take_expected: bool,
    /// Set by the MIDI hub's waker (a MIDI thread) when a message arrived; `None` until the
    /// waker is installed in the first frame.
    midi_seen: Option<Arc<AtomicBool>>,
}

impl App {
    /// The app on `engine` with `doc` open (screen `Project`; [`App::show_start_screen`] for S09).
    /// `settings` are applied (panel sizes); `settings_path` is where changes are saved (`None`:
    /// never), and the recent list lives next to it (`recent.json`).
    pub fn new(
        doc: Document,
        engine: EngineSync,
        settings: Settings,
        settings_path: Option<PathBuf>,
    ) -> App {
        let mut ui = UiState::new();
        ui.screen = Screen::Project;
        settings.panels.apply_to(&mut ui.layout);
        ui.browser.favorites = settings.favorites.clone();
        let saved_panels = Some(settings.panels);
        let mut recent_path = settings_path.as_deref().map(project_io::recent_path);
        // An unreadable list is reported and left alone, like the settings file: this session
        // keeps an in-memory list and never overwrites the file.
        let recent = match recent_path.as_deref().map(RecentList::load) {
            Some(Ok(list)) => list,
            Some(Err(e)) => {
                eprintln!("komponist: recent projects not loaded, keeping the file: {e}");
                recent_path = None;
                RecentList::default()
            }
            None => RecentList::default(),
        };
        App {
            doc,
            ui,
            engine,
            views: Views::new(),
            keymap: Keymap::default_bindings(),
            meters: Meters::new(),
            settings,
            settings_path,
            project_path: None,
            queue: Vec::new(),
            log: VecDeque::new(),
            shell: ShellState {
                saved_panels,
                ..ShellState::default()
            },
            param_undo: Default::default(),
            preset_preparations: Vec::new(),
            user_presets: Default::default(),
            dialogs: DialogState::default(),
            recent,
            recent_path,
            audio_devices: Arc::new(SystemDevices),
            recording: None,
            audio_recording: Default::default(),
            automation_capture: Default::default(),
            automation_controls: Vec::new(),
            automation_stop_pending: false,
            frames: PluginFrames::default(),
            export: Default::default(),
            lang: Lang::default(),
            recovery: crate::recovery::Recovery::disabled(),
            recovery_ui: Default::default(),
        }
    }

    /// The app in language `lang`. [`App::new`] never reads the environment (tests run in
    /// German whatever `LANG` says); `main()` resolves the language and calls this.
    pub fn with_lang(mut self, lang: Lang) -> App {
        self.lang = lang;
        self
    }

    /// The language of the UI.
    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// The app on the null backend with `project` open, default settings (with the null driver,
    /// as the engine runs), nothing persisted, and only the null driver in S07: for tests (and
    /// `--null-audio`).
    pub fn headless(project: Project) -> App {
        App::headless_with_midi(project, komponist_audio::MidiReceiver::disconnected())
    }

    /// [`App::headless`] with a MIDI queue that a [`komponist_audio::TestMidiSource`] feeds.
    pub fn headless_with_midi(project: Project, midi: komponist_audio::MidiReceiver) -> App {
        let engine = EngineSync::null_with_midi(Box::new(BuiltinHost::new()), midi);
        let mut settings = Settings::default();
        settings.audio.host = komponist_audio::HostKind::Null;
        let mut app = App::new(Document::new(project), engine, settings, None);
        static NEXT_RECORDING_ROOT: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        app = app.with_recording_directory(std::env::temp_dir().join(format!(
            "komponist-headless-recordings-{}-{}",
            std::process::id(),
            NEXT_RECORDING_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )));
        app.audio_devices = Arc::new(FixedDevices {
            hosts: vec![komponist_audio::HostKind::Null],
            devices: Vec::new(),
        });
        app
    }

    /// Replaces where S07 gets drivers and devices from (tests).
    pub fn with_audio_devices(mut self, devices: Box<dyn AudioDevices>) -> App {
        self.audio_devices = Arc::from(devices);
        self
    }

    /// Shows the start screen (S09) instead of the open document (no project argument).
    pub fn show_start_screen(&mut self) {
        self.ui.screen = Screen::Start;
        self.ui.focus = FocusArea::default();
        self.ui.start.reset_selection();
    }

    /// The recent projects, newest first.
    pub fn recent(&self) -> &[PathBuf] {
        &self.recent.projects
    }

    /// Replaces the device host (`main` plugs in [`crate::device_host::PluginHost`]; tests may
    /// too). The next sync creates every processor through the new host.
    pub fn with_device_host(mut self, host: Box<dyn DeviceHost>) -> App {
        self.engine.set_host(host);
        self
    }

    /// Replaces the views (WP-40 … WP-44 fill [`Views::new`]; tests may plug in their own).
    pub fn with_views(mut self, views: Views) -> App {
        self.views = views;
        self
    }

    /// The document.
    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// The current project.
    pub fn project(&self) -> &Arc<Project> {
        self.doc.project()
    }

    /// The view state.
    pub fn ui(&self) -> &UiState {
        &self.ui
    }

    /// The view state, mutable (tests, start-up).
    pub fn ui_mut(&mut self) -> &mut UiState {
        &mut self.ui
    }

    /// The engine sync (telemetry, device status, audio state).
    pub fn engine(&self) -> &EngineSync {
        &self.engine
    }

    /// The engine sync, mutable.
    pub fn engine_mut(&mut self) -> &mut EngineSync {
        &mut self.engine
    }

    /// The shortcut table.
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// The settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The settings, for tests that prepare what a session remembers.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// The file the project was opened from or saved to.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// The actions dispatched so far, oldest first (the last [`LOG_LIMIT`]).
    pub fn dispatched(&self) -> impl Iterator<Item = &Action> {
        self.log.iter()
    }

    /// Clears the dispatch log.
    pub fn clear_dispatched(&mut self) {
        self.log.clear();
    }

    /// The last edit that failed, if any.
    pub fn last_error(&self) -> Option<&EditError> {
        self.shell.last_error.as_ref()
    }

    /// The take being recorded, from the end of the count-in until Stop or Esc.
    pub fn recording(&self) -> Option<&Recording> {
        self.recording.as_ref()
    }

    /// Transport state from the engine (`Stopped` during an export).
    pub fn transport(&self) -> TransportState {
        self.engine.transport()
    }

    /// Whether play and record are possible (an audio device or the null backend runs).
    pub fn audio_ok(&self) -> bool {
        !matches!(self.engine.audio_state(), AudioState::NoDevice { .. })
    }

    /// Shows a message in the status bar.
    pub fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        self.ui.status = Some(StatusMessage {
            text: text.into(),
            kind,
            since: Instant::now(),
            link: None,
        });
    }

    /// Tells in the status bar that the saved audio input could not be started (at startup), with
    /// the shortcut of the settings, and logs the reason.
    pub fn report_audio_input_failed(&mut self, error: &AudioError) {
        eprintln!("komponist: audio input not started: {error}");
        let shortcut = self
            .keymap
            .label_for(
                &Action::Ui(UiAction::OpenSettings(
                    crate::ui_state::SettingsSection::Audio,
                )),
                self.lang,
            )
            .unwrap_or_default();
        self.set_status(
            self.lang.status_audio_input_failed(&shortcut),
            StatusKind::Error,
        );
    }

    /// Tells in the status bar that the engine's worker threads run without realtime priority
    /// (same kind as the bridge's hint, `handle_device_event`). Once per stream start.
    pub fn report_workers_without_realtime(&mut self) {
        self.set_status(self.lang.status_workers_no_realtime(), StatusKind::Info);
    }

    /// Tells in the status bar that the engine's worker threads could not join the audio
    /// workgroup (macOS). Once per stream start.
    pub fn report_workers_without_workgroup(&mut self) {
        self.set_status(self.lang.status_workers_no_workgroup(), StatusKind::Info);
    }

    /// Tells in the status bar that the audio engine panicked and a new one was built from the
    /// document (#210, `EngineSync::take_rebuild_notice`).
    pub fn report_engine_rebuilt(&mut self) {
        self.set_status(self.lang.status_engine_rebuilt(), StatusKind::Error);
    }

    /// The engine was rebuilt after a panic with the playhead at `at` (#210). An automation
    /// write pass ends there as at a stop (the old engine sends no stop boundary any more). A
    /// running take (MIDI or audio) ends there like an interruption and keeps what was recorded
    /// so far, with the pass; without one, the pass is committed on its own. A count-in that was
    /// running never ends, so no take waits for it. Returns whether a take ended (the caller
    /// reports the rebuild).
    fn end_take_after_rebuild(&mut self, at: Tick) -> bool {
        self.shell.take_expected = false;
        // The new engine's playhead is not `at` yet: end the writes here, before anything takes
        // the pass at the playhead.
        self.end_automation_writes_at(at);
        if self.recording.is_none() && !self.audio_recording.is_active() {
            if self.automation_capture.active() {
                self.commit_automation_capture();
            }
            return false;
        }
        // The crash may have lost the running lap's last events.
        let lap = self
            .recording
            .as_ref()
            .map_or(0, |recording| recording.laps().saturating_sub(1) as u32);
        self.engine_event(&EngineEvent::RecordingInterrupted {
            at,
            first_incomplete_lap: lap,
            triggering_track: None,
        });
        true
    }

    /// Tells in the status bar what the engine leaves out of the project (#92) when the last
    /// syncs left out something new: every left-out track and device, counted per reason
    /// ([`crate::diagnostics::summary`]).
    pub fn report_compile_diagnostics(&mut self) {
        if !self.engine.take_diagnostics_notice() {
            return;
        }
        if let Some(text) = crate::diagnostics::summary(self.lang, self.engine.diagnostics()) {
            self.set_status(text, StatusKind::Error);
        }
    }

    /// Tells in the status bar which MIDI inputs appeared or vanished in the last scans
    /// (`EngineSync::take_midi_changes`), by the name S07 shows. Several changes make one
    /// message, joined by " · ".
    pub fn report_midi_changes(&mut self) {
        let changes = self.engine.take_midi_changes();
        if changes.is_empty() {
            return;
        }
        let ports = self
            .engine
            .midi()
            .map(|hub| hub.ports())
            .unwrap_or_default();
        let all: Vec<&str> = ports.iter().map(|p| p.name.as_str()).collect();
        let texts: Vec<String> = changes
            .iter()
            .map(|change| match change {
                MidiChange::Appeared(name) => {
                    self.lang.status_midi_connected(midi_label(name, &all))
                }
                MidiChange::Vanished(name) => {
                    self.lang.status_midi_disconnected(midi_label(name, &all))
                }
            })
            .collect();
        self.set_status(texts.join(" · "), StatusKind::Info);
    }

    /// The device host's events polled at the start of this frame, after the app handled them:
    /// views and plugin windows react to what concerns them (`GuiOpened`, `GuiSizeRequest`,
    /// `GuiClosed` for the frame windows; `ParamsChanged`, `NoteNamesChanged`, …). Replaced at
    /// the next frame.
    pub fn device_events(&self) -> &[PluginManagerEvent] {
        &self.shell.device_events
    }

    /// Starts a background scan of the plugin folders ([`Settings::plugin_dirs`]); devices whose
    /// plugin it finds load when it finishes.
    pub fn scan_plugins(&mut self) {
        let dirs = self.settings.plugin_dirs();
        self.engine.host_mut().start_scan(&dirs);
    }

    /// One frame without a window (tests, headless tools): engine and device host events, queued
    /// actions, sync. Meters are not advanced.
    pub fn step(&mut self) {
        self.poll_events();
        self.poll_export();
        self.flush_queue();
        self.engine.sync(&self.doc);
        self.sync_automation_preview();
    }

    // --------------------------------------------------------------------------------------------
    // Frame
    // --------------------------------------------------------------------------------------------

    /// One frame: engine, keys, shell and views, dispatch, sync (module docs).
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        komponist_ui::set_lang(&ctx, self.lang);
        // The root node carries the language for screen readers (BCP 47).
        ctx.accesskit_node_builder(egui::accesskit_root_id(), |n| {
            n.set_language(self.lang.code());
        });
        if let Some(scale) = self.shell.pending_scale.take() {
            ctx.set_pixels_per_point(scale.unwrap_or(1.0));
        }
        // Closing the window with unsaved changes asks first (`App::close_requested`).
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.take_close() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.shell.midi_seen.is_none() {
            self.install_midi_waker(&ctx);
        }
        self.poll_engine(ctx.input(|i| i.time));
        self.poll_export();
        komponist_ui::widgets::number_field::prepare_value_control_input(&ctx);
        self.handle_keys(&ctx);
        // The whole window, whatever frame the host put around `ui` (kittest adds a margin).
        let screen = ctx.content_rect();
        let mut root = ui.new_child(egui::UiBuilder::new().max_rect(screen));
        root.set_clip_rect(screen);
        self.shell.panel_drag = false;
        self.draw_shell(&mut root);
        self.shell.requests = Requests::default();
        self.flush_queue();
        if komponist_ui::lang(&ctx) != self.lang {
            // A language switch: the next frame draws every text anew, at once.
            ctx.request_repaint();
        }
        if let Some(text) = self.shell.pending_clipboard_text.take() {
            ctx.copy_text(text);
        }
        self.sync_pads(&ctx);
        self.release_auditions(&ctx);
        self.update_plugin_frames(&ctx);
        self.engine.sync(&self.doc);
        self.sync_automation_preview();
        self.update_recovery(&ctx);
        self.persist_panels();
        self.update_title(&ctx);
        self.schedule_repaint(&ctx);
    }

    /// The window title follows the project name ("komponist · Night Drive"); the start screen
    /// shows the wordmark alone.
    fn update_title(&mut self, ctx: &Context) {
        let title = if self.ui.screen == Screen::Start {
            crate::panels::transport::WORDMARK.to_owned()
        } else {
            window_title(&self.project().name)
        };
        if self.shell.title.as_ref() != Some(&title) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.shell.title = Some(title);
        }
    }

    /// Dispatches the queued actions in order (and those they queue).
    pub fn flush_queue(&mut self) {
        while !self.queue.is_empty() {
            for action in std::mem::take(&mut self.queue) {
                self.dispatch(action);
            }
        }
    }

    fn poll_engine(&mut self, now: f64) {
        self.poll_events();
        // The meters do not show an export's render.
        if !self.engine.is_detached() {
            self.meters.update(&self.engine, now);
        }
    }

    /// Drains the engine's and the device host's events and handles them (this frame's).
    fn poll_events(&mut self) {
        self.shell.device_events.clear();
        self.drain_events();
    }

    /// Handles the events that arrived since the last drain; the device events are added to this
    /// frame's.
    pub(crate) fn drain_events(&mut self) {
        self.drain_events_with_automation(true);
    }

    fn drain_events_with_automation(&mut self, capture: bool) {
        let events = self.engine.poll_engine_events();
        // Only a device stream can drop out; on the null backend (tests, `--null-audio`, CI runners
        // without RTKit) the missing priority is not worth a notice.
        let device_stream = matches!(
            self.engine.audio_state(),
            AudioState::Running { config, .. } if config.host != komponist_audio::HostKind::Null
        );
        let rebuilt = self.engine.take_rebuild_notice();
        let no_realtime = self.engine.take_worker_notice();
        let no_workgroup = self.engine.take_workgroup_notice();
        // One message per frame; a rebuilt engine is the worst news (reported last, below, so no
        // other message of this drain replaces it), then a missing priority.
        let error_shown = matches!(&self.ui.status, Some(s) if s.kind == StatusKind::Error);
        match worker_notice(
            device_stream,
            rebuilt.is_some(),
            error_shown,
            no_realtime,
            no_workgroup,
        ) {
            Some(WorkerNotice::NoRealtime) => self.report_workers_without_realtime(),
            Some(WorkerNotice::NoWorkgroup) => self.report_workers_without_workgroup(),
            None => {}
        }
        self.report_midi_changes();
        self.report_compile_diagnostics();
        let playhead = self.engine.playhead();
        for event in events {
            if let EngineEvent::PluginParams {
                slot,
                device,
                ref events,
            } = event
            {
                self.native_plugin_params(slot, device, events, capture);
            } else if capture {
                self.capture_automation(&event);
            }
            self.engine_event(&event);
        }
        // After the old engine's last events: the take they belong to ends at the crash.
        let kept_take = rebuilt.is_some_and(|at| self.end_take_after_rebuild(at));
        let mut recompile = false;
        for event in self.engine.poll_device_events() {
            recompile |= matches!(
                event,
                PluginManagerEvent::LatencyChanged { .. }
                    | PluginManagerEvent::ParamsChanged { .. }
                    | PluginManagerEvent::StatusChanged { .. }
            );
            self.handle_device_event(&event);
            self.shell.device_events.push(event);
        }
        self.poll_preset_preparations();
        self.poll_user_presets();
        if let Some(rec) = &mut self.recording {
            rec.update(playhead);
        }
        self.update_audio_take();
        if recompile {
            // Delay compensation follows the new latencies; the slots stay as they are.
            self.engine.send_project(self.doc.project());
        }
        self.flush_deferred();
        if rebuilt.is_some() {
            if kept_take {
                self.set_status(self.lang.status_engine_rebuilt_take(), StatusKind::Error);
            } else {
                self.report_engine_rebuilt();
            }
        }
    }

    /// The recording's part of an engine event: the count-in's end starts the take, captured
    /// notes and loop wraps go into it.
    fn engine_event(&mut self, event: &EngineEvent) {
        match *event {
            EngineEvent::CountInDone { at } => {
                if std::mem::take(&mut self.shell.take_expected) {
                    self.begin_take(at);
                }
            }
            EngineEvent::RecordingInterrupted {
                at,
                first_incomplete_lap,
                triggering_track,
            } => {
                if self.recording.is_none()
                    && triggering_track.is_some()
                    && let Some(audio) = self.audio_recording.running()
                {
                    // An audio recording can gain its first MIDI input after capture starts.
                    let project = self.doc.project();
                    let start = if first_incomplete_lap == 0 {
                        audio.start
                    } else {
                        project.loop_range.start
                    };
                    self.recording = Some(Recording::new(project, start, self.lang));
                }
                if let Some(recording) = &mut self.recording {
                    recording.interrupt(
                        at,
                        first_incomplete_lap,
                        triggering_track,
                        self.doc.project(),
                    );
                }
                if self.recording.is_some() || self.audio_recording.is_active() {
                    self.finish_take_at(at);
                    self.set_status(self.lang.record_interrupted(), StatusKind::Error);
                }
            }
            // A device that dropped out of an export's render (its status says the rest).
            EngineEvent::ProcessorFailed { slot } => self.export.processor_failed(slot),
            _ => {
                if let Some(rec) = &mut self.recording {
                    rec.handle(event, self.doc.project());
                }
            }
        }
    }

    /// Capture started at `at`: the take grows from there, and the editor shows it (prototype
    /// `startRec`).
    fn begin_take(&mut self, at: Tick) {
        // A take still open belongs to an earlier capture; it is kept.
        if self.recording.is_some() {
            self.finish_take();
        }
        let midi = Recording::new(self.doc.project(), at, self.lang);
        self.recording = midi.first().is_some().then_some(midi);
        self.set_dock_tab(DockTab::Editor);
        if self.ui.dialog.is_none() {
            self.ui.focus = self.live_editor_area().unwrap_or(FocusArea::PianoRoll);
        }
    }

    /// Punch-out: commits the running take and stops capturing; playback
    /// goes on. Nothing happens without a take.
    pub(crate) fn stop_take(&mut self) {
        self.drain_events();
        let capturing_notes_or_audio = self.recording.is_some() || self.audio_recording.is_active();
        let capturing_automation = self.automation_capture.active();
        if capturing_notes_or_audio || capturing_automation {
            self.engine.send(Command::FinishAutomationWrite);
            self.finish_take();
        }
        if capturing_notes_or_audio {
            self.engine.send(Command::StopRecording);
        }
    }

    /// Whether capture has unsaved material that should trigger a prompt when leaving (#85).
    pub(crate) fn has_unsaved_take(&self) -> bool {
        self.recording.as_ref().is_some_and(Recording::has_material)
            || self.audio_recording.has_material()
            || self.automation_capture.has_points()
            || (self.audio_recording.is_active() && self.transport() == TransportState::Recording)
    }

    /// The project as a save writes it while capturing (#85): the document with the running take
    /// as Stop would commit it now. Call after draining capture events. Capture goes on.
    pub(crate) fn project_with_take(
        &mut self,
        project: &Project,
        playhead: Tick,
        recordings: &mut crate::audio_record::StagedRecordings,
    ) -> Result<Option<Project>, ()> {
        let mut project = project.clone();
        let mut changed = false;
        if let Some(rec) = self
            .recording
            .as_ref()
            .filter(|recording| recording.has_material())
            && let Some(commit) = rec.commit(&project, playhead)
        {
            match commit.apply(&project) {
                Ok((next, _)) => {
                    project = next;
                    changed = true;
                }
                Err(error) => {
                    self.record_edit_error(error);
                    return Err(());
                }
            }
        }
        if let Some(next) = self
            .automation_capture
            .project_at(&project, playhead)
            .map_err(|error| {
                self.record_edit_error(error);
            })?
        {
            project = next;
            changed = true;
        }
        if let Some(next) = recordings
            .project_with_audio_take(&project, self.lang)
            .map_err(|error| {
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
            })?
        {
            project = next;
            changed = true;
        }
        Ok(changed.then_some(project))
    }

    /// Commits the running take from the already-drained events.
    pub(crate) fn finish_take(&mut self) {
        self.finish_take_at(self.engine.playhead());
    }

    fn finish_take_at(&mut self, at: Tick) {
        let commit = self
            .recording
            .take()
            .and_then(|recording| recording.commit(self.doc.project(), at));
        let automation = self.take_automation_capture();
        if self.audio_recording.is_active() {
            self.audio_recording.defer_midi(commit);
            self.audio_recording.defer_automation(automation);
            self.audio_recording.stop();
            return;
        }
        let Some(commit) = commit else {
            if automation.has_points() {
                self.end_gesture();
                self.apply("undo-automation-record", |p| automation.apply(p));
            }
            return;
        };
        // The take is an undo step of its own, also in the middle of a drag.
        self.doc.end_gesture();
        let label = commit.label().to_owned();
        let Some(clips) = self.apply_with(&label, |p| {
            let project = automation.apply(p)?;
            commit.apply(&project)
        }) else {
            return;
        };
        if let Some(&first) = clips.first() {
            self.ui_action(UiAction::OpenClip(first));
        }
        if commit.incomplete() {
            self.set_status(self.lang.record_notes_lost(), StatusKind::Error);
        } else if let Some((take, notes)) = commit.summary() {
            self.set_status(self.lang.record_recorded(take, notes), StatusKind::Success);
        }
    }

    /// Esc while recording (or "Nicht speichern" before leaving the project): the running take is
    /// gone, playback goes on.
    pub(crate) fn discard_take(&mut self) {
        self.shell.take_expected = false;
        self.recording = None;
        self.discard_automation_capture();
        self.audio_recording.cancel();
        self.drain_events_with_automation(false);
        self.engine.send(Command::StopRecording);
    }

    /// The app's part of a device host event (module docs).
    fn handle_device_event(&mut self, event: &PluginManagerEvent) {
        match event {
            PluginManagerEvent::StateSnapshot { device, state } => {
                self.store_snapshot(*device, state);
            }
            PluginManagerEvent::WaveformMarkersEdited { device, result, .. } => {
                use komponist_ipc::waveform_markers::EditStatus;
                use komponist_plugins::WaveformMarkerEditResult;

                match result {
                    WaveformMarkerEditResult::Applied {
                        previous_state,
                        state,
                    } => {
                        self.record_waveform_marker_edit(*device, previous_state, state);
                    }
                    WaveformMarkerEditResult::Rejected(EditStatus::Unchanged) => {}
                    WaveformMarkerEditResult::Rejected(_) => {
                        self.set_status(self.lang.devices_marker_edit_failed(), StatusKind::Error);
                    }
                    WaveformMarkerEditResult::Failed(message) => {
                        eprintln!("komponist: marker edit for device {}: {message}", device.0);
                        self.set_status(self.lang.devices_marker_edit_failed(), StatusKind::Error);
                    }
                }
            }
            PluginManagerEvent::ClapPresetLoaded {
                device,
                previous_state,
                state,
            } => {
                self.record_clap_preset(*device, previous_state, state);
                self.frame_preset_finished(*device, true);
            }
            PluginManagerEvent::ClapPresetCaptured { device, result } => {
                self.user_preset_captured(*device, result);
            }
            PluginManagerEvent::ClapPresetFailed { device, message } => {
                self.frame_preset_finished(*device, false);
                eprintln!("komponist: preset load for device {}: {message}", device.0);
                self.set_status(self.lang.devices_preset_load_failed(), StatusKind::Error);
            }
            PluginManagerEvent::StatusChanged {
                device,
                status: DeviceStatus::Ok,
            } => self.device_up(*device),
            PluginManagerEvent::StateApplied { device } => {
                self.cancel_frame_preset_requests(*device)
            }
            PluginManagerEvent::StatusChanged {
                device,
                status: DeviceStatus::Crashed(reason),
            } => {
                self.cancel_frame_preset_requests(*device);
                let name = self.device_name(*device);
                eprintln!("komponist: device {} ({name}): {reason:?}", device.0);
                let text = match reason {
                    FailReason::Exited { .. } => self.lang.status_device_crashed(&name),
                    FailReason::Unresponsive => self.lang.status_device_unresponsive(&name),
                    FailReason::LoadFailed(_) => self.lang.status_device_load_failed(&name),
                };
                self.set_status(text, StatusKind::Error);
            }
            PluginManagerEvent::ScanFinished(_) => {
                let newly_missing = self.sync_catalog_devices();
                if self.dialogs.missing_check || newly_missing {
                    self.check_missing_plugins();
                }
            }
            PluginManagerEvent::Log {
                device,
                level: level @ (LogLevel::Warning | LogLevel::Error),
                message,
            } => {
                // Lines without device come from the scan.
                let name = device.map_or_else(
                    || self.lang.status_plugin_scan().to_owned(),
                    |d| self.device_name(d),
                );
                eprintln!("komponist: {name}: {level:?}: {message}");
                if *level != LogLevel::Warning {
                    // Errors come with a failure the device status already shows.
                    return;
                }
                if message.starts_with(STATE_LOG_PREFIX) {
                    let text = self.lang.status_state_load_failed(&name);
                    self.set_status(text, StatusKind::Error);
                } else if message.starts_with(RT_LOG_PREFIX) {
                    let text = self.lang.status_plugin_no_realtime(&name);
                    self.set_status(text, StatusKind::Info);
                } else {
                    // The plugin's or bridge's own words.
                    self.set_status(format!("{name}: {message}"), StatusKind::Info);
                }
            }
            _ => {}
        }
    }

    /// After a tracked document change that did not come from the running processors (an edit, undo,
    /// redo, a cancelled gesture; not a knob turn, which `PluginAction::SetParam` has sent already):
    /// every device whose state changed is brought in sync with the running processor (`EngineSync::sync_device_states`), since
    /// a state change is not otherwise sent to the engine.
    pub(crate) fn follow_device_states(&mut self, before: &Arc<Project>) {
        let after = Arc::clone(self.doc.project());
        if Arc::ptr_eq(before, &after) {
            return;
        }
        let dir = self.project_dir();
        self.engine
            .sync_device_states(before, &after, dir.as_deref());
        self.expect_echo(before, &after);
        self.release_keyboard_notes_if(|id| after.track(id).is_none());
    }

    /// Apply changes in catalog availability without replacing the project's saved devices.
    fn sync_catalog_devices(&mut self) -> bool {
        let project = Arc::clone(self.doc.project());
        let host = self.engine.host();
        let changes: Vec<_> = project
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter())
            .chain(project.master.devices.iter())
            .filter_map(|device| {
                let available = host.catalog().resolve(&device.plugin).is_some();
                let missing = host.status(device.id) == DeviceStatus::Missing;
                (available == missing).then_some((device.id, available))
            })
            .collect();
        for &(device, _) in &changes {
            self.engine.reload_device(&project, device);
        }
        if let Some(crate::ui_state::Dialog::MissingPlugins(missing)) = &mut self.ui.dialog {
            *missing = self.engine.host().missing_plugins(&project);
        }
        changes.iter().any(|&(_, available)| !available)
    }

    /// The plugin name of `device` for messages ("Surge XT"), its id if it is gone.
    pub(crate) fn device_name(&self, device: DeviceId) -> String {
        self.project().find_device(device).map_or_else(
            || self.lang.status_device_unnamed(device.0),
            |(_, _, d)| {
                crate::views::catalog_plugin_name(
                    self.lang,
                    self.engine.host().catalog(),
                    &d.plugin,
                )
            },
        )
    }

    /// MIDI input wakes the idle UI, so held keys (piano-roll keyboard) and the activity dots
    /// show without other input: a frame [`MIDI_REPAINT_DELAY`] after a message, when the audio
    /// callback has usually taken it.
    fn install_midi_waker(&mut self, ctx: &Context) {
        let seen = Arc::new(AtomicBool::new(false));
        if let Some(hub) = self.engine.midi() {
            let (ctx, seen) = (ctx.clone(), Arc::clone(&seen));
            hub.set_waker(move || {
                seen.store(true, Ordering::Relaxed);
                ctx.request_repaint_after(MIDI_REPAINT_DELAY);
            });
        }
        self.shell.midi_seen = Some(seen);
    }

    fn schedule_repaint(&self, ctx: &Context) {
        if self.transport() != TransportState::Stopped
            || self.meters.is_active()
            || !self.shell.device_events.is_empty()
        {
            ctx.request_repaint();
            return;
        }
        // After a MIDI message one more frame (the audio callback may have been slower than the
        // waker's frame), and frames while keys are held, so their release shows.
        let midi = self
            .shell
            .midi_seen
            .as_ref()
            .is_some_and(|seen| seen.swap(false, Ordering::Relaxed));
        if midi || self.engine.telemetry().held_input_keys() != 0 {
            ctx.request_repaint_after(HELD_KEYS_INTERVAL);
        }
        if let Some(status) = &self.ui.status {
            let left = STATUS_DURATION.saturating_sub(status.since.elapsed());
            if !left.is_zero() {
                ctx.request_repaint_after(left);
            }
        }
        let jack_routes = self.settings.audio.host == komponist_audio::HostKind::Jack
            && (!self.settings.audio.jack_routes.is_automatic()
                || self.settings.audio_input.is_some());
        if self.engine.host().needs_polling()
            || jack_routes
            || !self.preset_preparations.is_empty()
            || self.user_presets.is_busy()
        {
            ctx.request_repaint_after(DEVICE_POLL_INTERVAL);
        } else if self.engine.audio_input_config().is_some()
            && matches!(
                self.engine.audio_state(),
                AudioState::Running { config, .. } if config.host != komponist_audio::HostKind::Null
            )
        {
            ctx.request_repaint_after(CAPTURE_POLL_INTERVAL);
        }
        // A frame now and then looks for MIDI devices that were plugged in meanwhile.
        if self.engine.midi().is_some() {
            ctx.request_repaint_after(crate::engine_sync::MIDI_SCAN_INTERVAL);
        }
    }

    /// Saves changed panel sizes, except while a splitter is dragged: a drag is saved once it is
    /// released, not on every frame (#221).
    fn persist_panels(&mut self) {
        if !self.shell.panel_drag {
            self.persist_panel_layout(false);
        }
    }

    /// Writes changed panel sizes. They count as saved only once the write worked; a failed
    /// layout is written again when it changes, or with `retry` (shutdown).
    fn persist_panel_layout(&mut self, retry: bool) {
        let panels = PanelSettings::from_layout(&self.ui.layout);
        if self.shell.saved_panels == Some(panels) {
            // On disk already; a failed write of another layout may have left that one here.
            self.settings.panels = panels;
            self.shell.unsaved_panels = None;
            return;
        }
        if !retry && self.shell.unsaved_panels == Some(panels) {
            return;
        }
        self.settings.panels = panels;
        if !self.save_settings() {
            self.shell.unsaved_panels = Some(panels);
        }
    }

    /// Writes the settings file (if the app has one); failures go to the status bar. `false`
    /// if the write failed.
    pub fn save_settings(&mut self) -> bool {
        let Some(path) = self.settings_path.clone() else {
            return true;
        };
        if let Err(e) = self.settings.save(&path) {
            self.set_status(e.to_string(), StatusKind::Error);
            return false;
        }
        // Any write saves the panel sizes in `settings` too.
        self.shell.saved_panels = Some(self.settings.panels);
        self.shell.unsaved_panels = None;
        true
    }

    /// Stops the engine, shuts the plugin bridges down and saves the settings (window closed). A
    /// running export is cancelled first, so the engine is back.
    pub fn shutdown(&mut self) {
        // Quitting normally: the recovery copy goes (§2.3 of the crash-recovery proposal).
        self.recovery.shutdown();
        self.shutdown_export();
        self.persist_panel_layout(true);
        self.shutdown_plugin_frames();
        self.audio_recording
            .shutdown(crate::audio_record::TAKE_FINISH_TIMEOUT);
        self.engine.shutdown();
        self.engine.host_mut().shutdown();
    }

    // --------------------------------------------------------------------------------------------
    // Keys
    // --------------------------------------------------------------------------------------------

    /// Whether the computer keyboard plays notes: while a track is armed (ARCHITECTURE §13.2) or
    /// when switched on explicitly, and never in a dialog.
    pub fn keyboard_midi_active(&self) -> bool {
        self.ui.screen == Screen::Project
            && self.ui.dialog.is_none()
            && self.dialogs.overlay.is_none()
            && (self.ui.keyboard_midi || self.project().tracks.iter().any(|t| t.arm))
    }

    /// Turns this frame's key events into actions and dispatches them (module docs). Keys that
    /// trigger something are consumed, so widgets do not see them.
    fn handle_keys(&mut self, ctx: &Context) {
        let wants = ctx.egui_wants_keyboard_input();
        // An Escape that dismissed a text entry belongs to that editor.
        let text_focused = ctx.text_edit_focused();
        let text_focus_just_lost =
            std::mem::replace(&mut self.shell.text_edit_focused, text_focused) && !wants;
        // Tab always leaves the maximised mixer, even while a widget has the focus (ring) (owner
        // decision Q9, 2026-11-14); a text field or a dialog keeps its Tab, and every other key
        // stays with the focused widget.
        let layout = &self.ui.layout;
        let tab_only = wants
            && layout.dock_open
            && layout.dock_maximized
            && layout.dock_tab == DockTab::Mixer
            && self.ui.dialog.is_none()
            && self.dialogs.overlay.is_none()
            && !text_focused;
        if wants && !tab_only {
            return;
        }
        let start = self.ui.screen == Screen::Start;
        // An open dropdown list or menu takes Esc and the arrows itself.
        let floating = komponist_ui::widgets::floating::current(ctx).is_some();
        let events = ctx.input(|i| i.events.clone());
        // A widget reached with Tab (focus ring visible) takes Space, Enter, arrows and Tab.
        let widget_keys =
            komponist_ui::focus::is_visible(ctx) && ctx.memory(|m| m.focused().is_some());
        for event in events {
            let Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } = event
            else {
                continue;
            };
            plugin_frame::keys::log(format!(
                "main: {key:?} pressed={pressed} repeat={repeat} {modifiers:?}"
            ));
            // `command` is Ctrl on Linux and ⌘ on macOS: the shortcut modifier (a macOS Ctrl is
            // not a shortcut).
            let ctrl = modifiers.command;
            let physical = key_from_egui(physical_key.unwrap_or(key));
            // A held note ends on release even if the layer turned off meanwhile.
            let held =
                physical.is_some_and(|k| self.shell.held_notes.iter().any(|(h, _, _)| *h == k));
            let layer = self.keyboard_midi_active() && !ctrl && !modifiers.alt;
            if let Some(k) = physical
                && let Some(note) = keymap::note_for(k)
                && (layer || (held && !pressed))
            {
                ctx.input_mut(|i| i.consume_key(modifiers, key));
                self.keyboard_note(k, note, pressed, repeat);
                continue;
            }
            if !pressed {
                continue;
            }
            let Some(k) = key_from_egui(key) else {
                continue;
            };
            if tab_only && !(k == Key::Tab && !modifiers.shift && !modifiers.alt) {
                continue;
            }
            // Shift is part of typing "+" on some layouts.
            let shift = modifiers.shift && !matches!(k, Key::Plus | Key::Minus);
            let chord = KeyChord {
                key: k,
                mods: keymap::Modifiers {
                    command: ctrl,
                    shift,
                    alt: modifiers.alt,
                },
            };
            let navigation = matches!(
                k,
                Key::Space
                    | Key::Enter
                    | Key::Tab
                    | Key::ArrowUp
                    | Key::ArrowDown
                    | Key::ArrowLeft
                    | Key::ArrowRight
            );
            if (widget_keys && navigation && !ctrl && !tab_only)
                || (floating && navigation && k != Key::Tab && !modifiers.any())
            {
                continue;
            }
            let Some(action) = self.keymap.lookup(chord, self.ui.focus).cloned() else {
                continue;
            };
            if (floating && k == Key::Escape)
                || (text_focus_just_lost && k == Key::Escape)
                || (self.dialogs.overlay.is_some() && !closes_dialog(&action))
                || (start && !on_start_screen(&action))
            {
                continue;
            }
            ctx.input_mut(|i| i.consume_key(modifiers, key));
            if k == Key::Tab {
                // The canvas takes Tab; egui must not move the widget focus.
                ctx.memory_mut(|m| m.move_focus(FocusDirection::None));
            }
            if repeat && !repeatable(&action) {
                continue;
            }
            let action = self.remap(action);
            self.dispatch(action);
        }
    }

    /// Context-dependent meaning of a shortcut: Esc while recording discards the take; else Esc
    /// during a drag cancels its gesture (the selection stays).
    fn remap(&self, action: Action) -> Action {
        match action {
            Action::Ui(UiAction::ClearSelection)
                if self.dialogs.overlay.is_some() || self.ui.screen == Screen::Start =>
            {
                Action::Ui(UiAction::CloseDialog)
            }
            Action::Ui(UiAction::ClearSelection)
                if self.recording.is_some()
                    || self.audio_recording.is_active()
                    || self.automation_capture.active()
                    || matches!(
                        self.transport(),
                        TransportState::Recording | TransportState::CountIn
                    ) =>
            {
                Action::Transport(TransportAction::DiscardTake)
            }
            Action::Ui(UiAction::ClearSelection) if self.doc.in_gesture() => {
                Action::Edit(EditAction::CancelGesture)
            }
            Action::Ui(UiAction::ClearSelection) if self.ui.track_macros.mapping.is_some() => {
                Action::Macro(crate::views::track_macros::Action::ExitMapping)
            }
            other => other,
        }
    }

    pub(crate) fn keyboard_note(&mut self, key: Key, note: u8, pressed: bool, repeat: bool) {
        if pressed {
            if repeat || self.shell.held_notes.iter().any(|(k, _, _)| *k == key) {
                return;
            }
            let Some(track) = self
                .live_track()
                .filter(|id| self.project().track(*id).is_some())
            else {
                return;
            };
            self.shell.held_notes.push((key, note, track));
            self.dispatch(Action::Transport(TransportAction::LiveNote {
                track: Some(track),
                key: note,
                velocity: KEYBOARD_VELOCITY,
            }));
        } else if let Some(i) = self.shell.held_notes.iter().position(|(k, _, _)| *k == key) {
            let (_, note, track) = self.shell.held_notes.remove(i);
            if self.project().track(track).is_none() {
                return;
            }
            self.dispatch(Action::Transport(TransportAction::LiveNote {
                track: Some(track),
                key: note,
                velocity: 0,
            }));
        }
    }

    /// Releases keys before their target or keyboard layer disappears. Explicit targets prevent
    /// late releases from reaching another selected track or a new document reusing the same id.
    fn release_keyboard_notes_if(&mut self, mut release: impl FnMut(TrackId) -> bool) {
        let mut index = 0;
        while index < self.shell.held_notes.len() {
            let (_, key, track) = self.shell.held_notes[index];
            if release(track) {
                self.shell.held_notes.remove(index);
                self.engine.send(Command::LiveNote {
                    track,
                    key,
                    velocity: 0,
                });
            } else {
                index += 1;
            }
        }
    }

    // --------------------------------------------------------------------------------------------
    // Dispatch
    // --------------------------------------------------------------------------------------------

    /// Applies one action (ARCHITECTURE §9.3): edits on the document, transport on the engine,
    /// view changes on the UI state.
    pub fn dispatch(&mut self, action: Action) {
        if self.log.len() == LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(action.clone());
        let mapping_context = self
            .ui
            .track_macros
            .mapping
            .map(|_| (self.track_macros_track(), self.ui.layout.dock_tab));
        match action {
            Action::Transport(a) => self.transport_action(a),
            Action::Edit(a) => self.edit_action(a),
            Action::Automation(a) => self.automation_action(a),
            Action::Macro(a) => self.track_macro_action(a),
            Action::Ui(a) => self.ui_action(a),
            Action::Plugin(a) => self.plugin_action(a),
            Action::Project(a) => self.project_action(a),
            Action::Settings(a) => self.settings_action(a),
        }
        if mapping_context
            .is_some_and(|context| context != (self.track_macros_track(), self.ui.layout.dock_tab))
            || self
                .ui
                .track_macros
                .mapping
                .is_some_and(|track| self.project().track(track).is_none())
        {
            self.ui.track_macros.mapping = None;
        }
        if !self.keyboard_midi_active() {
            self.release_keyboard_notes_if(|_| true);
        }
    }

    fn automation_action(&mut self, action: crate::views::automation::Action) {
        use crate::views::automation::{Action as Automation, Source, lanes};
        use komponist_model::automation::AutomationOwner;
        use komponist_model::edits::automation;
        use komponist_model::routing::group_ancestors;

        match action {
            Automation::Shown(owner, shown) => {
                if self.apply("undo-automation-shown", |p| {
                    automation::set_shown(p, owner, shown)
                }) && shown
                    && let Some(automation) = lanes(self.doc.project(), owner)
                {
                    for lane in &automation.lanes {
                        self.ui.hidden_automation_lanes.remove(&lane.id);
                    }
                }
            }
            Automation::HideLane(id) => {
                self.ui.hidden_automation_lanes.insert(id);
            }
            Automation::ShowTarget {
                owner,
                replace,
                target,
                label,
            } => {
                let old = lanes(self.project(), owner);
                let exists = old.is_some_and(|a| a.lane(&target).is_some());
                let reuse = replace.filter(|id| {
                    if self.automation_capture.references_lane(owner, *id)
                        || self.audio_recording.references_automation_lane(owner, *id)
                    {
                        return false;
                    }
                    old.is_some_and(|a| {
                        a.lanes
                            .iter()
                            .any(|lane| lane.id == *id && lane.points.is_empty())
                    })
                });
                let undo = if exists {
                    "undo-automation-shown"
                } else if reuse.is_some() {
                    "undo-automation-retarget-lane"
                } else {
                    "undo-automation-add-lane"
                };
                let shown_target = target.clone();
                if self.apply(undo, |p| {
                    let edited;
                    let p = if exists {
                        p
                    } else {
                        edited = if let Some(id) = reuse {
                            automation::retarget_lane(p, owner, id, target, label)?
                        } else {
                            automation::add_lane(p, owner, target, label)?.0
                        };
                        &edited
                    };
                    let project = automation::set_shown(p, owner, true)?;
                    if let AutomationOwner::Track(track) = owner {
                        let mut groups = group_ancestors(&project, track);
                        groups.retain(|id| project.track(*id).is_some_and(|group| group.folded));
                        if !groups.is_empty() {
                            return edits::set_track_prop(
                                &project,
                                &groups,
                                TrackProp::Folded(false),
                            );
                        }
                    }
                    Ok(project)
                }) && let Some(lane) =
                    lanes(self.project(), owner).and_then(|a| a.lane(&shown_target))
                {
                    let lane = lane.id;
                    self.ui.hidden_automation_lanes.remove(&lane);
                    self.ui.arrangement.automation_reveal = Some((owner, lane));
                    self.ui.layout.dock_maximized = false;
                }
            }
            Automation::Remove(Source::Lane(owner, id)) => {
                self.apply("undo-automation-remove-lane", |p| {
                    automation::remove_lane(p, owner, id)
                });
            }
            Automation::Remove(Source::Clip(clip, target)) => {
                self.apply("undo-automation-clip-remove", |p| {
                    automation::remove_clip_automation(p, clip, &target)
                });
            }
            Automation::Clear(owner, id) => {
                self.apply("undo-automation-clear-lane", |p| {
                    automation::clear_lane(p, owner, id)
                });
            }
            Automation::Height(owner, id, height) => {
                self.apply("undo-automation-lane-height", |p| {
                    automation::set_lane_height(p, owner, id, height)
                });
            }
            Automation::Mode(owner, id, mode) => {
                self.close_writes_for_mode_change(owner, Some(id), mode);
                self.apply("undo-automation-mode", |p| {
                    automation::set_lane_mode(p, owner, id, mode)
                });
            }
            Automation::AllModes(owner, mode) => {
                self.close_writes_for_mode_change(owner, None, mode);
                self.apply("undo-automation-modes", |p| {
                    automation::set_all_modes(p, owner, mode)
                });
            }
            Automation::Points(source, points, undo) => {
                self.apply(undo, |p| match source {
                    Source::Lane(owner, id) => automation::set_lane_points(p, owner, id, points),
                    Source::Clip(clip, target) => {
                        automation::set_clip_points(p, clip, &target, points)
                    }
                });
            }
            Automation::AddClip(clip, target, label) => {
                self.apply("undo-automation-clip-add", |p| {
                    automation::add_clip_automation(p, clip, target, label)
                });
            }
        }
    }

    fn stop_playback(&mut self) {
        self.drain_events();
        if self.automation_capture.active() {
            self.automation_stop_pending = true;
        } else {
            self.finish_take();
        }
        self.engine.send(Command::Stop);
    }

    fn transport_action(&mut self, action: TransportAction) {
        let playing = self.transport() != TransportState::Stopped;
        let count_in = self.transport() == TransportState::CountIn;
        match action {
            TransportAction::TogglePlay | TransportAction::Stop if count_in => {
                self.cancel_count_in();
            }
            TransportAction::TogglePlay => {
                if playing {
                    self.stop_playback();
                } else if self.audio_ok() {
                    // Play the document as it is now: changes dispatched before Play in the same
                    // frame (a loop range, an edit) reach the engine before Play, not at the end
                    // of the frame. Otherwise playback can start with the old loop and pass the
                    // new loop end before the program arrives, and then it never wraps.
                    self.engine.sync(&self.doc);
                    self.engine.send(Command::Play);
                }
            }
            TransportAction::Stop => {
                if playing {
                    self.stop_playback();
                } else {
                    // Stop while stopped: the hard cut (tails end at once), then back to the
                    // start.
                    let start = self.start_tick();
                    self.engine.send(Command::Stop);
                    self.engine.send(Command::Locate(start));
                }
            }
            TransportAction::ToggleRecord => self.toggle_record(),
            TransportAction::DiscardTake => self.discard_take(),
            TransportAction::ToggleLoop => {
                let on = !self.project().loop_on;
                self.untracked(ProjectProp::LoopOn(on));
            }
            TransportAction::ToggleMetronome => {
                let on = !self.project().metronome;
                self.untracked(ProjectProp::Metronome(on));
            }
            TransportAction::ToggleCountIn => {
                let bars = if self.project().count_in_bars > 0 {
                    0
                } else {
                    1
                };
                self.untracked(ProjectProp::CountInBars(bars));
            }
            TransportAction::GoToStart => {
                let start = self.start_tick();
                self.engine.send(Command::Locate(start));
            }
            TransportAction::Locate(tick) => {
                self.engine.send(Command::Locate(tick.max(Tick::ZERO)));
            }
            TransportAction::SetLoopRange(range) => {
                self.untracked(ProjectProp::LoopRange(range));
                self.untracked(ProjectProp::LoopOn(true));
            }
            TransportAction::LiveNote {
                track,
                key,
                velocity,
            } => {
                if let Some(track) = track.or_else(|| self.live_track()) {
                    self.engine.send(Command::LiveNote {
                        track,
                        key,
                        velocity,
                    });
                }
            }
        }
    }

    /// The loop start with loop on, else tick 0 (Pos1, second Stop).
    fn start_tick(&self) -> Tick {
        let p = self.project();
        if p.loop_on {
            p.loop_range.start
        } else {
            Tick::ZERO
        }
    }

    /// The track live notes go to: the first armed track, else the first selected one (prototype
    /// `liveTrack`).
    fn live_track(&self) -> Option<TrackId> {
        let p = self.project();
        p.tracks
            .iter()
            .find(|t| t.arm)
            .map(|t| t.id)
            .or_else(|| self.ui.selection.tracks.iter().next().copied())
    }

    /// R: records on all armed tracks after the count-in (arming the selected track if none is
    /// armed); while recording, ends the take (punch-out: playback goes on); during the count-in,
    /// cancels the recording and stops (prototype `toggleRec` → `finishRec` with `recPend`). The
    /// take itself is [`Recording`], built from the engine's events.
    fn toggle_record(&mut self) {
        match self.transport() {
            TransportState::CountIn => self.cancel_count_in(),
            TransportState::Recording => self.stop_take(),
            _ if !self.audio_ok() => {}
            state => {
                if !self.project().tracks.iter().any(|t| t.arm) {
                    let Some(&track) = self.ui.selection.tracks.iter().next() else {
                        self.set_status(self.lang.record_no_track(), StatusKind::Error);
                        return;
                    };
                    if !self.audio_arm_allowed(track) {
                        return;
                    }
                    // Arming switches monitoring on (S02 "I").
                    self.apply("undo-arm-track", |p| {
                        edits::set_exclusive(p, track, edits::ExclusiveFlag::Arm)
                    });
                    // The engine must know the armed track before it starts capturing.
                    self.engine.sync(&self.doc);
                }
                let meter = self.project().meter;
                let bar = meter.ticks_per_bar();
                let at = self.engine.playhead().0.max(0);
                let from = if state == TransportState::Stopped {
                    Tick(at.div_euclid(bar) * bar)
                } else {
                    Tick(at)
                };
                // Timing/input edits dispatched in this frame must precede Record too,
                // even when the track was already armed (not only automatic arming).
                self.engine.sync(&self.doc);
                if !self.prepare_audio_take(from) {
                    return;
                }
                self.shell.record_from = Some(from);
                self.shell.take_expected = true;
                self.engine.send(Command::Record { from });
            }
        }
    }

    /// R, Leertaste or Stop during the count-in: no take, the transport stops and the playhead
    /// goes back to where the recording would have started (prototype `finishRec`: `recPend` →
    /// `pos = recAt`).
    fn cancel_count_in(&mut self) {
        // Ignore a count-in completion or capture interruption that arrived since the last poll.
        self.shell.take_expected = false;
        self.recording = None;
        self.audio_recording.cancel();
        self.drain_events();
        self.engine.send(Command::Stop);
        if let Some(from) = self.shell.record_from {
            self.engine.send(Command::Locate(from));
        }
    }

    /// Applies a transport setting outside the history (`Document::apply_untracked`).
    pub(crate) fn untracked(&mut self, prop: ProjectProp) {
        if let Err(e) = self
            .doc
            .apply_untracked(|p| edits::set_project_prop(p, prop.clone()))
        {
            self.record_edit_error(e);
        }
    }

    /// Remembers a rejected edit and tells the user: a message for the errors a user can cause,
    /// the generic "Bearbeitung nicht möglich" (plus a log line) for those only a bug can cause.
    pub fn record_edit_error(&mut self, error: EditError) {
        let t = self.lang;
        let text = match &error {
            EditError::IdsExhausted => t.status_id_space_exhausted().to_owned(),
            EditError::RoutingCycle => t.status_edit_routing_cycle().to_owned(),
            EditError::NotABus(track) => {
                let name = self.project().track(*track).map(|t| t.name.clone());
                t.status_edit_not_a_bus(name.as_deref().unwrap_or_default())
            }
            EditError::AutomationExpansionTooLarge => {
                t.status_edit_automation_too_large().to_owned()
            }
            EditError::MacroMappingLimit { index } => {
                t.status_edit_macro_full(u32::from(*index) + 1)
            }
            EditError::TrackNotFound(_)
            | EditError::ClipNotFound(_)
            | EditError::NoteNotFound(_)
            | EditError::DeviceNotFound(_)
            | EditError::SendNotFound(_)
            | EditError::ClipNotAllowed { .. }
            | EditError::WrongClipKind { .. }
            | EditError::IndexOutOfRange { .. }
            | EditError::Invalid(_) => {
                eprintln!("komponist: edit rejected: {error}");
                t.status_edit_failed().to_owned()
            }
        };
        self.set_status(text, StatusKind::Error);
        self.shell.last_error = Some(error);
    }

    /// Applies `edit` as an undo step `label` (or inside the open gesture); records a failure.
    fn apply(&mut self, label: &str, edit: impl FnOnce(&Project) -> EditResult) -> bool {
        let before = Arc::clone(self.doc.project());
        match self.doc.apply(label, edit) {
            Ok(()) => {
                self.follow_device_states(&before);
                true
            }
            Err(e) => {
                self.record_edit_error(e);
                false
            }
        }
    }

    /// Like [`App::apply`] for edits returning a value.
    pub(crate) fn apply_with<T>(
        &mut self,
        label: &str,
        edit: impl FnOnce(&Project) -> Result<(Project, T), EditError>,
    ) -> Option<T> {
        let before = Arc::clone(self.doc.project());
        match self.doc.apply_with(label, edit) {
            Ok(v) => {
                self.follow_device_states(&before);
                Some(v)
            }
            Err(e) => {
                self.record_edit_error(e);
                None
            }
        }
    }

    /// A delta-carrying drag edit: applied to the gesture's start state while a gesture is open
    /// (actions module docs), else as a normal undo step.
    fn apply_delta<T>(
        &mut self,
        label: &str,
        edit: impl FnOnce(&Project) -> Result<(Project, T), EditError>,
    ) -> Option<T> {
        let before = Arc::clone(self.doc.project());
        let result = if self.doc.in_gesture() {
            self.doc.apply_in_gesture_with(edit)
        } else {
            self.doc.apply_with(label, edit)
        };
        match result {
            Ok(v) => {
                self.follow_device_states(&before);
                Some(v)
            }
            Err(e) => {
                self.record_edit_error(e);
                None
            }
        }
    }

    /// The selected tracks in display order.
    fn selected_track_order(&self) -> Vec<TrackId> {
        self.project()
            .tracks
            .iter()
            .map(|t| t.id)
            .filter(|t| self.ui.selection.tracks.contains(t))
            .collect()
    }

    /// F2: the selected clip (exactly one: the inspector's clip section), else the first selected
    /// track, opens its name field where it is on screen: the arrangement's header, else the
    /// mixer's strip (the dock's mixer tab, or when the mixer has the focus), else the inspector
    /// (opened if hidden). A clip always goes to the inspector, the only place that renames clips.
    fn rename_selection(&mut self) {
        let clip = (self.ui.selection.clips.len() == 1)
            .then(|| self.ui.selection.clips.iter().next().copied())
            .flatten()
            .filter(|c| self.project().find_clip(*c).is_some());
        if let Some(clip) = clip {
            self.ui.layout.inspector_open = true;
            self.ui.inspector.rename_request = Some(InspectorRename::Clip(clip));
            return;
        }
        let Some(track) = self.selected_track_order().first().copied() else {
            return;
        };
        let l = &self.ui.layout;
        let arrangement = !(l.dock_open && l.dock_maximized);
        let mixer = l.dock_open && l.dock_tab == DockTab::Mixer;
        if mixer && (!arrangement || self.ui.focus == FocusArea::Mixer) {
            self.ui.mixer.rename_request = Some(track);
        } else if arrangement {
            self.ui.arrangement.rename_request = Some(track);
        } else {
            self.ui.layout.inspector_open = true;
            self.ui.inspector.rename_request = Some(InspectorRename::Track(track));
        }
    }

    /// Ctrl T and instrument drops: a new MIDI track with `instrument`, armed exclusively,
    /// selected (S10).
    fn new_midi_track(&mut self, index: Option<usize>, instrument: Option<PluginRef>) {
        let spec = NewTrack {
            name: None,
            hue: None,
            instrument,
            index,
            arm_exclusive: true,
        };
        let names = default_names(self.lang);
        if let Some(id) = self.apply_with("undo-new-midi-track", |p| {
            edits::add_track_in(p, spec, &names)
        }) {
            self.select_tracks(vec![id], SelectMode::Replace);
        }
    }

    /// "Anlegen" of the "Neue Spur" dialog: the tracks of `spec` below the last track, one undo
    /// step, all selected, the last one armed. One track is exactly what an instrument drop onto
    /// the "Neue Spur" box makes.
    pub(crate) fn create_tracks(&mut self, spec: &crate::dialogs::new_track::Spec) {
        if spec.bus {
            return self.create_buses(spec);
        }
        if spec.audio {
            return self.create_audio_tracks(spec);
        }
        let tracks = spec.tracks();
        let label = if tracks.len() > 1 {
            "undo-add-tracks"
        } else {
            "undo-new-midi-track"
        };
        let names = default_names(self.lang);
        let made = self.apply_with(label, |p| {
            let mut p = p.clone();
            let mut ids = Vec::with_capacity(tracks.len());
            for spec in tracks {
                let (next, id) = edits::add_track_in(&p, spec, &names)?;
                p = next;
                ids.push(id);
            }
            Ok((p, ids))
        });
        if let Some(ids) = made {
            self.select_tracks(ids, SelectMode::Replace);
        }
    }

    /// "Anlegen" with the type „Bus“: effect buses (no group, no senders, no members) after the
    /// last track, one undo step, the last one selected.
    fn create_buses(&mut self, spec: &crate::dialogs::new_track::Spec) {
        let count = spec.count;
        let names = default_names(self.lang);
        let made = self.apply_with("undo-add-bus", |p| {
            let mut p = p.clone();
            let mut ids = Vec::with_capacity(count);
            for i in 1..=count {
                let name = spec.name.as_ref().map(|n| {
                    if count == 1 {
                        n.clone()
                    } else {
                        format!("{n} {i}")
                    }
                });
                let (next, id) = edits::add_bus_in(
                    &p,
                    edits::NewBus {
                        name,
                        ..edits::NewBus::default()
                    },
                    &names,
                )?;
                p = next;
                ids.push(id);
            }
            Ok((p, ids))
        });
        if let Some(ids) = made {
            self.select_tracks(ids, SelectMode::Replace);
        }
    }

    fn default_hexa(&self) -> Option<PluginRef> {
        let catalog = self.engine.host().catalog();
        catalog
            .packaged(komponist_builtin::BuiltinKind::Hexa)
            .or_else(|| catalog.resolve(&hexa_ref()))
            .map(|entry| entry.plugin.clone())
    }

    /// Opens the "Neue Spur" dialog with the instrument of the last one (hexa the first time).
    fn open_new_track_dialog(&mut self) {
        if self.ui.dialog.is_some() || self.dialogs.overlay.is_some() {
            return;
        }
        let catalog = self.engine.host().catalog();
        let instrument = match &self.dialogs.last_instrument {
            Some(None) => None,
            Some(Some(last)) => catalog
                .resolve(last)
                .filter(|entry| entry.kind == komponist_ipc::PluginKind::Instrument)
                .map(|entry| entry.plugin.clone())
                .or_else(|| self.default_hexa()),
            None => self.default_hexa(),
        };
        let mut state = crate::dialogs::new_track::State::new(instrument);
        state.choose_default_input(self.project(), &self.engine);
        self.open_overlay(crate::dialogs::Overlay::NewTrack(state));
    }

    /// Drops selected objects that no longer exist.
    fn prune_selection(&mut self) {
        let p = Arc::clone(self.project());
        let sel = &mut self.ui.selection;
        sel.tracks.retain(|t| p.track(*t).is_some());
        sel.clips.retain(|c| p.find_clip(*c).is_some());
        sel.devices.retain(|d| p.find_device(*d).is_some());
        if sel.anchor_track.is_some_and(|t| p.track(t).is_none()) {
            sel.anchor_track = None;
        }
        if self
            .ui
            .editor
            .clip
            .is_some_and(|c| p.find_clip(c).is_none())
        {
            self.ui.editor.clip = None;
            self.ui.selection.notes.clear();
            self.ui.selection.step = None;
        }
        self.ui
            .track_macros
            .selected
            .retain(|id, _| p.track(*id).is_some());
        self.ui
            .track_macros
            .collapsed
            .retain(|id| p.track(*id).is_some());
        self.update_time_range();
    }

    fn update_time_range(&mut self) {
        let p = Arc::clone(self.project());
        let spans = self
            .ui
            .selection
            .clips
            .iter()
            .filter_map(|c| p.find_clip(*c))
            .map(|(_, c)| (c.start, c.end()));
        let range = spans.fold(None::<(Tick, Tick)>, |acc, (s, e)| match acc {
            None => Some((s, e)),
            Some((a, b)) => Some((a.min(s), b.max(e))),
        });
        self.ui.selection.time_range = range.map(|(s, e)| TickRange::new(s, e));
    }

    fn select_tracks(&mut self, tracks: Vec<TrackId>, mode: SelectMode) {
        let order: Vec<TrackId> = self.project().tracks.iter().map(|t| t.id).collect();
        let sel = &mut self.ui.selection;
        match mode {
            SelectMode::Replace => {
                sel.tracks = tracks.iter().copied().collect();
                sel.anchor_track = tracks.first().copied();
            }
            SelectMode::Toggle => {
                for t in &tracks {
                    if !sel.tracks.remove(t) {
                        sel.tracks.insert(*t);
                    }
                }
                sel.anchor_track = tracks.first().copied().or(sel.anchor_track);
            }
            SelectMode::Extend => {
                let anchor = sel.anchor_track.or_else(|| tracks.first().copied());
                let ends: Vec<usize> = anchor
                    .into_iter()
                    .chain(tracks.iter().copied())
                    .filter_map(|t| order.iter().position(|o| *o == t))
                    .collect();
                if let (Some(&lo), Some(&hi)) = (ends.iter().min(), ends.iter().max()) {
                    sel.tracks = order[lo..=hi].iter().copied().collect();
                }
                sel.anchor_track = anchor;
            }
            SelectMode::Add => sel.tracks.extend(tracks.iter().copied()),
        }
    }

    fn select_clips(&mut self, clips: Vec<komponist_model::ClipId>, mode: SelectMode) {
        let sel = &mut self.ui.selection;
        match mode {
            SelectMode::Replace => sel.clips = clips.iter().copied().collect(),
            SelectMode::Toggle => {
                for c in &clips {
                    if !sel.clips.remove(c) {
                        sel.clips.insert(*c);
                    }
                }
            }
            SelectMode::Extend | SelectMode::Add => sel.clips.extend(clips.iter().copied()),
        }
        // The inspector shows the track of the selected clip (S01).
        let p = Arc::clone(self.project());
        let tracks: BTreeSet<TrackId> = self
            .ui
            .selection
            .clips
            .iter()
            .filter_map(|c| p.find_clip(*c))
            .map(|(t, _)| t.id)
            .collect();
        if !tracks.is_empty() {
            self.ui.selection.anchor_track = tracks.iter().next().copied();
            self.ui.selection.tracks = tracks;
        }
        self.update_time_range();
    }

    fn ui_action(&mut self, action: UiAction) {
        let layout = &mut self.ui.layout;
        match action {
            UiAction::TogglePanel(panel) => {
                match panel {
                    Panel::Browser => layout.browser_open = !layout.browser_open,
                    Panel::Inspector => layout.inspector_open = !layout.inspector_open,
                    Panel::Dock => {
                        layout.dock_open = !layout.dock_open;
                        if !layout.dock_open {
                            layout.dock_maximized = false;
                            layout.before_mixer = None;
                        }
                    }
                }
                self.refocus_hidden();
            }
            UiAction::ToggleDockMaximized => {
                layout.dock_maximized = !layout.dock_maximized;
                layout.dock_open = true;
                layout.before_mixer = None;
                self.refocus_hidden();
            }
            UiAction::ToggleMixer => {
                if layout.dock_tab == DockTab::Mixer && layout.dock_maximized {
                    let (tab, maximized) = layout.before_mixer.take().unwrap_or_default();
                    layout.dock_tab = tab;
                    layout.dock_maximized = maximized;
                    self.ui.focus = FocusArea::Arrangement;
                    self.refocus_hidden();
                } else {
                    let previous = if layout.dock_tab == DockTab::Mixer {
                        DockTab::Editor
                    } else {
                        layout.dock_tab
                    };
                    layout.before_mixer = Some((previous, layout.dock_maximized));
                    layout.dock_tab = DockTab::Mixer;
                    layout.dock_maximized = true;
                    layout.dock_open = true;
                    self.ui.focus = FocusArea::Mixer;
                }
            }
            UiAction::SetDockTab(tab) => self.set_dock_tab(tab),
            UiAction::ToggleMacroMapping => {
                if let Some(track) = self
                    .ui
                    .track_macros
                    .mapping
                    .or_else(|| self.track_macros_track())
                {
                    self.track_macro_action(crate::views::track_macros::Action::ToggleMapping {
                        track,
                    });
                }
            }
            UiAction::NextTab => self.next_tab(),
            UiAction::Focus(area) => {
                if self.ui.dialog.is_none() && self.dialogs.overlay.is_none() {
                    self.ui.focus = area;
                }
            }
            UiAction::Zoom { vertical, steps } => self.zoom(vertical, steps),
            UiAction::ZoomToFit => self.zoom_to_fit(),
            UiAction::RenameSelection => self.rename_selection(),
            UiAction::GridFiner | UiAction::GridCoarser => {
                let finer = action == UiAction::GridFiner;
                let target = self.focused_grid();
                let grid = self.grid_mut(target);
                *grid = if finer { grid.finer() } else { grid.coarser() };
            }
            UiAction::SetGrid { target, grid } => *self.grid_mut(target) = grid,
            UiAction::ToggleSnap(target) => match target {
                GridTarget::PianoRoll => self.ui.piano_roll.snap = !self.ui.piano_roll.snap,
                GridTarget::Arrangement => self.ui.arrangement.snap = !self.ui.arrangement.snap,
            },
            UiAction::ShowStatus { text, kind } => self.set_status(text, kind),
            UiAction::SetTool(tool) => self.ui.piano_roll.tool = tool,
            UiAction::SetStepLane(lane) => self.ui.step_seq.lane = lane,
            UiAction::Select(change) => self.select(change),
            UiAction::SelectAll => {
                if matches!(self.ui.focus, FocusArea::PianoRoll) {
                    if let Some((id, true)) = self.open_clip_notes()
                        && let Some((_, clip)) = self.project().find_clip(id)
                        && let ClipContent::Midi(midi) = &clip.content
                    {
                        let notes = midi.active().notes.iter().map(|n| n.id).collect();
                        self.ui.selection.notes = notes;
                    }
                } else {
                    let clips = self
                        .project()
                        .tracks
                        .iter()
                        .flat_map(|t| t.clips.iter().map(|c| c.id))
                        .collect();
                    self.select_clips(clips, SelectMode::Replace);
                }
            }
            UiAction::ClearSelection => {
                // Plugin frames are windows of their own: Esc inside one closes it
                // (`windows::plugin_frame`), Esc here leaves them open.
                if self.ui.dialog.is_some() || self.dialogs.overlay.is_some() {
                    self.ui_action(UiAction::CloseDialog);
                } else {
                    let sel = &mut self.ui.selection;
                    sel.notes.clear();
                    sel.clips.clear();
                    sel.step = None;
                    sel.time_range = None;
                }
            }
            UiAction::OpenClip(clip) => {
                let Some((_, c)) = self.project().find_clip(clip) else {
                    return;
                };
                let empty_midi =
                    matches!(&c.content, ClipContent::Midi(m) if m.active().notes.is_empty());
                if matches!(&c.content, ClipContent::Audio(_)) {
                    self.ui.waveform.fit_clip = true;
                }
                if self.ui.editor.clip != Some(clip) {
                    self.ui.selection.notes.clear();
                    self.ui.selection.step = None;
                }
                self.ui.editor.clip = Some(clip);
                if empty_midi {
                    self.ui.piano_roll.tool = Tool::Pencil;
                }
                self.select_clips(vec![clip], SelectMode::Replace);
                self.set_dock_tab(DockTab::Editor);
                self.ui.focus = self.ui.editor_area(self.doc.project());
            }
            UiAction::FocusBrowserSearch => {
                self.ui.layout.browser_open = true;
                self.ui.focus = FocusArea::Browser;
                self.shell.requests.focus_browser_search = true;
            }
            UiAction::SetBrowserSegment(segment) => self.ui.browser.segment = segment,
            UiAction::SetBrowserQuery(query) => {
                self.ui.browser.query = query;
                self.ui.browser.highlighted = None;
            }
            UiAction::OpenAbout => self.open_dialog(Dialog::About),
            UiAction::OpenSettings(section) => self.open_dialog(Dialog::Settings(section)),
            UiAction::OpenNewTrackDialog => self.open_new_track_dialog(),
            UiAction::SetPluginView { device, view } => self.set_plugin_view(device, view),
            UiAction::SetPluginKeepOnTop { device, on } => self.set_keep_on_top(device, on),
            UiAction::CloseDialog => {
                // Esc on the recovery prompt is „Später“: nothing is lost by closing it.
                if matches!(
                    self.dialogs.overlay,
                    Some(crate::dialogs::Overlay::Recovery)
                ) {
                    self.answer_recovery(crate::dialogs::recovery::Answer::Later);
                    return;
                }
                // The file browser or the unsaved-changes prompt closes first.
                if self.close_overlay() {
                    return;
                }
                // Esc during an export cancels it; the dialog closes when the engine is back.
                if self.export.is_active() {
                    self.cancel_export();
                    return;
                }
                self.close_modal_dialog();
            }
            UiAction::ToggleKeyboardMidi => {
                self.ui.keyboard_midi = !self.ui.keyboard_midi;
                if !self.ui.keyboard_midi {
                    self.release_keyboard_notes_if(|_| true);
                }
            }
            UiAction::ResetMeters { track, all } => self.meters.reset(track, all),
            UiAction::ResizePanel { panel, size } => {
                let layout = &mut self.ui.layout;
                match panel {
                    Panel::Browser => {
                        layout.browser_width = crate::layout::clamp_side(panel, size);
                    }
                    Panel::Inspector => {
                        layout.inspector_width = crate::layout::clamp_side(panel, size);
                    }
                    Panel::Dock => {
                        layout.dock_height = size.max(komponist_tokens::size::dock::MIN);
                    }
                }
            }
            UiAction::ResetPanelSize(panel) => {
                self.ui_action(UiAction::ResizePanel {
                    panel,
                    size: crate::layout::default_size(panel),
                });
            }
            UiAction::ToggleDeviceCollapsed { device, all } => {
                self.toggle_device_collapsed(device, all);
            }
            UiAction::SetDevicePage { device, page } => {
                self.ui.devices.pages.insert(device, page);
            }
            UiAction::CopyDevices => self.copy_devices(),
            UiAction::SelectAdjacentDevice(dir) => self.select_adjacent_device(dir),
            UiAction::RevealDevice(device) => self.reveal_device(device),
            UiAction::ScrollToDevice(device) => {
                self.ui.devices.reveal = Some((device, crate::ui_state::Reveal::Start));
            }
            UiAction::TouchParam {
                device,
                param,
                dragging,
            } => self.touch_param(device, param, dragging),
        }
    }

    /// SetDockTab: opens the dock; leaving the mixer that Tab maximised restores the layout
    /// (prototype `setDockTab`).
    fn set_dock_tab(&mut self, tab: DockTab) {
        let layout = &mut self.ui.layout;
        layout.dock_open = true;
        if layout.dock_tab != tab {
            self.ui.track_macros.mapping = None;
        }
        layout.dock_tab = tab;
        if tab != DockTab::Mixer
            && layout.dock_maximized
            && let Some((_, maximized)) = layout.before_mixer.take()
        {
            layout.dock_maximized = maximized;
        }
        if self.ui.focus.in_dock() {
            self.ui.focus = self.dock_area();
        }
    }

    /// Ctrl Tab: the next tab of the focused area (dock tabs, browser segments).
    fn next_tab(&mut self) {
        use crate::ui_state::BrowserSegment as B;
        match self.ui.focus {
            area if area.in_dock() => {
                let next = match self.ui.layout.dock_tab {
                    DockTab::Editor => DockTab::Devices,
                    DockTab::Devices => DockTab::Mixer,
                    DockTab::Mixer => DockTab::Editor,
                };
                self.set_dock_tab(next);
                self.ui.focus = self.dock_area();
            }
            FocusArea::Browser => {
                let b = &mut self.ui.browser;
                b.segment = match b.segment {
                    B::All => B::Instruments,
                    B::Instruments => B::Effects,
                    B::Effects => B::Files,
                    B::Files => B::All,
                };
            }
            _ => {}
        }
    }

    /// A focused panel that got hidden passes the focus to the arrangement, or to the dock when
    /// the arrangement is hidden.
    fn refocus_hidden(&mut self) {
        let l = &self.ui.layout;
        let hidden = match self.ui.focus {
            FocusArea::Browser => !l.browser_open,
            FocusArea::Inspector => !l.inspector_open,
            FocusArea::Arrangement => l.dock_open && l.dock_maximized,
            area if area.in_dock() => !l.dock_open,
            _ => false,
        };
        if hidden {
            self.ui.focus = if l.dock_open && l.dock_maximized {
                self.dock_area()
            } else {
                FocusArea::Arrangement
            };
        }
    }

    /// The focus area of the dock's active tab, including the selected live MIDI or audio take.
    pub(crate) fn dock_area(&self) -> FocusArea {
        if self.ui.layout.dock_tab == DockTab::Editor
            && let Some(area) = self.live_editor_area()
        {
            area
        } else {
            self.ui.dock_area(self.doc.project())
        }
    }

    /// Removes the modal it owns, preserving any file or unsaved-changes overlay above it.
    pub(crate) fn close_modal_dialog(&mut self) {
        // Esc reaches both the keymap and the modal; the second close changes nothing.
        if self.ui.dialog.take().is_none() {
            return;
        }
        let focus = self
            .shell
            .focus_before_dialog
            .take()
            .unwrap_or(FocusArea::Arrangement);
        if self.dialogs.overlay.is_some() {
            if self.dialogs.overlay_focus == Some(FocusArea::Dialog) {
                self.dialogs.overlay_focus = Some(focus);
            }
        } else {
            self.ui.focus = focus;
            self.open_queued_dialog();
        }
    }

    pub(crate) fn open_queued_dialog(&mut self) {
        if self.ui.dialog.is_none()
            && let Some(next) = self.dialogs.queued.take()
        {
            self.open_dialog(next);
        }
    }

    pub(crate) fn open_dialog(&mut self, dialog: Dialog) {
        // The export dialog shows the progress and Esc cancels: another dialog (S06 after a late
        // scan, Strg , settings) waits until it closes.
        if self.export.is_active()
            && matches!(self.ui.dialog, Some(Dialog::Export(_) | Dialog::Flatten))
        {
            if !matches!(dialog, Dialog::Export(_) | Dialog::Flatten) {
                self.dialogs.queued = Some(dialog);
            }
            return;
        }
        if self.ui.dialog.is_none() {
            self.shell.focus_before_dialog = Some(self.ui.focus);
        }
        self.ui.dialog = Some(dialog);
        self.ui.focus = FocusArea::Dialog;
    }

    fn piano_roll_focused(&self) -> bool {
        self.ui.focus == FocusArea::PianoRoll
    }

    /// The grid the keyboard (Ctrl [ / Ctrl ]) changes: the piano roll's while it has the focus.
    fn focused_grid(&self) -> GridTarget {
        if self.piano_roll_focused() {
            GridTarget::PianoRoll
        } else {
            GridTarget::Arrangement
        }
    }

    fn grid_mut(&mut self, target: GridTarget) -> &mut komponist_model::Grid {
        match target {
            GridTarget::PianoRoll => &mut self.ui.piano_roll.grid,
            GridTarget::Arrangement => &mut self.ui.arrangement.grid,
        }
    }

    fn zoom(&mut self, vertical: bool, steps: i32) {
        use metrics::arrangement::{PX_PER_BAR_MAX, PX_PER_BAR_MIN, V_ZOOM, ZOOM_STEP};
        let factor = ZOOM_STEP.powi(steps);
        match self.ui.focus {
            FocusArea::PianoRoll => {
                if vertical {
                    // Whole pixels, so the rows and the key labels stay sharp.
                    let pr = &mut self.ui.piano_roll;
                    for _ in 0..steps.unsigned_abs() {
                        let h = pr.key_height.round();
                        let scaled = (h * ZOOM_STEP.powi(steps.signum())).round();
                        let next = if scaled == h {
                            h + steps.signum() as f32
                        } else {
                            scaled
                        };
                        pr.key_height = next.clamp(
                            komponist_tokens::size::key::MIN,
                            komponist_tokens::size::key::MAX,
                        );
                    }
                } else {
                    self.shell.requests.piano_roll_zoom += steps;
                }
            }
            FocusArea::Waveform if !vertical => {
                let w = &mut self.ui.waveform;
                w.px_per_bar = (w.px_per_bar * factor).clamp(
                    crate::views::audio_editor::metrics::PPB_MIN,
                    crate::views::audio_editor::metrics::PPB_MAX,
                );
            }
            FocusArea::Arrangement => {
                let a = &mut self.ui.arrangement;
                if vertical {
                    a.v_zoom = (a.v_zoom * factor).clamp(V_ZOOM[0], V_ZOOM[1]);
                } else {
                    a.px_per_bar = (a.px_per_bar * factor).clamp(PX_PER_BAR_MIN, PX_PER_BAR_MAX);
                }
            }
            _ => {}
        }
    }

    /// Z: the selection's time range (or the whole song) fills the arrangement's timeline.
    fn zoom_to_fit(&mut self) {
        use metrics::arrangement::{PX_PER_BAR_MAX, PX_PER_BAR_MIN};
        if self.ui.focus == FocusArea::PianoRoll {
            // The piano roll knows its grid width: it fits the clip in the next frame.
            self.shell.requests.fit_piano_roll = true;
            return;
        }
        if self.ui.focus != FocusArea::Arrangement {
            return;
        }
        let p = self.project();
        let bar = p.meter.ticks_per_bar() as f64;
        let range = self
            .ui
            .selection
            .time_range
            .unwrap_or_else(|| TickRange::new(Tick::ZERO, p.song_end()));
        let bars = ((range.end.0 - range.start.0) as f64 / bar).max(1.0);
        let width = self.shell.timeline_width;
        if width <= 0.0 {
            return;
        }
        let a = &mut self.ui.arrangement;
        a.px_per_bar = (width / bars as f32).clamp(PX_PER_BAR_MIN, PX_PER_BAR_MAX);
        a.scroll_tick = range.start.0 as f64;
    }

    fn select(&mut self, change: SelectionChange) {
        match change {
            SelectionChange::Tracks { tracks, mode } => self.select_tracks(tracks, mode),
            SelectionChange::Clips { clips, mode } => self.select_clips(clips, mode),
            SelectionChange::Notes { notes, mode } => {
                let sel = &mut self.ui.selection.notes;
                match mode {
                    SelectMode::Replace => *sel = notes.into_iter().collect(),
                    SelectMode::Toggle => {
                        for n in notes {
                            if !sel.remove(&n) {
                                sel.insert(n);
                            }
                        }
                    }
                    SelectMode::Extend | SelectMode::Add => sel.extend(notes),
                }
            }
            SelectionChange::Step { clip, row, step } => {
                self.ui.selection.step = Some(crate::ui_state::StepCursor { clip, row, step });
            }
            SelectionChange::Devices { devices, mode } => {
                let sel = &mut self.ui.selection.devices;
                match mode {
                    SelectMode::Replace => *sel = devices.into_iter().collect(),
                    SelectMode::Toggle => {
                        for d in devices {
                            if !sel.remove(&d) {
                                sel.insert(d);
                            }
                        }
                    }
                    SelectMode::Extend | SelectMode::Add => sel.extend(devices),
                }
            }
        }
    }

    pub(crate) fn plugin_action(&mut self, action: PluginAction) {
        match action {
            PluginAction::Add { plugin, target } => self.add_plugin(plugin, target),
            PluginAction::RequestWaveformMarkers { device, slot } => {
                self.engine
                    .host_mut()
                    .request_waveform_markers(device, slot);
            }
            PluginAction::EditWaveformMarkers {
                device,
                slot,
                request,
            } => {
                if self
                    .engine
                    .host_mut()
                    .edit_waveform_markers(device, slot, request)
                    .is_err()
                {
                    self.set_status(self.lang.devices_marker_edit_failed(), StatusKind::Error);
                }
            }
            PluginAction::ApplyClapPreset { device, source } => {
                if let Some((_, _, target)) = self.project().find_device(device) {
                    if let Some(state) = komponist_plugins::presets::legacy_state(
                        &target.plugin,
                        target.state.as_deref(),
                        &source,
                    ) {
                        self.apply("undo-load-preset", |p| {
                            edits::set_device_state(p, device, Some(state.into()))
                        });
                        self.frame_preset_finished(device, true);
                    } else {
                        let target = Arc::clone(target);
                        if !self.engine.host_mut().load_clap_preset(&target, &source) {
                            self.frame_preset_finished(device, false);
                            self.set_status(
                                self.lang.devices_preset_load_failed(),
                                StatusKind::Error,
                            );
                        }
                    }
                }
            }
            PluginAction::SaveUserPreset(device) => self.open_user_preset_dialog(device, true),
            PluginAction::LoadUserPreset(device) => self.open_user_preset_dialog(device, false),
            PluginAction::AddFromPreset {
                plugin,
                source,
                track,
            } => {
                self.add_from_preset(plugin, source, track);
            }
            PluginAction::OpenWindow(device) => {
                if !self.ui.plugin_windows.iter().any(|w| w.device == device) {
                    // The plugin's own GUI if it has one (the frame opens it, WP-50), else the
                    // generic parameter view.
                    let view = if self.engine.host().has_gui(device) {
                        PluginView::Gui
                    } else {
                        PluginView::Params
                    };
                    let memory = self
                        .project()
                        .find_device(device)
                        .and_then(|(_, _, d)| self.settings.frame_memory(&d.plugin))
                        .cloned();
                    self.ui
                        .plugin_windows
                        .push(PluginWindow::new(device, view, memory.as_ref()));
                } else {
                    self.raise_plugin_window(device);
                }
            }
            // Closes the plugin's GUI if it is open; a state snapshot follows (`StateSnapshot`).
            PluginAction::CloseWindow(device) => self.close_plugin_window(device),
            PluginAction::FocusWindow(device) => self.focus_plugin_window(device),
            PluginAction::Reload(device) => {
                self.cancel_frame_preset_requests(device);
                self.reset_sampler_hit(device);
                self.engine.reload_device(self.doc.project(), device);
            }
            PluginAction::PickPluginAsset { device, slot } => self.pick_plugin_asset(device, slot),
            PluginAction::SetPluginAsset { device, slot, path } => {
                self.set_plugin_asset(device, slot, path.as_deref());
            }
            PluginAction::DropPluginAssets { device, files } => {
                self.drop_plugin_assets(device, &files);
            }
            PluginAction::LoadSample { .. }
            | PluginAction::ClearSample { .. }
            | PluginAction::PickSample { .. }
            | PluginAction::AuditionNote { .. }
            | PluginAction::PreviewWaveform { .. }
            | PluginAction::SetSamplerSlices { .. } => {
                self.sampler_action(&action);
            }
            PluginAction::BeginParamGesture { device, id } => {
                let intercept =
                    self.device_automation_target(device, id)
                        .is_some_and(|(owner, target)| {
                            self.macro_target_captures_gesture(owner, &target)
                        });
                if !intercept {
                    self.begin_param_gesture(device, id);
                }
            }
            PluginAction::SetParam { device, id, value } => {
                let handled = self
                    .device_automation_target(device, id)
                    .is_some_and(|(owner, target)| self.handle_macro_target_edit(owner, &target));
                if !handled {
                    self.set_param(device, id, value);
                }
            }
        }
    }

    /// Browser double-click, Enter or drop. The role comes from the device host (built-ins are
    /// instruments; the plugin manager knows the rest).
    fn add_plugin(&mut self, plugin: PluginRef, target: DropTarget) {
        let Some(role) = self.engine.host().role(&plugin) else {
            return;
        };
        let selected = self.ui.selection.tracks.iter().next().copied();
        match (role, target) {
            (DeviceRole::Instrument, DropTarget::NewTrack { index }) => {
                self.new_midi_track(index, Some(plugin));
            }
            (DeviceRole::Instrument, DropTarget::Track(track))
            | (
                DeviceRole::Instrument,
                DropTarget::Chain {
                    channel: ChannelRef::Track(track),
                    ..
                },
            ) => {
                self.apply_with("undo-replace-instrument", |p| {
                    edits::replace_instrument(p, track, plugin)
                });
            }
            (DeviceRole::Instrument, DropTarget::Chain { .. }) => {}
            (DeviceRole::MidiEffect, target) => {
                // Prototype `dropNew` again: MIDI effects belong to a track's chain, in front of
                // the instrument (`insert_device` clamps the index into the MIDI part).
                let (channel, index) = match target {
                    DropTarget::NewTrack { .. } => match selected {
                        Some(t) => (ChannelRef::Track(t), usize::MAX),
                        None => {
                            self.set_status(
                                self.lang.status_midi_fx_need_track(),
                                StatusKind::Error,
                            );
                            return;
                        }
                    },
                    DropTarget::Track(t) => (ChannelRef::Track(t), usize::MAX),
                    DropTarget::Chain { channel, index } => (channel, index),
                };
                if channel == ChannelRef::Master {
                    self.set_status(self.lang.status_midi_fx_need_track(), StatusKind::Error);
                    return;
                }
                let spec = NewDevice {
                    role: DeviceRole::MidiEffect,
                    plugin,
                    state: None,
                    sidechain: None,
                };
                self.apply_with("undo-add-midi-effect", |p| {
                    edits::insert_device(p, channel, index, spec)
                });
            }
            (DeviceRole::Effect, target) => {
                let (channel, index) = match target {
                    DropTarget::NewTrack { .. } => match selected {
                        Some(t) => (ChannelRef::Track(t), None),
                        None => {
                            // Prototype `dropNew`.
                            self.set_status(self.lang.status_fx_need_track(), StatusKind::Error);
                            return;
                        }
                    },
                    DropTarget::Track(t) => (ChannelRef::Track(t), None),
                    DropTarget::Chain { channel, index } => (channel, Some(index)),
                };
                let p = self.project();
                let len = match channel {
                    ChannelRef::Track(t) => p.track(t).map_or(0, |t| t.channel.devices.len()),
                    ChannelRef::Master => p.master.devices.len(),
                };
                let index = index.unwrap_or(len).min(len);
                let spec = NewDevice {
                    role: DeviceRole::Effect,
                    plugin,
                    state: None,
                    sidechain: None,
                };
                self.apply_with("undo-add-effect", |p| {
                    edits::insert_device(p, channel, index, spec)
                });
            }
        }
    }

    fn project_action(&mut self, action: ProjectAction) {
        match action {
            // The project lifecycle (WP-52): `project_io`.
            ProjectAction::New
            | ProjectAction::Open
            | ProjectAction::OpenPath(_)
            | ProjectAction::ReplaceAudio(_)
            | ProjectAction::ReplaceAudioPath { .. }
            | ProjectAction::Save
            | ProjectAction::SaveAs
            | ProjectAction::CollectAndSave
            | ProjectAction::ShowStartScreen
            | ProjectAction::Quit
            | ProjectAction::ContinueWithMissing
            | ProjectAction::CheckPluginFolders => self.lifecycle_action(action),
            // The export (WP-53b): `crate::export`.
            ProjectAction::ImportAudio => self.open_audio_import(),
            ProjectAction::ImportAudioPaths { paths, target, at } => {
                self.import_audio_paths(&paths, target, at)
            }
            ProjectAction::OpenExportDialog => self.open_export_dialog(),
            ProjectAction::StartExport(export) => self.start_export(export),
            ProjectAction::CancelExport => self.cancel_export(),
            ProjectAction::ShowInFolder(folder) => self.show_in_folder(&folder),
        }
    }

    /// Makes `doc` the open document: transport stopped at 0, the engine reset (device ids restart
    /// at 1 in every project, [`EngineSync::reset`]), fresh selection, editor, plugin windows,
    /// meters and scroll position.
    pub(crate) fn open_document(&mut self, doc: Document, path: Option<PathBuf>) {
        self.stop_waveform_preview();
        self.release_keyboard_notes_if(|_| true);
        // Plugin GUIs close in their bridges before the reset releases the bridges.
        self.close_all_plugin_frames();
        // A take of the old project does not go into the new one, nor does a count-in that ends
        // while the project opens.
        self.recording = None;
        self.audio_recording.reset();
        self.discard_automation_capture();
        self.shell.take_expected = false;
        self.engine.send(Command::Stop);
        self.engine.send(Command::Locate(Tick::ZERO));
        self.engine.reset();
        // The old document's recovery copy goes; a recovered copy is adopted afterwards.
        self.recovery.document_replaced(None);
        self.recovery_ui.save_asks = false;
        self.doc = doc;
        self.dialogs.repaired = 0;
        self.param_undo.forget();
        self.preset_preparations.clear();
        self.user_presets = Default::default();
        self.dialogs.samples.forget_device_state();
        self.project_path = path;
        // Relative sample paths resolve against the project file's folder, before the devices of
        // the project get their processors.
        let dir = self.project_dir();
        self.engine.host_mut().set_project_dir(dir);
        self.meters = Meters::new();
        self.export.forget();
        self.ui.selection = Default::default();
        self.ui.track_macros = Default::default();
        self.ui.editor = Default::default();
        self.views.arrangement.reset_document();
        self.views.piano_roll.reset_document();
        self.views.step_seq.reset_document();
        self.ui.plugin_windows.clear();
        self.ui.arrangement.scroll_tick = 0.0;
        self.ui.arrangement.scroll_y = 0.0;
        self.ui.arrangement.automation_reveal = None;
        self.ui.hidden_automation_lanes.clear();
        self.ui.automation_hint = false;
        self.ui.screen = Screen::Project;
        self.ui.dialog = None;
        self.shell.focus_before_dialog = None;
        self.ui.focus = FocusArea::Arrangement;
    }

    fn persist_audio_input(&mut self, successful_choice: bool) {
        let requested = self.engine.audio_input_config();
        // A previous write may have failed after the in-memory preference changed.
        if successful_choice || self.settings.audio_input.as_ref() != requested {
            self.settings.audio_input = requested.cloned();
            self.save_settings();
        }
    }

    fn choose_track_audio_input(
        &mut self,
        track: TrackId,
        input: AudioInput,
        device: InputConfig,
        mut set_input: impl FnMut(&mut EngineSync, Option<InputConfig>) -> Result<(), AudioError>,
    ) {
        use crate::views::audio_inputs::input_label;

        let previous = self.engine.audio_input_config().cloned();
        let problem = match set_input(&mut self.engine, Some(device)) {
            Ok(())
                if matches!(input, AudioInput::Hardware(hardware)
                    if !self.engine.hardware_input_available(hardware)) =>
            {
                let label = input_label(self.lang, self.project(), input);
                Some(self.lang.audio_input_unavailable(label))
            }
            Ok(()) => {
                self.persist_audio_input(true);
                self.dispatch(Action::Edit(EditAction::SetTrackProp {
                    tracks: vec![track],
                    prop: TrackProp::AudioInput(input),
                }));
                return;
            }
            Err(error)
                if self.engine.audio_input_error().is_some()
                    || !matches!(self.engine.audio_state(), AudioState::NoDevice { .. }) =>
            {
                Some(self.lang.audio_input_open_failed(error))
            }
            Err(_) => None,
        };

        if self.engine.audio_input_config() != previous.as_ref() {
            let _ = set_input(&mut self.engine, previous);
        }
        if let Some(problem) = problem {
            self.set_status(problem, StatusKind::Error);
        }
    }

    fn settings_action(&mut self, action: SettingsAction) {
        let changes_driver = matches!(&action, SettingsAction::SetAudio(config)
            if config.host != self.settings.audio.host);
        if changes_driver
            && (self.shell.take_expected
                || self.audio_recording().is_some()
                || self.recording.is_some())
        {
            self.set_status(
                self.lang.settings_audio_driver_change_recording(),
                StatusKind::Error,
            );
            return;
        }
        let changes_input = matches!(
            &action,
            SettingsAction::SetAudioInput(_)
                | SettingsAction::SetJackCaptureAutomatic
                | SettingsAction::ChooseTrackAudioInput { .. }
        ) || matches!(&action, SettingsAction::SetAudio(config)
            if config.jack_routes.capture != self.settings.audio.jack_routes.capture);
        let changes_jack_output = matches!(&action, SettingsAction::SetAudio(config)
            if config.jack_routes.playback != self.settings.audio.jack_routes.playback
                || (config.host == komponist_audio::HostKind::Jack
                    && (config.device != self.settings.audio.device
                        || config.device_id != self.settings.audio.device_id)));
        if self.audio_recording().is_some() {
            let message = if changes_input {
                Some(self.lang.audio_input_change_recording())
            } else if changes_jack_output {
                Some(self.lang.settings_jack_playback_change_recording())
            } else {
                None
            };
            if let Some(message) = message {
                self.set_status(message, StatusKind::Error);
                return;
            }
        }
        match action {
            SettingsAction::SetAudio(config) => {
                let _ = self.engine.set_audio(&config);
                self.settings.audio = config;
                self.save_settings();
            }
            SettingsAction::SetJackCaptureAutomatic => {
                let input = Some(komponist_audio::InputConfig::default());
                if let Err(error) = self.engine.set_audio_input(input.clone()) {
                    self.set_status(self.lang.audio_input_open_failed(error), StatusKind::Error);
                    return;
                }
                self.settings.audio_input = input;
                self.settings.audio.jack_routes.capture = None;
                let _ = self.engine.set_audio(&self.settings.audio);
                self.save_settings();
            }
            SettingsAction::SetAudioInput(input) => {
                let result = self.engine.set_audio_input(input);
                self.persist_audio_input(result.is_ok());
                if let Err(error) = result
                    && self.engine.audio_input_error().is_none()
                    && !matches!(self.engine.audio_state(), AudioState::NoDevice { .. })
                {
                    self.set_status(self.lang.audio_input_open_failed(error), StatusKind::Error);
                }
            }
            SettingsAction::ChooseTrackAudioInput {
                track,
                input,
                device,
            } => self.choose_track_audio_input(track, input, device, EngineSync::set_audio_input),
            SettingsAction::SetCaptureFormat(format) => {
                self.settings.capture_format = format;
                self.save_settings();
            }
            SettingsAction::RetryAudio => {
                // Only a stream that is not running is restarted: reopening a running one would
                // interrupt playback, and the retry button also shows for a failed device list.
                if !matches!(self.engine.audio_state(), AudioState::Running { .. }) {
                    let config = self.settings.audio.clone();
                    let _ = self.engine.set_audio(&config);
                }
                // The device list may have changed too.
                self.dialogs.settings.refresh_devices();
                self.dialogs.settings.invalidate_jack_ports();
            }
            SettingsAction::SetMidiInput { name, enabled } => {
                if let Some(hub) = self.engine.midi_mut()
                    && let Err(error) = hub.set_enabled(&name, enabled)
                {
                    eprintln!("komponist: MIDI input {name}: {error}");
                    self.set_status(
                        self.lang.status_midi_enable_failed(&name),
                        StatusKind::Error,
                    );
                }
                self.settings.midi_disabled.retain(|n| *n != name);
                self.settings.midi_enabled.retain(|n| *n != name);
                if enabled {
                    self.settings.midi_enabled.push(name);
                } else {
                    self.settings.midi_disabled.push(name);
                }
                self.save_settings();
            }
            SettingsAction::AddPluginDir { clap, path } => {
                let dirs = self.plugin_dirs(clap);
                if !dirs.contains(&path) {
                    dirs.push(path);
                }
                self.save_settings();
                self.scan_plugins();
            }
            SettingsAction::RemovePluginDir { clap, path } => {
                self.plugin_dirs(clap).retain(|p| *p != path);
                self.save_settings();
                self.scan_plugins();
            }
            SettingsAction::RescanPlugins => self.scan_plugins(),
            SettingsAction::SetUiScale(scale) => {
                self.settings.ui_scale = scale;
                self.shell.pending_scale = Some(scale);
                self.save_settings();
            }
            SettingsAction::SetLanguage(language) => {
                self.settings.language = language;
                let lang = language.resolve(sys_locale::get_locale().as_deref());
                if lang != self.lang {
                    self.lang = lang;
                    // Status messages are formatted text of the old language.
                    self.ui.status = None;
                }
                self.save_settings();
            }
            SettingsAction::ToggleFavorite(plugin) => {
                self.settings.favorites.toggle(&plugin);
                self.ui.browser.favorites = self.settings.favorites.clone();
                self.save_settings();
            }
            SettingsAction::ToggleMasterDisplay => {
                self.settings.master_display = self.settings.master_display.toggled();
                self.save_settings();
            }
        }
    }

    fn plugin_dirs(&mut self, clap: bool) -> &mut Vec<PathBuf> {
        if clap {
            &mut self.settings.clap_dirs
        } else {
            &mut self.settings.vst3_dirs
        }
    }
}

/// The window title for a project: "komponist · Night Drive" (ARCHITECTURE §13.6).
pub fn window_title(project_name: &str) -> String {
    format!("{} · {project_name}", crate::panels::transport::WORDMARK)
}

/// Whether `action` closes a dialog (the only keys an overlay leaves to the keymap).
fn closes_dialog(action: &Action) -> bool {
    matches!(
        action,
        Action::Ui(UiAction::CloseDialog | UiAction::ClearSelection)
    )
}

/// Whether a shortcut works on the start screen (S09): new, open, settings, Esc in dialogs.
fn on_start_screen(action: &Action) -> bool {
    closes_dialog(action)
        || matches!(
            action,
            Action::Project(ProjectAction::New | ProjectAction::Open | ProjectAction::Quit)
                | Action::Ui(UiAction::OpenSettings(_) | UiAction::OpenAbout)
        )
}

/// Whether a held key may repeat `action` (zoom, transpose, nudge).
pub(crate) fn repeatable(action: &Action) -> bool {
    matches!(
        action,
        Action::Ui(UiAction::Zoom { .. })
            | Action::Edit(EditAction::TransposeSelection(_) | EditAction::NudgeSelection(_))
    )
}

/// The reference of the built-in hexa (Ctrl T).
pub fn hexa_ref() -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: BUILTIN_HEXA.to_owned(),
        name: "Hexa".to_owned(),
        vendor: "klang-studio".to_owned(),
        module: Box::default(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// The keymap's key for an egui key (`=` and `+` are both [`Key::Plus`]).
pub fn key_from_egui(key: egui::Key) -> Option<Key> {
    use egui::Key as E;
    Some(match key {
        E::A => Key::A,
        E::B => Key::B,
        E::C => Key::C,
        E::D => Key::D,
        E::E => Key::E,
        E::F => Key::F,
        E::G => Key::G,
        E::H => Key::H,
        E::I => Key::I,
        E::J => Key::J,
        E::K => Key::K,
        E::L => Key::L,
        E::M => Key::M,
        E::N => Key::N,
        E::O => Key::O,
        E::P => Key::P,
        E::Q => Key::Q,
        E::R => Key::R,
        E::S => Key::S,
        E::T => Key::T,
        E::U => Key::U,
        E::V => Key::V,
        E::W => Key::W,
        E::X => Key::X,
        E::Y => Key::Y,
        E::Z => Key::Z,
        E::Num0 => Key::Num0,
        E::Num1 => Key::Num1,
        E::Num2 => Key::Num2,
        E::Num3 => Key::Num3,
        E::Num4 => Key::Num4,
        E::Num5 => Key::Num5,
        E::Num6 => Key::Num6,
        E::Num7 => Key::Num7,
        E::Num8 => Key::Num8,
        E::Num9 => Key::Num9,
        E::Space => Key::Space,
        E::Tab => Key::Tab,
        E::Enter => Key::Enter,
        E::Escape => Key::Escape,
        E::Delete => Key::Delete,
        E::Backspace => Key::Backspace,
        E::Home => Key::Home,
        E::End => Key::End,
        E::ArrowUp => Key::ArrowUp,
        E::ArrowDown => Key::ArrowDown,
        E::ArrowLeft => Key::ArrowLeft,
        E::ArrowRight => Key::ArrowRight,
        E::Plus | E::Equals => Key::Plus,
        E::Minus => Key::Minus,
        E::OpenBracket => Key::OpenBracket,
        E::CloseBracket => Key::CloseBracket,
        E::Comma => Key::Comma,
        E::F2 => Key::F2,
        _ => return None,
    })
}

/// The egui key for a keymap key (tests press chords through `egui_kittest`).
pub fn key_to_egui(key: Key) -> egui::Key {
    use egui::Key as E;
    match key {
        Key::A => E::A,
        Key::B => E::B,
        Key::C => E::C,
        Key::D => E::D,
        Key::E => E::E,
        Key::F => E::F,
        Key::G => E::G,
        Key::H => E::H,
        Key::I => E::I,
        Key::J => E::J,
        Key::K => E::K,
        Key::L => E::L,
        Key::M => E::M,
        Key::N => E::N,
        Key::O => E::O,
        Key::P => E::P,
        Key::Q => E::Q,
        Key::R => E::R,
        Key::S => E::S,
        Key::T => E::T,
        Key::U => E::U,
        Key::V => E::V,
        Key::W => E::W,
        Key::X => E::X,
        Key::Y => E::Y,
        Key::Z => E::Z,
        Key::Num0 => E::Num0,
        Key::Num1 => E::Num1,
        Key::Num2 => E::Num2,
        Key::Num3 => E::Num3,
        Key::Num4 => E::Num4,
        Key::Num5 => E::Num5,
        Key::Num6 => E::Num6,
        Key::Num7 => E::Num7,
        Key::Num8 => E::Num8,
        Key::Num9 => E::Num9,
        Key::Space => E::Space,
        Key::Tab => E::Tab,
        Key::Enter => E::Enter,
        Key::Escape => E::Escape,
        Key::Delete => E::Delete,
        Key::Backspace => E::Backspace,
        Key::Home => E::Home,
        Key::End => E::End,
        Key::ArrowUp => E::ArrowUp,
        Key::ArrowDown => E::ArrowDown,
        Key::ArrowLeft => E::ArrowLeft,
        Key::ArrowRight => E::ArrowRight,
        Key::Plus => E::Plus,
        Key::Minus => E::Minus,
        Key::OpenBracket => E::OpenBracket,
        Key::CloseBracket => E::CloseBracket,
        Key::Comma => E::Comma,
        Key::F2 => E::F2,
    }
}

/// Which worker-thread hint one drain puts in the status bar.
#[derive(Debug, PartialEq, Eq)]
enum WorkerNotice {
    NoRealtime,
    NoWorkgroup,
}

/// One message per frame: a rebuilt engine is the worst news (reported by the caller, so no
/// other message of the drain replaces it), then a missing priority. An error already in the
/// status bar (the saved audio input that did not start, reported before the first frame) stays:
/// a priority hint is only an info and never replaces it.
fn worker_notice(
    device_stream: bool,
    rebuilt: bool,
    error_shown: bool,
    no_realtime: bool,
    no_workgroup: bool,
) -> Option<WorkerNotice> {
    if !device_stream || rebuilt || error_shown {
        None
    } else if no_realtime {
        Some(WorkerNotice::NoRealtime)
    } else if no_workgroup {
        Some(WorkerNotice::NoWorkgroup)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::empty_project;

    /// #221: panel sizes count as saved only after a write that worked. A failed one is not
    /// retried every frame, but shutdown writes it again.
    #[test]
    fn a_failed_panel_save_is_written_again_at_shutdown() {
        let dir =
            std::env::temp_dir().join(format!("komponist-panels-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file where the settings folder should be: the write fails.
        let blocker = dir.join("config");
        std::fs::write(&blocker, b"").unwrap();
        let path = blocker.join("settings.json");
        let engine = EngineSync::null(Box::new(crate::device_host::BuiltinHost::new()));
        let mut app = App::new(
            Document::new(empty_project()),
            engine,
            Settings::default(),
            Some(path.clone()),
        );
        app.ui.layout.browser_width = 300.0;
        app.persist_panels();
        assert!(!path.exists());
        assert_ne!(app.shell.saved_panels.unwrap().browser_width, 300.0);
        std::fs::remove_file(&blocker).unwrap();
        app.persist_panels();
        assert!(!path.exists(), "not retried every frame");
        app.shutdown();
        let saved = Settings::load(&path).unwrap();
        assert_eq!(saved.panels.browser_width, 300.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed panel write followed by another settings write (which saves the failed layout
    /// too) and a return to the earlier layout: the file ends up with the layout on screen.
    #[test]
    fn returning_to_the_saved_layout_after_a_failed_write_is_saved() {
        let dir =
            std::env::temp_dir().join(format!("komponist-panels-back-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let engine = EngineSync::null(Box::new(crate::device_host::BuiltinHost::new()));
        let mut app = App::new(
            Document::new(empty_project()),
            engine,
            Settings::default(),
            Some(path.clone()),
        );
        let width = |path: &std::path::Path| Settings::load(path).unwrap().panels.browser_width;
        let blocker = dir.join("blocked");
        std::fs::write(&blocker, b"").unwrap();
        let set_width = |app: &mut App, width: f32, writable: bool| {
            app.settings_path = Some(if writable {
                path.clone()
            } else {
                blocker.join("settings.json")
            });
            app.ui.layout.browser_width = width;
            app.persist_panels();
        };
        // A (300) is saved, B (320) fails, the layout goes back to A, another setting is saved.
        set_width(&mut app, 300.0, true);
        set_width(&mut app, 320.0, false);
        set_width(&mut app, 300.0, true);
        assert!(app.save_settings());
        assert_eq!(width(&path), 300.0);
        // B fails, another setting saves B with it, the layout goes back to A.
        set_width(&mut app, 320.0, false);
        app.settings_path = Some(path.clone());
        assert!(app.save_settings());
        assert_eq!(width(&path), 320.0);
        set_width(&mut app, 300.0, true);
        assert_eq!(width(&path), 300.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_notices_show_without_an_error_and_never_replace_one() {
        use WorkerNotice::{NoRealtime, NoWorkgroup};
        assert_eq!(
            worker_notice(true, false, false, true, true),
            Some(NoRealtime)
        );
        assert_eq!(
            worker_notice(true, false, false, false, true),
            Some(NoWorkgroup)
        );
        assert_eq!(worker_notice(true, false, true, true, true), None);
        assert_eq!(worker_notice(true, true, false, true, false), None);
        assert_eq!(worker_notice(false, false, false, true, true), None);
    }

    #[test]
    fn a_failed_audio_input_stays_in_the_status_bar_after_the_first_frame() {
        let mut app = App::headless(empty_project());
        app.report_audio_input_failed(&AudioError::NoDevice("gone".into()));
        let shown = app.ui.status.as_ref().map(|s| s.text.clone());
        app.step();
        let after = app.ui.status.as_ref().expect("status kept");
        assert_eq!(Some(&after.text), shown.as_ref());
        assert_eq!(after.kind, StatusKind::Error);
    }

    #[test]
    fn retry_audio_keeps_a_running_stream_untouched() {
        let mut app = App::headless(empty_project());
        app.step();
        let running = app.engine.audio_state().clone();
        assert!(matches!(running, AudioState::Running { .. }));
        // A saved config that differs from the running stream: a restart would replace it.
        let AudioState::Running { config, .. } = &running else {
            unreachable!()
        };
        let mut other = config.clone();
        other.buffer_frames = if config.buffer_frames == 512 {
            256
        } else {
            512
        };
        app.settings.audio = other;
        app.dispatch(Action::Settings(SettingsAction::RetryAudio));
        assert_eq!(*app.engine.audio_state(), running);
    }

    fn recording_app() -> App {
        let mut app = App::headless(empty_project());
        app.dispatch(Action::Edit(EditAction::NewMidiTrack));
        app
    }

    #[test]
    fn capture_open_failure_rolls_back_the_track_choice_while_output_runs() {
        use komponist_model::{HardwareInput, edits::NewAudioTrack};

        let old_route = AudioInput::Hardware(HardwareInput {
            first: 0,
            stereo: false,
        });
        let new_route = AudioInput::Hardware(HardwareInput {
            first: 1,
            stereo: false,
        });
        let (project, track) = edits::add_audio_track(
            &empty_project(),
            NewAudioTrack {
                input: old_route,
                ..Default::default()
            },
        )
        .unwrap();
        let mut app = App::headless(project);
        let previous = InputConfig {
            device: Some("Previous input".into()),
            device_id: None,
        };
        let failed = InputConfig {
            device: Some("Unavailable input".into()),
            device_id: None,
        };
        app.engine.set_audio_input(Some(previous.clone())).unwrap();
        app.settings.audio_input = Some(previous.clone());
        let scratch = std::env::temp_dir().join(format!(
            "komponist-track-capture-rollback-{}",
            std::process::id()
        ));
        let settings_path = scratch.join("settings.json");
        app.settings.save(&settings_path).unwrap();
        app.settings_path = Some(settings_path.clone());

        app.choose_track_audio_input(track, new_route, failed.clone(), |engine, input| {
            let capture_rejected = input.as_ref() == Some(&failed);
            engine.set_audio_input(input)?;
            assert!(matches!(engine.audio_state(), AudioState::Running { .. }));
            if capture_rejected {
                return Err(AudioError::NoDevice("test capture open failed".into()));
            }
            Ok(())
        });
        assert!(matches!(
            app.engine.audio_state(),
            AudioState::Running { .. }
        ));
        assert_eq!(app.engine.audio_input_config(), Some(&previous));
        assert_eq!(app.project().track(track).unwrap().audio.input, old_route);
        assert_eq!(app.settings.audio_input, Some(previous.clone()));
        assert_eq!(
            Settings::load(&settings_path).unwrap().audio_input,
            Some(previous)
        );
        std::fs::remove_dir_all(scratch).unwrap();
    }

    /// A count-in whose end is still in flight while a project opens (the audio block ends it
    /// after the UI drained the queue in `EngineSync::reset`) must not start a take in the new
    /// project.
    #[test]
    fn count_in_ending_during_project_open_starts_no_take() {
        let mut app = recording_app();
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.shell.take_expected);
        app.open_document(Document::new(empty_project()), None);
        assert!(!app.shell.take_expected);
        app.engine_event(&EngineEvent::CountInDone { at: Tick::ZERO });
        assert!(app.recording().is_none());
    }

    #[test]
    fn count_in_ending_after_cancel_or_discard_starts_no_take() {
        let mut app = recording_app();
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        app.cancel_count_in();
        app.engine_event(&EngineEvent::CountInDone { at: Tick::ZERO });
        assert!(app.recording().is_none());
    }

    #[test]
    fn an_audio_driver_change_is_blocked_before_the_midi_take_event() {
        let mut app = recording_app();
        let old = app.settings.audio.clone();
        let mut next = old.clone();
        next.host = komponist_audio::HostKind::Jack;
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.shell.take_expected);
        assert!(app.audio_recording().is_none() && app.recording().is_none());
        app.dispatch(Action::Settings(SettingsAction::SetAudio(next)));
        assert_eq!(app.settings.audio, old);
        assert_eq!(
            app.ui.status.as_ref().unwrap().text,
            app.lang.settings_audio_driver_change_recording()
        );
        app.cancel_count_in();
        assert!(!app.shell.take_expected);
    }

    /// #210 review: a MIDI change and a compile diagnostic in the same drain as an engine rebuild
    /// do not replace its message (the notice is taken once, so it would be lost).
    #[test]
    fn a_rebuild_outranks_the_other_notices_of_its_frame() {
        use komponist_engine::compile::{Diagnostic, Reason, Subject};
        use komponist_model::fixtures::demo_project;

        let mut app = App::headless(demo_project());
        app.step();
        let lead = app.project().tracks[2].channel.devices[0].id;
        // Single-threaded, so the panic is on this thread and outside `assert_no_alloc`.
        assert_eq!(app.engine.set_workers(Some(0)).unwrap().unwrap().threads, 0);
        let slot = app.engine.program().unwrap().slots.slot(lead).unwrap();
        app.engine.hold_engine().unwrap();
        app.engine.send(Command::InsertProcessor {
            slot,
            processor: Box::new(komponist_engine::testing::TestPanic),
        });
        let engine = &mut app.engine;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.run_held(256, &[], 0);
        }));
        assert!(caught.is_err(), "the processor ran");
        app.engine.release_engine();
        app.engine.queue_notices(
            MidiChange::Appeared("P-125".into()),
            Diagnostic {
                subject: Subject::Track {
                    track: app.project().tracks[0].id,
                    index: 0,
                },
                reason: Reason::TrackLimit,
            },
        );
        app.ui.status = None;
        app.drain_events();
        assert_eq!(
            app.ui.status.as_ref().map(|s| (s.text.as_str(), s.kind)),
            Some((app.lang.status_engine_rebuilt(), StatusKind::Error))
        );
        // Both notices were handled in that drain.
        assert!(app.engine.take_midi_changes().is_empty());
        assert!(!app.engine.take_diagnostics_notice());
    }

    #[test]
    fn a_checkpoint_and_interruption_preserve_an_incomplete_midi_take() {
        let mut app = recording_app();
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        app.engine_event(&EngineEvent::CountInDone { at: Tick::ZERO });
        let track = app.recording().unwrap().first().unwrap().0;
        app.engine_event(&EngineEvent::RecordingNoteLoss { track, lap: 0 });
        assert!(app.has_unsaved_take());

        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("Checkpoint.komponist");
        assert!(app.save_to(&path));
        let checkpoint = crate::project_io::load_project(&path).unwrap();
        let name = app.lang.record_take_incomplete(1);
        assert_eq!(checkpoint.track(track).unwrap().clips[0].name, name);
        assert!(app.project().track(track).unwrap().clips.is_empty());

        app.engine_event(&EngineEvent::RecordingInterrupted {
            at: Tick(20),
            first_incomplete_lap: 0,
            triggering_track: None,
        });
        assert!(app.recording().is_none());
        assert_eq!(app.project().track(track).unwrap().clips[0].name, name);
        assert_eq!(
            app.ui.status.as_ref().unwrap().text,
            app.lang.record_interrupted()
        );

        // A new request consumes only its own count-in and starts a clean take.
        app.shell.take_expected = true;
        app.engine_event(&EngineEvent::CountInDone { at: Tick(40) });
        assert!(app.recording().is_some());
        assert!(!app.shell.take_expected);
        assert!(!app.has_unsaved_take());
    }

    #[test]
    fn an_interrupted_take_includes_the_track_whose_first_loss_overflowed() {
        let mut app = recording_app();
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        app.engine_event(&EngineEvent::CountInDone { at: Tick::ZERO });
        let first = app.recording().unwrap().first().unwrap().0;
        app.dispatch(Action::Edit(EditAction::NewMidiTrack));
        let added = app
            .project()
            .tracks
            .iter()
            .find(|track| track.id != first)
            .unwrap()
            .id;
        assert!(app.recording().unwrap().clip(added).is_none());
        app.engine_event(&EngineEvent::RecordingInterrupted {
            at: Tick(20),
            first_incomplete_lap: 0,
            triggering_track: Some(added),
        });
        assert_eq!(
            app.project().track(added).unwrap().clips[0].name,
            app.lang.record_take_incomplete(2)
        );
    }

    #[test]
    fn count_in_ending_after_a_record_starts_the_take() {
        let mut app = recording_app();
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        app.engine_event(&EngineEvent::CountInDone { at: Tick::ZERO });
        assert!(app.recording().is_some());
        assert!(!app.shell.take_expected);
    }
}
