//! The app: document, view state, engine, views, keymap and settings, with the one place that
//! applies [`Action`]s ([`App::dispatch`], ARCHITECTURE §9.3).
//!
//! A frame ([`App::show`]):
//!
//! 1. poll the engine (events, garbage, device status) and advance the meters;
//! 2. turn this frame's key events into actions through the [`Keymap`] (text fields first, then
//!    the computer-keyboard MIDI layer, then the table) and dispatch them;
//! 3. draw the shell and the views, which queue actions;
//! 4. dispatch the queue, then [`EngineSync::sync`] the document to the engine;
//! 5. persist changed panel settings.
//!
//! [`App::headless`] builds the app on the null audio backend without a window, so tests drive it
//! through `egui_kittest` or call [`App::dispatch`] directly.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, Event, FocusDirection, Ui};
use komponist_audio::AudioState;
use komponist_engine::{Command, TransportState};
use komponist_model::edits::{
    self, ChannelProp, EditResult, NewDevice, NewTrack, ProjectProp, TrackProp, VelocityChange,
};
use komponist_model::project::{BUILTIN_PISYNTH, VOLUME_DB_MIN};
use komponist_model::{
    ChannelRef, ClipContent, DeviceRole, Document, EditError, PluginFormat, PluginRef, Project,
    Tick, TickRange, TrackId, fixtures, persist,
};

use crate::actions::{
    Action, DropTarget, EditAction, PluginAction, ProjectAction, SelectMode, SelectionChange,
    SettingsAction, TransportAction, UiAction,
};
use crate::device_host::{BuiltinHost, DeviceHost};
use crate::engine_sync::EngineSync;
use crate::keymap::{self, Key, KeyChord, Keymap};
use crate::meters::Meters;
use crate::metrics;
use crate::settings::{self, PanelSettings, Settings};
use crate::ui_state::{
    Dialog, DockTab, ExportDialog, FocusArea, Panel, PluginView, PluginWindow, Screen,
    SettingsSection, StatusKind, StatusMessage, Tool, UiState,
};
use crate::views::{Requests, Views};

/// How long a status bar message stays (`ks-view.js` `statusView`: 7000 ms).
pub const STATUS_DURATION: Duration = Duration::from_secs(7);

/// Most entries kept in the dispatch log ([`App::dispatched`]).
const LOG_LIMIT: usize = 512;

/// Velocity of computer-keyboard notes (prototype `noteOn(…, 100, …)`).
const KEYBOARD_VELOCITY: u8 = 100;

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
    project_path: Option<PathBuf>,
    /// Actions queued by the shell and the views during this frame.
    pub(crate) queue: Vec<Action>,
    log: VecDeque<Action>,
    pub(crate) shell: ShellState,
}

/// Per-session state of the shell that is not view state.
#[derive(Debug, Default)]
pub(crate) struct ShellState {
    /// Focus to restore when the dialog closes.
    focus_before_dialog: Option<FocusArea>,
    /// Computer-keyboard notes held down: key and MIDI note.
    held_notes: Vec<(Key, u8)>,
    /// One-shot requests for the views of the next frame.
    pub(crate) requests: Requests,
    /// A UI scale change to apply at the start of the next frame.
    pending_scale: Option<Option<f32>>,
    /// Width of the arrangement's timeline in the last frame (for "Z: einpassen").
    pub(crate) timeline_width: f32,
    /// The panel settings last written to the settings file.
    saved_panels: Option<PanelSettings>,
    /// The last edit that failed (diagnostics, tests).
    last_error: Option<EditError>,
    /// The window title last sent ("komponist · Nachtfahrt").
    title: Option<String>,
}

impl App {
    /// The app on `engine` with `doc` open (screen `Project`). `settings` are applied (panel
    /// sizes); `settings_path` is where changes are saved (`None`: never).
    pub fn new(
        doc: Document,
        engine: EngineSync,
        settings: Settings,
        settings_path: Option<PathBuf>,
    ) -> App {
        let mut ui = UiState::new();
        ui.screen = Screen::Project;
        settings.panels.apply_to(&mut ui.layout);
        let saved_panels = Some(settings.panels);
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
        }
    }

    /// The app on the null backend with `project` open, default settings, nothing persisted: for
    /// tests (and `--null-audio`).
    pub fn headless(project: Project) -> App {
        let engine = EngineSync::null(Box::new(BuiltinHost::new()));
        App::new(Document::new(project), engine, Settings::default(), None)
    }

    /// Replaces the device host (WP-45 plugs in the plugin manager here).
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

    /// Transport state from the engine.
    pub fn transport(&self) -> TransportState {
        self.engine.telemetry().transport()
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

    // --------------------------------------------------------------------------------------------
    // Frame
    // --------------------------------------------------------------------------------------------

    /// One frame: engine, keys, shell and views, dispatch, sync (module docs).
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if let Some(scale) = self.shell.pending_scale.take() {
            ctx.set_pixels_per_point(scale.unwrap_or(1.0));
        }
        self.poll_engine(ctx.input(|i| i.time));
        self.handle_keys(&ctx);
        // The whole window, whatever frame the host put around `ui` (kittest adds a margin).
        let screen = ctx.content_rect();
        let mut root = ui.new_child(egui::UiBuilder::new().max_rect(screen));
        root.set_clip_rect(screen);
        self.draw_shell(&mut root);
        self.shell.requests = Requests::default();
        self.flush_queue();
        self.engine.sync(&self.doc);
        self.persist_panels();
        self.update_title(&ctx);
        self.schedule_repaint(&ctx);
    }

    /// The window title follows the project name ("komponist · Nachtfahrt").
    fn update_title(&mut self, ctx: &Context) {
        let title = window_title(&self.project().name);
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
        // Garbage is dropped inside `EngineSync::poll`. The rest has no consumer yet: recording
        // (WP-51) builds takes from RecordedNoteOn/Off, LoopWrapped and CountInDone; the plugin
        // manager's state snapshots go into the document with WP-45.
        for _event in self.engine.poll() {}
        self.meters.update(&self.engine, now);
    }

    fn schedule_repaint(&self, ctx: &Context) {
        if self.transport() != TransportState::Stopped || self.meters.is_active() {
            ctx.request_repaint();
        } else if let Some(status) = &self.ui.status {
            let left = STATUS_DURATION.saturating_sub(status.since.elapsed());
            if !left.is_zero() {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn persist_panels(&mut self) {
        let panels = PanelSettings::from_layout(&self.ui.layout);
        if self.shell.saved_panels == Some(panels) {
            return;
        }
        self.settings.panels = panels;
        self.shell.saved_panels = Some(panels);
        self.save_settings();
    }

    /// Writes the settings file (if the app has one); failures go to the status bar.
    pub fn save_settings(&mut self) {
        let Some(path) = self.settings_path.clone() else {
            return;
        };
        if let Err(e) = self.settings.save(&path) {
            self.set_status(e.to_string(), StatusKind::Error);
        }
    }

    /// Stops the engine and saves the settings (window closed).
    pub fn shutdown(&mut self) {
        self.persist_panels();
        self.engine.shutdown();
    }

    // --------------------------------------------------------------------------------------------
    // Keys
    // --------------------------------------------------------------------------------------------

    /// Whether the computer keyboard plays notes: while a track is armed (ARCHITECTURE §13.2) or
    /// when switched on explicitly, and never in a dialog.
    pub fn keyboard_midi_active(&self) -> bool {
        self.ui.screen == Screen::Project
            && self.ui.dialog.is_none()
            && (self.ui.keyboard_midi || self.project().tracks.iter().any(|t| t.arm))
    }

    /// Turns this frame's key events into actions and dispatches them (module docs). Keys that
    /// trigger something are consumed, so widgets do not see them.
    fn handle_keys(&mut self, ctx: &Context) {
        if self.ui.screen != Screen::Project || ctx.egui_wants_keyboard_input() {
            return;
        }
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
            let ctrl = modifiers.ctrl || modifiers.command;
            let physical = key_from_egui(physical_key.unwrap_or(key));
            // A held note ends on release even if the layer turned off meanwhile.
            let held = physical.is_some_and(|k| self.shell.held_notes.iter().any(|(h, _)| *h == k));
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
            // Shift is part of typing "+" on some layouts.
            let shift = modifiers.shift && !matches!(k, Key::Plus | Key::Minus);
            let chord = KeyChord {
                key: k,
                mods: keymap::Modifiers {
                    ctrl,
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
            if widget_keys && navigation && !ctrl {
                continue;
            }
            let Some(action) = self.keymap.lookup(chord, self.ui.focus).cloned() else {
                continue;
            };
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

    /// Context-dependent meaning of a shortcut: Esc while recording discards the take.
    fn remap(&self, action: Action) -> Action {
        match action {
            Action::Ui(UiAction::ClearSelection)
                if matches!(
                    self.transport(),
                    TransportState::Recording | TransportState::CountIn
                ) =>
            {
                Action::Transport(TransportAction::DiscardTake)
            }
            other => other,
        }
    }

    fn keyboard_note(&mut self, key: Key, note: u8, pressed: bool, repeat: bool) {
        if pressed {
            if repeat || self.shell.held_notes.iter().any(|(k, _)| *k == key) {
                return;
            }
            self.shell.held_notes.push((key, note));
            self.dispatch(Action::Transport(TransportAction::LiveNote {
                track: None,
                key: note,
                velocity: KEYBOARD_VELOCITY,
            }));
        } else if let Some(i) = self.shell.held_notes.iter().position(|(k, _)| *k == key) {
            let (_, note) = self.shell.held_notes.remove(i);
            self.dispatch(Action::Transport(TransportAction::LiveNote {
                track: None,
                key: note,
                velocity: 0,
            }));
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
        match action {
            Action::Transport(a) => self.transport_action(a),
            Action::Edit(a) => self.edit_action(a),
            Action::Ui(a) => self.ui_action(a),
            Action::Plugin(a) => self.plugin_action(a),
            Action::Project(a) => self.project_action(a),
            Action::Settings(a) => self.settings_action(a),
        }
    }

    fn transport_action(&mut self, action: TransportAction) {
        let playing = self.transport() != TransportState::Stopped;
        match action {
            TransportAction::TogglePlay => {
                if playing {
                    self.engine.send(Command::Stop);
                } else if self.audio_ok() {
                    self.engine.send(Command::Play);
                }
            }
            TransportAction::Stop => {
                if playing {
                    self.engine.send(Command::Stop);
                } else {
                    let start = self.start_tick();
                    self.engine.send(Command::Locate(start));
                }
            }
            TransportAction::ToggleRecord => self.toggle_record(),
            TransportAction::DiscardTake => {
                // WP-51 drops the take it builds; the engine stops capturing, playback goes on.
                self.engine.send(Command::StopRecording);
            }
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

    /// R: the engine part of recording. Takes are built by WP-51 (`record.rs`) from the engine's
    /// recorded-note events.
    fn toggle_record(&mut self) {
        match self.transport() {
            TransportState::Recording | TransportState::CountIn => {
                self.engine.send(Command::StopRecording);
            }
            _ if !self.audio_ok() => {}
            state => {
                if !self.project().tracks.iter().any(|t| t.arm)
                    && let Some(&track) = self.ui.selection.tracks.iter().next()
                {
                    self.apply("Spur scharf schalten", |p| {
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
                self.engine.send(Command::Record { from });
            }
        }
    }

    /// Applies a transport setting outside the history (`Document::apply_untracked`).
    fn untracked(&mut self, prop: ProjectProp) {
        if let Err(e) = self
            .doc
            .apply_untracked(|p| edits::set_project_prop(p, prop.clone()))
        {
            self.shell.last_error = Some(e);
        }
    }

    /// Applies `edit` as an undo step `label` (or inside the open gesture); records a failure.
    fn apply(&mut self, label: &str, edit: impl FnOnce(&Project) -> EditResult) -> bool {
        match self.doc.apply(label, edit) {
            Ok(()) => true,
            Err(e) => {
                self.shell.last_error = Some(e);
                false
            }
        }
    }

    /// Like [`App::apply`] for edits returning a value.
    fn apply_with<T>(
        &mut self,
        label: &str,
        edit: impl FnOnce(&Project) -> Result<(Project, T), EditError>,
    ) -> Option<T> {
        match self.doc.apply_with(label, edit) {
            Ok(v) => Some(v),
            Err(e) => {
                self.shell.last_error = Some(e);
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
        let result = if self.doc.in_gesture() {
            self.doc.apply_in_gesture_with(edit)
        } else {
            self.doc.apply_with(label, edit)
        };
        match result {
            Ok(v) => Some(v),
            Err(e) => {
                self.shell.last_error = Some(e);
                None
            }
        }
    }

    fn open_clip_notes(&self) -> Option<(komponist_model::ClipId, bool)> {
        let id = self.ui.editor.clip?;
        let (_, clip) = self.project().find_clip(id)?;
        Some((id, matches!(clip.content, ClipContent::Midi(_))))
    }

    fn selected_notes(&self) -> Vec<komponist_model::NoteId> {
        self.ui.selection.notes.iter().copied().collect()
    }

    fn edit_action(&mut self, action: EditAction) {
        match action {
            EditAction::Undo => {
                self.doc.undo();
            }
            EditAction::Redo => {
                self.doc.redo();
            }
            EditAction::BeginGesture { label } => self.doc.begin_gesture(label),
            EditAction::EndGesture => self.doc.end_gesture(),
            EditAction::CancelGesture => self.doc.cancel_gesture(),
            EditAction::DeleteSelection => self.delete_selection(),
            EditAction::DuplicateSelection => {
                let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
                if clips.is_empty() {
                    return;
                }
                if let Some(copies) =
                    self.apply_with("Clips duplizieren", |p| edits::duplicate_clips(p, &clips))
                {
                    self.select_clips(copies, SelectMode::Replace);
                }
            }
            EditAction::SplitAtPlayhead => {
                let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
                if clips.is_empty() {
                    return;
                }
                let at = self.engine.playhead();
                self.apply_with("Clips teilen", |p| edits::split_clips(p, &clips, at));
            }
            EditAction::ToggleMuteSelection => {
                let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
                let p = self.project();
                let any_unmuted = clips
                    .iter()
                    .filter_map(|&c| p.find_clip(c))
                    .any(|(_, c)| !c.mute);
                if clips.is_empty() {
                    return;
                }
                self.apply("Clips stumm schalten", |p| {
                    edits::set_clips_muted(p, &clips, any_unmuted)
                });
            }
            EditAction::ToggleMuteTracks => {
                let tracks: Vec<_> = self.ui.selection.tracks.iter().copied().collect();
                let p = self.project();
                let any_unmuted = tracks
                    .iter()
                    .filter_map(|&t| p.track(t))
                    .any(|t| !t.channel.mute);
                if tracks.is_empty() {
                    return;
                }
                self.apply("Spuren stumm schalten", |p| {
                    edits::set_track_prop(
                        p,
                        &tracks,
                        TrackProp::Channel(ChannelProp::Mute(any_unmuted)),
                    )
                });
            }
            EditAction::QuantizeSelection => {
                let Some((clip, true)) = self.open_clip_notes() else {
                    return;
                };
                let notes = self.selected_notes();
                let grid = self.ui.piano_roll.grid.ticks(self.project().meter);
                self.apply("Quantisieren", |p| {
                    let sel = (!notes.is_empty()).then_some(notes.as_slice());
                    edits::quantize_notes(p, clip, sel, grid)
                });
            }
            EditAction::TransposeSelection(semitones) => {
                let Some((clip, true)) = self.open_clip_notes() else {
                    return;
                };
                let notes = self.selected_notes();
                if notes.is_empty() {
                    return;
                }
                self.apply("Noten transponieren", |p| {
                    edits::move_notes(p, clip, &notes, Tick::ZERO, i32::from(semitones))
                });
            }
            EditAction::NudgeSelection(steps) => {
                let Some((clip, true)) = self.open_clip_notes() else {
                    return;
                };
                let notes = self.selected_notes();
                if notes.is_empty() {
                    return;
                }
                let grid = self.ui.piano_roll.grid.ticks(self.project().meter);
                let delta = Tick(grid.0 * i64::from(steps));
                self.apply("Noten verschieben", |p| {
                    edits::move_notes(p, clip, &notes, delta, 0)
                });
            }
            EditAction::NewMidiTrack => self.new_midi_track(None, pisynth_ref()),
            EditAction::SetProjectProp(prop) => {
                self.apply("Projekt ändern", |p| edits::set_project_prop(p, prop));
            }
            EditAction::SetMasterProp(prop) => {
                let before = self.doc.revision();
                if self.apply("Master ändern", |p| edits::set_master_prop(p, prop))
                    && self.doc.in_gesture()
                    && let ChannelProp::VolumeDb(db) = prop
                {
                    let after = self.doc.revision();
                    self.engine
                        .fast_path(Command::SetMasterGain(fader_gain(db)), before, after);
                }
            }
            EditAction::AddTrack(spec) => {
                if let Some(id) = self.apply_with("Spur hinzufügen", |p| edits::add_track(p, spec))
                {
                    self.select_tracks(vec![id], SelectMode::Replace);
                }
            }
            EditAction::RemoveTracks(tracks) => {
                if self.apply("Spuren löschen", |p| edits::remove_tracks(p, &tracks)) {
                    self.prune_selection();
                }
            }
            EditAction::MoveTracks { tracks, to_index } => {
                self.apply("Spuren verschieben", |p| {
                    edits::move_tracks(p, &tracks, to_index)
                });
            }
            EditAction::SetTrackProp { tracks, prop } => self.set_track_prop(tracks, prop),
            EditAction::SetExclusive { track, flag } => {
                self.apply("Exklusiv schalten", |p| {
                    edits::set_exclusive(p, track, flag)
                });
            }
            EditAction::AddClip { track, clip } => {
                if let Some(id) =
                    self.apply_with("Clip anlegen", |p| edits::add_clip(p, track, clip))
                {
                    self.select_clips(vec![id], SelectMode::Replace);
                    self.ui_action(UiAction::OpenClip(id));
                }
            }
            EditAction::RemoveClips(clips) => {
                if self.apply("Clips löschen", |p| edits::remove_clips(p, &clips)) {
                    self.prune_selection();
                }
            }
            EditAction::MoveClips {
                clips,
                delta,
                track_delta,
            } => {
                self.apply_delta("Clips verschieben", |p| {
                    edits::move_clips(p, &clips, delta, track_delta).map(|p| (p, ()))
                });
            }
            EditAction::CopyClips {
                clips,
                delta,
                track_delta,
            } => {
                if let Some(copies) = self.apply_delta("Clips kopieren", |p| {
                    edits::copy_clips(p, &clips, delta, track_delta)
                }) {
                    self.select_clips(copies, SelectMode::Replace);
                }
            }
            EditAction::SetClipLength { clip, length } => {
                self.apply("Clip-Länge", |p| edits::set_clip_length(p, clip, length));
            }
            EditAction::SetClipLoop {
                clip,
                loop_length,
                length,
            } => {
                self.apply("Clip-Loop", |p| {
                    edits::set_clip_loop(p, clip, loop_length, length)
                });
            }
            EditAction::SetClipsMuted { clips, muted } => {
                self.apply("Clips stumm schalten", |p| {
                    edits::set_clips_muted(p, &clips, muted)
                });
            }
            EditAction::SetClipProp { clip, prop } => {
                self.apply("Clip ändern", |p| edits::set_clip_prop(p, clip, prop));
            }
            EditAction::SelectTake { clip, take } => {
                self.apply("Take wählen", |p| edits::select_take(p, clip, take));
            }
            EditAction::AddNotes { clip, notes } => {
                if let Some(ids) =
                    self.apply_with("Noten hinzufügen", |p| edits::add_notes(p, clip, &notes))
                {
                    self.ui.selection.notes = ids.into_iter().collect();
                }
            }
            EditAction::RemoveNotes { clip, notes } => {
                if self.apply("Noten löschen", |p| edits::remove_notes(p, clip, &notes)) {
                    for n in &notes {
                        self.ui.selection.notes.remove(n);
                    }
                }
            }
            EditAction::MoveNotes {
                clip,
                notes,
                delta,
                pitch_delta,
            } => {
                self.apply_delta("Noten verschieben", |p| {
                    edits::move_notes(p, clip, &notes, delta, pitch_delta).map(|p| (p, ()))
                });
            }
            EditAction::CopyNotes {
                clip,
                notes,
                delta,
                pitch_delta,
            } => {
                if let Some(ids) = self.apply_delta("Noten kopieren", |p| {
                    edits::copy_notes(p, clip, &notes, delta, pitch_delta)
                }) {
                    self.ui.selection.notes = ids.into_iter().collect();
                }
            }
            EditAction::ResizeNotes { clip, notes, delta } => {
                self.apply_delta("Noten-Länge", |p| {
                    edits::resize_notes(p, clip, &notes, delta).map(|p| (p, ()))
                });
            }
            EditAction::SetNotesVelocity {
                clip,
                notes,
                change,
            } => {
                let label = "Velocity";
                match change {
                    VelocityChange::Offset(_) => {
                        self.apply_delta(label, |p| {
                            edits::set_notes_velocity(p, clip, &notes, change).map(|p| (p, ()))
                        });
                    }
                    VelocityChange::Set(_) => {
                        self.apply(label, |p| {
                            edits::set_notes_velocity(p, clip, &notes, change)
                        });
                    }
                }
            }
            EditAction::SetStep {
                clip,
                row,
                step,
                prop,
            } => {
                self.apply("Step", |p| edits::set_step(p, clip, row, step, prop));
            }
            EditAction::SetStepClipProp { clip, prop } => {
                self.apply("Pattern ändern", |p| {
                    edits::set_step_clip_prop(p, clip, prop)
                });
            }
            EditAction::SetStepRow { clip, row, prop } => {
                self.apply("Zeile ändern", |p| edits::set_step_row(p, clip, row, prop));
            }
            EditAction::StepClipToMidi(clip) => {
                self.apply("Als MIDI-Clip", |p| edits::step_clip_to_midi(p, clip));
            }
            EditAction::RemoveDevice(device) => {
                if self.apply("Gerät entfernen", |p| edits::remove_device(p, device)) {
                    self.ui.selection.devices.remove(&device);
                }
            }
            EditAction::MoveDevice { device, to_index } => {
                self.apply("Gerät verschieben", |p| {
                    edits::move_device(p, device, to_index)
                });
            }
            EditAction::SetDeviceBypass { device, bypass } => {
                self.apply("Bypass", |p| edits::set_device_bypass(p, device, bypass));
            }
        }
    }

    fn set_track_prop(&mut self, tracks: Vec<TrackId>, prop: TrackProp) {
        let before = self.doc.revision();
        let fast = match &prop {
            TrackProp::Channel(c) => Some(*c),
            _ => None,
        };
        if !self.apply("Spur ändern", |p| edits::set_track_prop(p, &tracks, prop)) {
            return;
        }
        let (Some(channel), true) = (fast, self.doc.in_gesture()) else {
            return;
        };
        let after = self.doc.revision();
        for track in tracks {
            let command = match channel {
                ChannelProp::VolumeDb(db) => Command::SetTrackGain {
                    track,
                    gain: fader_gain(db),
                },
                ChannelProp::Pan(pan) => Command::SetTrackPan { track, pan },
                ChannelProp::Mute(mute) => Command::SetTrackMute { track, mute },
                ChannelProp::Solo(solo) => Command::SetTrackSolo { track, solo },
            };
            self.engine.fast_path(command, before, after);
        }
    }

    /// Entf: notes in the editor, else clips, else tracks.
    fn delete_selection(&mut self) {
        let editor = matches!(
            self.ui.focus,
            FocusArea::PianoRoll | FocusArea::StepSequencer
        );
        if editor
            && let Some((clip, true)) = self.open_clip_notes()
            && !self.ui.selection.notes.is_empty()
        {
            let notes = self.selected_notes();
            if self.apply("Noten löschen", |p| edits::remove_notes(p, clip, &notes)) {
                self.ui.selection.notes.clear();
            }
            return;
        }
        if editor {
            return;
        }
        if !self.ui.selection.clips.is_empty() {
            let clips: Vec<_> = self.ui.selection.clips.iter().copied().collect();
            if self.apply("Clips löschen", |p| edits::remove_clips(p, &clips)) {
                self.prune_selection();
            }
        } else if !self.ui.selection.tracks.is_empty() {
            let tracks: Vec<_> = self.ui.selection.tracks.iter().copied().collect();
            if self.apply("Spuren löschen", |p| edits::remove_tracks(p, &tracks)) {
                self.prune_selection();
            }
        }
    }

    /// Ctrl T and instrument drops: a new MIDI track with `instrument`, armed exclusively,
    /// selected (S10).
    fn new_midi_track(&mut self, index: Option<usize>, instrument: PluginRef) {
        let spec = NewTrack {
            name: None,
            hue: None,
            instrument: Some(instrument),
            index,
            arm_exclusive: true,
        };
        if let Some(id) = self.apply_with("Neue MIDI-Spur", |p| edits::add_track(p, spec)) {
            self.select_tracks(vec![id], SelectMode::Replace);
        }
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
            UiAction::NextTab => self.next_tab(),
            UiAction::Focus(area) => {
                if self.ui.dialog.is_none() {
                    self.ui.focus = area;
                }
            }
            UiAction::Zoom { vertical, steps } => self.zoom(vertical, steps),
            UiAction::ZoomToFit => self.zoom_to_fit(),
            UiAction::GridFiner | UiAction::GridCoarser => {
                let finer = action == UiAction::GridFiner;
                let grid = self.grid_mut();
                *grid = if finer { grid.finer() } else { grid.coarser() };
            }
            UiAction::SetGrid(grid) => *self.grid_mut() = grid,
            UiAction::ToggleSnap => {
                if self.piano_roll_focused() {
                    self.ui.piano_roll.snap = !self.ui.piano_roll.snap;
                } else {
                    self.ui.arrangement.snap = !self.ui.arrangement.snap;
                }
            }
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
                if self.ui.dialog.is_some() {
                    self.ui_action(UiAction::CloseDialog);
                } else if !self.ui.plugin_windows.is_empty() {
                    self.ui.plugin_windows.pop();
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
            UiAction::OpenSettings(section) => self.open_dialog(Dialog::Settings(section)),
            UiAction::CloseDialog => {
                // Esc reaches both the keymap and the modal; the second close changes nothing.
                if self.ui.dialog.take().is_none() {
                    return;
                }
                self.ui.focus = self
                    .shell
                    .focus_before_dialog
                    .take()
                    .unwrap_or(FocusArea::Arrangement);
            }
            UiAction::ToggleKeyboardMidi => self.ui.keyboard_midi = !self.ui.keyboard_midi,
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
        }
    }

    /// SetDockTab: opens the dock; leaving the mixer that Tab maximised restores the layout
    /// (prototype `setDockTab`).
    fn set_dock_tab(&mut self, tab: DockTab) {
        let layout = &mut self.ui.layout;
        layout.dock_open = true;
        layout.dock_tab = tab;
        if tab != DockTab::Mixer
            && layout.dock_maximized
            && let Some((_, maximized)) = layout.before_mixer.take()
        {
            layout.dock_maximized = maximized;
        }
        if self.ui.focus.in_dock() {
            self.ui.focus = self.ui.dock_area(self.doc.project());
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
                self.ui.focus = self.ui.dock_area(self.doc.project());
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
                self.ui.dock_area(self.doc.project())
            } else {
                FocusArea::Arrangement
            };
        }
    }

    fn open_dialog(&mut self, dialog: Dialog) {
        if self.ui.dialog.is_none() {
            self.shell.focus_before_dialog = Some(self.ui.focus);
        }
        self.ui.dialog = Some(dialog);
        self.ui.focus = FocusArea::Dialog;
    }

    fn piano_roll_focused(&self) -> bool {
        self.ui.focus == FocusArea::PianoRoll
    }

    fn grid_mut(&mut self) -> &mut komponist_model::Grid {
        if self.piano_roll_focused() {
            &mut self.ui.piano_roll.grid
        } else {
            &mut self.ui.arrangement.grid
        }
    }

    fn zoom(&mut self, vertical: bool, steps: i32) {
        use metrics::arrangement::{PX_PER_BAR_MAX, PX_PER_BAR_MIN, V_ZOOM, ZOOM_STEP};
        let factor = ZOOM_STEP.powi(steps);
        match self.ui.focus {
            FocusArea::PianoRoll => {
                let pr = &mut self.ui.piano_roll;
                if vertical {
                    pr.key_height = (pr.key_height * factor).clamp(
                        komponist_tokens::size::key::MIN,
                        komponist_tokens::size::key::MAX,
                    );
                } else {
                    pr.px_per_bar = (pr.px_per_bar * factor).max(PX_PER_BAR_MIN);
                }
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

    fn plugin_action(&mut self, action: PluginAction) {
        match action {
            PluginAction::Add { plugin, target } => self.add_plugin(plugin, target),
            PluginAction::ApplyPisynthPreset { device, preset } => {
                let Some(patch) = komponist_builtin::preset_patch(preset) else {
                    return;
                };
                let state: Arc<[u8]> = komponist_builtin::encode_state(&patch).into();
                if self.apply("Preset laden", |p| {
                    edits::set_device_state(p, device, Some(Arc::clone(&state)))
                }) {
                    self.engine.reload_device(self.doc.project(), device);
                }
            }
            PluginAction::OpenWindow(device) => {
                if !self.ui.plugin_windows.iter().any(|w| w.device == device) {
                    self.ui.plugin_windows.push(PluginWindow {
                        device,
                        view: PluginView::Params,
                        width: 0,
                        height: 0,
                    });
                }
            }
            PluginAction::CloseWindow(device) => {
                self.ui.plugin_windows.retain(|w| w.device != device);
            }
            PluginAction::Reload(device) => {
                self.engine.reload_device(self.doc.project(), device);
            }
            PluginAction::SetParam { device, id, value } => {
                if let Some(slot) = self.engine.program().and_then(|p| p.slots.slot(device)) {
                    self.engine.send(Command::SetParam { slot, id, value });
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
                self.new_midi_track(index, plugin);
            }
            (DeviceRole::Instrument, DropTarget::Track(track))
            | (
                DeviceRole::Instrument,
                DropTarget::Chain {
                    channel: ChannelRef::Track(track),
                    ..
                },
            ) => {
                self.apply_with("Instrument ersetzen", |p| {
                    edits::replace_instrument(p, track, plugin)
                });
            }
            (DeviceRole::Instrument, DropTarget::Chain { .. }) => {}
            (DeviceRole::Effect, target) => {
                let (channel, index) = match target {
                    DropTarget::NewTrack { .. } => match selected {
                        Some(t) => (ChannelRef::Track(t), None),
                        None => return,
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
                };
                self.apply_with("Effekt hinzufügen", |p| {
                    edits::insert_device(p, channel, index, spec)
                });
            }
        }
    }

    fn project_action(&mut self, action: ProjectAction) {
        match action {
            ProjectAction::New => {
                self.open_document(Document::new(fixtures::empty_project()), None);
            }
            ProjectAction::OpenPath(path) => match load_project(&path) {
                Ok(project) => {
                    self.open_document(Document::new(project), Some(path.clone()));
                    self.settings.add_recent(path);
                    self.save_settings();
                }
                Err(e) => self.set_status(e, StatusKind::Error),
            },
            // Without a path the file dialog asks for one (WP-52).
            ProjectAction::Save => {
                if let Some(path) = self.project_path.clone() {
                    self.save_to(&path);
                }
            }
            // File dialogs: WP-52.
            ProjectAction::Open | ProjectAction::SaveAs => {}
            ProjectAction::OpenExportDialog => {
                if self.transport() != TransportState::Stopped {
                    self.engine.send(Command::Stop);
                }
                let folder = self
                    .project_path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let mut export = self
                    .settings
                    .export
                    .clone()
                    .unwrap_or_else(|| settings::default_export(&self.project().name, folder));
                export.file_name = format!("{}.wav", self.project().name);
                self.open_dialog(Dialog::Export(ExportDialog {
                    settings: export,
                    progress: None,
                }));
            }
            // The export itself: WP-53.
            ProjectAction::StartExport(export) => {
                self.settings.export = Some(export);
                self.save_settings();
            }
            ProjectAction::CancelExport => {}
            ProjectAction::ContinueWithMissing => self.ui_action(UiAction::CloseDialog),
            // S06 → S07: the settings replace the dialog; the focus to return to stays.
            ProjectAction::CheckPluginFolders => {
                self.open_dialog(Dialog::Settings(SettingsSection::Plugins));
            }
        }
    }

    /// Makes `doc` the open document: fresh selection and editor, transport stopped at 0.
    fn open_document(&mut self, doc: Document, path: Option<PathBuf>) {
        self.engine.send(Command::Stop);
        self.engine.send(Command::Locate(Tick::ZERO));
        self.doc = doc;
        self.project_path = path;
        self.ui.selection = Default::default();
        self.ui.editor = Default::default();
        self.ui.screen = Screen::Project;
        self.ui.dialog = None;
        self.ui.focus = FocusArea::Arrangement;
    }

    fn save_to(&mut self, path: &Path) {
        let result = persist::to_json(self.project())
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(path, json).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                self.doc.mark_saved();
                self.project_path = Some(path.to_path_buf());
            }
            Err(e) => self.set_status(e, StatusKind::Error),
        }
    }

    fn settings_action(&mut self, action: SettingsAction) {
        match action {
            SettingsAction::SetAudio(config) => {
                let _ = self.engine.set_audio(&config);
                self.settings.audio = config;
                self.save_settings();
            }
            SettingsAction::RetryAudio => {
                let config = self.settings.audio.clone();
                let _ = self.engine.set_audio(&config);
            }
            SettingsAction::SetMidiInput { name, enabled } => {
                if let Some(hub) = self.engine.midi_mut() {
                    let _ = hub.set_enabled(&name, enabled);
                }
                self.settings.midi_disabled.retain(|n| *n != name);
                if !enabled {
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
            }
            SettingsAction::RemovePluginDir { clap, path } => {
                self.plugin_dirs(clap).retain(|p| *p != path);
                self.save_settings();
            }
            // Scanning: WP-45.
            SettingsAction::RescanPlugins => {}
            SettingsAction::SetUiScale(scale) => {
                self.settings.ui_scale = scale;
                self.shell.pending_scale = Some(scale);
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

/// The window title for a project: "komponist · Nachtfahrt" (ARCHITECTURE §13.6).
pub fn window_title(project_name: &str) -> String {
    format!("{} · {project_name}", crate::panels::transport::WORDMARK)
}

/// Whether a held key may repeat `action` (zoom, transpose, nudge).
fn repeatable(action: &Action) -> bool {
    matches!(
        action,
        Action::Ui(UiAction::Zoom { .. })
            | Action::Edit(EditAction::TransposeSelection(_) | EditAction::NudgeSelection(_))
    )
}

/// The fader's linear gain for `db` (`VOLUME_DB_MIN` = −∞ → 0), as the compiler computes it.
fn fader_gain(db: f32) -> f32 {
    if db <= VOLUME_DB_MIN {
        0.0
    } else {
        klang_dsp::units::db_to_gain(db)
    }
}

/// The reference of the built-in pisynth (Ctrl T).
pub fn pisynth_ref() -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: BUILTIN_PISYNTH.to_owned(),
        name: "pisynth".to_owned(),
        vendor: "klang-studio".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn load_project(path: &Path) -> Result<Project, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    persist::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
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
    }
}
