//! Every interaction as data (ARCHITECTURE §9.3).
//!
//! Panels, canvases, dialogs and the keymap produce [`Action`]s; one dispatcher (WP-30, `app.rs`)
//! applies them: document edits through `Document::apply` (or a gesture), transport and live notes
//! as engine commands, UI changes on [`crate::ui_state::UiState`], plugin requests through the
//! `PluginManager`. Panels never mutate the document directly, which makes every flow testable
//! without pixels.
//!
//! Drags are gestures: the canvas sends `Edit(BeginGesture)`, then edit actions, then
//! `Edit(EndGesture)` (or `CancelGesture` on Esc). While a gesture is open, the delta-carrying
//! actions (`MoveClips`, `CopyClips`, `MoveNotes`, `CopyNotes`, `ResizeNotes`, `SetNotesVelocity`
//! with an offset) carry the **total** delta since the drag start and are applied with
//! `Document::apply_in_gesture` (to the state at `BeginGesture`), so each replaces the previous
//! intermediate state and clamping never accumulates. All other actions (step painting, fader
//! values, device parameters) are applied to the current state.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use komponist_audio::StreamConfig;
use komponist_model::edits::{
    ChannelProp, ClipProp, ExclusiveFlag, NewBus, NewClip, NewNote, NewTrack, ProjectProp,
    SendProp, StepClipProp, StepProp, StepRowProp, TrackProp, VelocityChange,
};
use komponist_model::{
    ChannelRef, ClipId, DeviceId, Grid, NoteId, PluginRef, SendId, Tick, TickRange, TrackId,
    TrackOutput,
};

use crate::ui_state::{
    BrowserSegment, DockTab, FocusArea, Panel, PluginView, SettingsSection, StatusKind, StepLane,
    Tool,
};

/// One user intention.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Playback, recording, loop, live notes.
    Transport(TransportAction),
    /// Document changes and history.
    Edit(EditAction),
    /// Automation lane, point and clip-envelope edits.
    Automation(crate::views::automation::Action),
    /// Track macro controls and mappings.
    Macro(crate::views::track_macros::Action),
    /// View state: panels, focus, zoom, selection, dialogs.
    Ui(UiAction),
    /// Plugins: add from the browser, GUI windows, reload, parameters.
    Plugin(PluginAction),
    /// Project lifecycle: new, open, save, export.
    Project(ProjectAction),
    /// Settings (S07), applied at once.
    Settings(SettingsAction),
}

/// Transport actions (transport bar, global shortcuts).
#[derive(Debug, Clone, PartialEq)]
pub enum TransportAction {
    /// Space: play / stop (stopping while recording finishes the take; during the count-in it
    /// cancels the recording like R).
    TogglePlay,
    /// Stop button: stop; when already stopped, jump to the loop start (loop on) or tick 0.
    Stop,
    /// R: start recording on all armed tracks after the count-in (arming the selected track if none
    /// is armed), or finish the running recording (punch-out: playback goes on). During the
    /// count-in it cancels the recording, stops and returns to the recording's start.
    ToggleRecord,
    /// Esc while recording: discard the running take, playback continues.
    DiscardTake,
    /// L: loop on/off.
    ToggleLoop,
    /// C: metronome on/off.
    ToggleMetronome,
    /// "Vorzähler" button: count-in on (1 bar) / off.
    ToggleCountIn,
    /// Pos1: to the loop start (loop on) or tick 0.
    GoToStart,
    /// Set the playhead.
    Locate(Tick),
    /// Set the loop range and turn loop on.
    SetLoopRange(TickRange),
    /// Computer keyboard MIDI or step-row audition: a live note on `track`, or with `None` on the
    /// first armed track, else the selected track (prototype `liveTrack`). Velocity 0 = note off.
    LiveNote {
        /// Target track, `None` = the live-target rule.
        track: Option<TrackId>,
        /// MIDI key.
        key: u8,
        /// MIDI velocity, 0 = off.
        velocity: u8,
    },
}

/// Document edits. Variants without ids act on the current selection and focus; the others mirror
/// functions of `komponist_model::edits` one to one. Device insertion goes through
/// [`PluginAction::Add`]; takes (`add_take`), step-row replacement (`set_step_rows`) and plugin
/// state snapshots (`set_device_state`) are issued by the dispatcher itself.
#[derive(Debug, Clone, PartialEq)]
pub enum EditAction {
    /// Ctrl Z.
    Undo,
    /// Ctrl Shift Z.
    Redo,
    /// Start of a drag: following edits form one undo step `label`.
    BeginGesture {
        /// Undo label.
        label: String,
    },
    /// End of a drag: push the undo step.
    EndGesture,
    /// Esc during a drag: restore the state from `BeginGesture`.
    CancelGesture,

    /// Entf: delete selected notes (editor focused), else selected clips, else selected tracks.
    DeleteSelection,
    /// Ctrl D: duplicate selected clips behind themselves.
    DuplicateSelection,
    /// Ctrl E: split selected clips at the playhead.
    SplitAtPlayhead,
    /// 0: toggle mute of the selected clips (all muted if any is unmuted).
    ToggleMuteSelection,
    /// M: toggle mute of the selected tracks.
    ToggleMuteTracks,
    /// Q: quantise the selected notes (all notes of the open clip without selection) to the piano
    /// roll grid.
    QuantizeSelection,
    /// ↑ ↓ / Shift ↑ ↓: transpose the selected notes by semitones (±1, ±12).
    TransposeSelection(i8),
    /// ← →: move the selected notes by grid steps.
    NudgeSelection(i32),
    /// Ctrl T: new MIDI track with hexa, armed exclusively.
    NewMidiTrack,
    /// Ctrl Shift T: open the new-track dialog with Audio selected.
    NewAudioTrack,
    /// Create an armed audio track recording this bus, outside its group.
    RecordBus(TrackId),
    /// Choose an audio take.
    SelectAudioTake { clip: ClipId, take: usize },
    /// Rename an audio take.
    RenameAudioTake {
        clip: ClipId,
        take: usize,
        name: String,
    },
    /// Remove an audio take, keeping at least one.
    RemoveAudioTake { clip: ClipId, take: usize },

    /// `edits::set_project_prop`.
    SetProjectProp(ProjectProp),
    /// `edits::set_master_prop`.
    SetMasterProp(ChannelProp),
    /// `edits::add_track`.
    AddTrack(NewTrack),
    /// `edits::remove_tracks`.
    RemoveTracks(Vec<TrackId>),
    /// Renders an individual MIDI track into an audio clip, then replaces the track.
    FlattenMidiTrack(TrackId),
    /// `edits::move_tracks`.
    MoveTracks {
        /// Tracks to move.
        tracks: Vec<TrackId>,
        /// Target index without the moved tracks.
        to_index: usize,
    },
    /// `edits::set_track_prop`.
    SetTrackProp {
        /// Tracks.
        tracks: Vec<TrackId>,
        /// Property.
        prop: TrackProp,
    },
    /// `edits::set_exclusive` (Alt-click S / R).
    SetExclusive {
        /// The track.
        track: TrackId,
        /// Solo or arm.
        flag: ExclusiveFlag,
    },
    /// `edits::add_clip` (double-click on an empty lane: one bar, then open it).
    AddClip {
        /// Target track.
        track: TrackId,
        /// The clip.
        clip: NewClip,
    },
    /// `edits::remove_clips`.
    RemoveClips(Vec<ClipId>),
    /// `edits::move_clips`.
    MoveClips {
        /// Clips.
        clips: Vec<ClipId>,
        /// Ticks.
        delta: Tick,
        /// Tracks (negative = up).
        track_delta: i32,
    },
    /// `edits::copy_clips` (Alt-drag).
    CopyClips {
        /// Clips.
        clips: Vec<ClipId>,
        /// Ticks.
        delta: Tick,
        /// Tracks.
        track_delta: i32,
    },
    /// `edits::set_clip_length` (edge drag).
    SetClipLength {
        /// The clip.
        clip: ClipId,
        /// New length.
        length: Tick,
    },
    /// Trim the source start of an audio clip.
    TrimAudioClipStart {
        /// Clip to trim.
        clip: ClipId,
        /// New timeline start.
        start: Tick,
    },
    /// Replace the linked file while preserving the audio clip settings.
    SetAudioClipFile {
        /// Clip to relink.
        clip: ClipId,
        /// Decoded file metadata and link.
        file: komponist_model::AudioFile,
    },
    /// Set an audio clip parameter.
    SetAudioClipProp {
        /// Clip to edit.
        clip: ClipId,
        /// New property value.
        prop: komponist_model::edits::AudioClipProp,
    },
    /// Opt in/out of the optional pitch-preserving Warp mapping.
    SetAudioClipWarp { clip: ClipId, enabled: bool },
    /// `edits::set_clip_loop` (corner drag, inspector).
    SetClipLoop {
        /// The clip.
        clip: ClipId,
        /// Loop length, `None` = off.
        loop_length: Option<Tick>,
        /// New timeline length.
        length: Tick,
    },
    /// `edits::set_clips_muted`.
    SetClipsMuted {
        /// Clips.
        clips: Vec<ClipId>,
        /// Mute.
        muted: bool,
    },
    /// `edits::set_clip_prop` (inspector).
    SetClipProp {
        /// The clip.
        clip: ClipId,
        /// Property.
        prop: ClipProp,
    },
    /// `edits::select_take` (inspector take list).
    SelectTake {
        /// The clip.
        clip: ClipId,
        /// Take index.
        take: usize,
    },
    /// `edits::add_notes` (pencil, double-click).
    AddNotes {
        /// The clip.
        clip: ClipId,
        /// Notes.
        notes: Vec<NewNote>,
    },
    /// `edits::remove_notes` (eraser, double-click).
    RemoveNotes {
        /// The clip.
        clip: ClipId,
        /// Notes.
        notes: Vec<NoteId>,
    },
    /// `edits::move_notes` (drag).
    MoveNotes {
        /// The clip.
        clip: ClipId,
        /// Notes.
        notes: Vec<NoteId>,
        /// Ticks.
        delta: Tick,
        /// Semitones.
        pitch_delta: i32,
    },
    /// `edits::copy_notes` (Alt-drag).
    CopyNotes {
        /// The clip.
        clip: ClipId,
        /// Notes.
        notes: Vec<NoteId>,
        /// Ticks.
        delta: Tick,
        /// Semitones.
        pitch_delta: i32,
    },
    /// `edits::resize_notes` (edge drag, all selected).
    ResizeNotes {
        /// The clip.
        clip: ClipId,
        /// Notes.
        notes: Vec<NoteId>,
        /// Length change in ticks.
        delta: Tick,
    },
    /// `edits::set_notes_velocity` (Ctrl-drag ↕, velocity lane).
    SetNotesVelocity {
        /// The clip.
        clip: ClipId,
        /// Notes.
        notes: Vec<NoteId>,
        /// Change.
        change: VelocityChange,
    },
    /// `edits::set_step` (click, paint, drag ↕, Alt-drag ↕, Shift-click).
    SetStep {
        /// The step clip.
        clip: ClipId,
        /// Row.
        row: usize,
        /// Step 0 .. 32.
        step: usize,
        /// Property.
        prop: StepProp,
    },
    /// `edits::set_step_clip_prop` (Länge, Auflösung, Swing).
    SetStepClipProp {
        /// The step clip.
        clip: ClipId,
        /// Property.
        prop: StepClipProp,
    },
    /// `edits::set_step_row` (row header M / S, rename).
    SetStepRow {
        /// The step clip.
        clip: ClipId,
        /// Row.
        row: usize,
        /// Property.
        prop: StepRowProp,
    },
    /// `edits::step_clip_to_midi` ("Als MIDI-Clip").
    StepClipToMidi(ClipId),
    /// `edits::remove_device`.
    RemoveDevice(DeviceId),
    /// `edits::move_device` (card or insert drag).
    MoveDevice {
        /// The device.
        device: DeviceId,
        /// Target index without it.
        to_index: usize,
    },
    /// `edits::set_device_bypass` (card switch, Alt-click insert).
    SetDeviceBypass {
        /// The device.
        device: DeviceId,
        /// Bypass.
        bypass: bool,
    },
    /// `edits::set_sidechain` (the "SC" popover in a device head): the source and tap of the
    /// device's sidechain input, `None` for the internal input signal.
    SetSidechain {
        /// The device.
        device: DeviceId,
        /// The new sidechain.
        sidechain: Option<komponist_model::Sidechain>,
    },
    /// Strg G: the selected tracks into a new group (`edits::add_bus` with `NewBus::group`).
    GroupSelection,
    /// „In Gruppe zusammenfassen“ (context menu, output menu „Neue Gruppe …“): `tracks` into a
    /// new group, one undo step.
    GroupTracks(Vec<TrackId>),
    /// „Send zu neuem Bus“ / „Neuer Bus …“: a new effect bus with a send from each of `senders`
    /// (−6 dB, post fader), one undo step.
    SendToNewBus(Vec<TrackId>),
    /// `edits::add_bus` for a bus without senders or members (the „Neue Spur“ dialog).
    AddBus(NewBus),
    /// `edits::set_output` (output menu, dropping a track on a group).
    SetOutput {
        /// The tracks.
        tracks: Vec<TrackId>,
        /// Master or a bus.
        output: TrackOutput,
    },
    /// `edits::add_send` („+“ in a send list, „Send an …“).
    AddSend {
        /// The sending track.
        track: TrackId,
        /// The bus.
        target: TrackId,
    },
    /// `edits::set_send` (level drag, pre/post, target). A level change inside a gesture reaches
    /// the engine on the fast path (`Command::SetSendGain`).
    SetSend {
        /// The send.
        send: SendId,
        /// The change.
        prop: SendProp,
    },
    /// `edits::remove_send`.
    RemoveSend(SendId),
    /// Alt-drag on a device head: a copy of `device` (plugin, state; its sidechain where the
    /// source can feed `to`, #7) at `index` of `to`.
    CopyDevice {
        /// The device to copy.
        device: DeviceId,
        /// The chain the copy goes into.
        to: ChannelRef,
        /// Insert index (an effect never goes before the instrument).
        index: usize,
    },
    /// Ctrl D on the device chain: each selected effect copied right behind itself.
    DuplicateDevices,
    /// Ctrl V on the device chain: the copied devices behind the selected device (else at the end)
    /// of the selected track.
    PasteDevices,
    /// Entf on the device chain: remove the selected devices.
    DeleteDevices,
}

/// Where a plugin from the browser goes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DropTarget {
    /// Empty arrangement area or "Neue Spur": an instrument makes a new armed track (S10); an effect
    /// goes onto the selected track.
    NewTrack {
        /// Display index, `None` = append.
        index: Option<usize>,
    },
    /// A track header or lane: an instrument replaces the instrument, an effect is appended.
    Track(TrackId),
    /// A position in a chain (device chain, mixer inserts; 2 px insert line).
    Chain {
        /// The channel.
        channel: ChannelRef,
        /// Insert index.
        index: usize,
    },
}

/// Plugin actions.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginAction {
    /// Open the file browser for a plugin's declared asset slot.
    PickPluginAsset { device: DeviceId, slot: u32 },
    /// Bind a file to a declared slot, or clear that slot.
    SetPluginAsset {
        device: DeviceId,
        slot: u32,
        path: Option<PathBuf>,
    },
    /// Bind dropped files to all slots in the plugin's display order.
    DropPluginAssets {
        device: DeviceId,
        files: Vec<PathBuf>,
    },
    /// Browser double-click, Enter or drop: add `plugin` at `target`.
    Add {
        /// The catalog entry's reference.
        plugin: PluginRef,
        /// Where.
        target: DropTarget,
    },
    /// Refresh the marker snapshot for a visible audio asset graph.
    RequestWaveformMarkers { device: DeviceId, slot: u32 },
    /// Apply one marker operation against the snapshot displayed by a waveform.
    EditWaveformMarkers {
        device: DeviceId,
        slot: u32,
        request: komponist_plugin_api::waveform_markers::Request,
    },
    /// Load a discovered preset into the live CLAP instance.
    ApplyClapPreset {
        device: DeviceId,
        source: komponist_plugin_api::clap_presets::ClapPresetSource,
    },
    /// Choose a file in which to save the current CLAP preset.
    SaveUserPreset(DeviceId),
    /// Choose a CLAP user preset to load into this device.
    LoadUserPreset(DeviceId),
    /// Insert a device with a preset already loaded.
    AddFromPreset {
        plugin: PluginRef,
        source: komponist_plugin_api::clap_presets::ClapPresetSource,
        track: Option<TrackId>,
    },
    /// "Öffnen" on a device card or insert click: open the plugin window (own GUI, or the generic
    /// parameter view for plugins without one).
    OpenWindow(DeviceId),
    /// Close the plugin window.
    CloseWindow(DeviceId),
    /// A plugin window picked in the Window, Dock or "⋯" menu (#95): it comes to the front with
    /// the keyboard focus. Nothing happens if it closed meanwhile.
    FocusWindow(DeviceId),
    /// "Neu laden" on a crashed device.
    Reload(DeviceId),
    /// A drag or a wheel burst on parameter `id` of `device` starts: opens a gesture whose one undo
    /// step is named after the parameter („Cutoff ändern“). `Edit(EndGesture)` closes it (or
    /// `CancelGesture`: the device goes back to the value it had). Without it every
    /// [`PluginAction::SetParam`] is a step of its own.
    BeginParamGesture {
        /// The device.
        device: DeviceId,
        /// Parameter id.
        id: u32,
    },
    /// Generic parameter knob or device-card macro. An undoable change of the device's state
    /// (hexa, internal devices) or of the bridged plugin (its snapshot follows, see
    /// `crate::param_undo`).
    SetParam {
        /// The device.
        device: DeviceId,
        /// Parameter id.
        id: u32,
        /// Plain value.
        value: f64,
    },
    /// Commit a user-entered value through CLAP text conversion, with a numeric fallback.
    SetParamText {
        device: DeviceId,
        id: u32,
        text: String,
        fallback: Option<f64>,
    },
    /// Play an authored MIDI note briefly on the device's track.
    AuditionNote { device: DeviceId, key: u8 },
    /// Start a press-and-release audition from an authored waveform.
    PreviewWaveform {
        device: DeviceId,
        slot: u32,
        key: u8,
        slice_time: Option<f64>,
    },
}

/// Project lifecycle actions.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectAction {
    /// Ctrl N: new empty project (S10), asking to save a dirty one first.
    New,
    /// Ctrl O: file dialog, then open.
    Open,
    /// Open this file (recent list, start screen Enter).
    OpenPath(PathBuf),
    /// Open the audio file replacement browser.
    ReplaceAudio(ClipId),
    /// Replace with a chosen, decoded file.
    ReplaceAudioPath {
        /// Clip to relink.
        clip: ClipId,
        /// Source file to link.
        path: PathBuf,
    },
    /// Ctrl I: choose linked audio files to import.
    ImportAudio,
    /// Link files on an audio track, or create one track per file when target is absent.
    ImportAudioPaths {
        /// Files in import order.
        paths: Vec<PathBuf>,
        /// Existing audio track, or new tracks.
        target: Option<TrackId>,
        /// Grid position, in ticks.
        at: Tick,
    },
    /// Ctrl S: save (file dialog the first time).
    Save,
    /// Ctrl Shift S or "⋯": save under a new path (file dialog).
    SaveAs,
    /// "Sammeln und speichern" (WP-87, project menu): copies every sample the project links from
    /// outside `<project name> Samples/` into that folder next to the project file, points the
    /// links there, then saves. The one action that creates a folder; a project without a file
    /// asks for one first (the browser of "Speichern unter").
    CollectAndSave,
    /// "⋯" › "Zum Startbildschirm": back to S09, asking to save a dirty project first.
    ShowStartScreen,
    /// ⌘Q (macOS) and the app menu's Quit: quit the app, asking to save a dirty project first
    /// (the same path as the window's close button).
    Quit,
    /// Ctrl Shift E or "⋯": open the export dialog with the last settings.
    OpenExportDialog,
    /// The status bar's "Im Ordner zeigen": opens this folder in the file manager.
    ShowInFolder(PathBuf),
    /// "Exportieren" / Enter in the export dialog.
    StartExport(ExportSettings),
    /// Esc during an export: cancel, leave no partial file.
    CancelExport,
    /// S06 "Fortfahren": keep missing devices as placeholders.
    ContinueWithMissing,
    /// S06 "Plugin-Ordner prüfen": open settings at Plugins and rescan.
    CheckPluginFolders,
}

/// Settings actions (S07; applied at once, persisted in `settings.json`).
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsAction {
    /// Driver, device, rate or buffer changed: rebuild the stream (engine untouched).
    SetAudio(StreamConfig),
    /// Explicit capture device selection; None disables capture.
    SetAudioInput(Option<komponist_audio::InputConfig>),
    /// Enable automatic JACK capture and clear the explicit capture-port override.
    SetJackCaptureAutomatic,
    /// Open a capture device, then assign its channels only if opening succeeded.
    ChooseTrackAudioInput {
        track: TrackId,
        input: komponist_model::AudioInput,
        device: komponist_audio::InputConfig,
    },
    /// Encoding of newly recorded WAV files.
    SetCaptureFormat(crate::settings::CaptureFormat),
    /// "Erneut suchen" after the device was lost.
    RetryAudio,
    /// MIDI input switch.
    SetMidiInput {
        /// Port name.
        name: String,
        /// Enabled.
        enabled: bool,
    },
    /// "+ Ordner".
    AddPluginDir {
        /// `true` = CLAP, `false` = VST3.
        clap: bool,
        /// The folder.
        path: PathBuf,
    },
    /// Remove a plugin folder.
    RemovePluginDir {
        /// `true` = CLAP, `false` = VST3.
        clap: bool,
        /// The folder.
        path: PathBuf,
    },
    /// "Neu scannen" (background).
    RescanPlugins,
    /// UI scale override; `None` = follow the display (1× / 2×).
    SetUiScale(Option<f32>),
    /// UI language (S07 "Darstellung"): applied in the same frame, saved in `settings.json`.
    SetLanguage(crate::settings::Language),
    /// The star of a browser row: make the plugin a favourite, or remove it from the favourites.
    ToggleFavorite(PluginRef),
    /// A click (or Space / Enter) on the transport bar's master display: oscilloscope ↔ spectrum
    /// (#34, remembered in the settings).
    ToggleMasterDisplay,
}

/// View actions.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    /// Ctrl 1 / 2 / 3: browser / inspector / dock in or out.
    TogglePanel(Panel),
    /// Ctrl ↑: maximise the dock (hides the arrangement) or restore.
    ToggleDockMaximized,
    /// Tab with a canvas focused: mixer maximised ↔ previous layout (ARCHITECTURE §13.1).
    ToggleMixer,
    /// Ctrl/Command Shift M: toggle mapping mode for the selected track.
    ToggleMacroMapping,
    /// Click on a dock tab.
    SetDockTab(DockTab),
    /// Ctrl Tab: next tab in the focused area.
    NextTab,
    /// Click into a panel.
    Focus(FocusArea),
    /// `+` / `−` (horizontal, around the mouse or the playhead) and Alt `+` / Alt `−` (vertical) in
    /// the focused canvas; `steps` > 0 zooms in, each step by the factor 1.25 (prototype).
    Zoom {
        /// `true` = vertical.
        vertical: bool,
        /// Zoom steps.
        steps: i32,
    },
    /// Z: fit the selection (or everything) into the view.
    ZoomToFit,
    /// F2: the name of the selected clip (exactly one clip selected), else of the first selected
    /// track, goes into an edit field of the view that shows it (arrangement header, mixer
    /// strip or inspector). The edit is the one a double-click makes.
    RenameSelection,
    /// Ctrl `[`: finer grid.
    GridFiner,
    /// Ctrl `]`: coarser grid.
    GridCoarser,
    /// Grid dropdown ("Raster") of the arrangement or of the piano roll.
    SetGrid {
        /// Whose grid.
        target: GridTarget,
        /// The new grid.
        grid: Grid,
    },
    /// Snap toggle of the arrangement or the piano roll.
    ToggleSnap(GridTarget),
    /// A transient message from a view in the status bar ("Ratchet ×3", "Velocity 52"; the
    /// prototype's `app.status`).
    ShowStatus {
        /// German text.
        text: String,
        /// Neutral, success or error.
        kind: StatusKind,
    },
    /// Piano roll tool (1 Zeiger, 2 Stift, 3 Radierer).
    SetTool(Tool),
    /// Step sequencer lane tab (Vel / Wahrsch. / Ratch).
    SetStepLane(StepLane),
    /// A selection change from a canvas, list or header.
    Select(SelectionChange),
    /// Ctrl A: all notes of the open clip (editor focused) or all clips.
    SelectAll,
    /// Esc: close the dialog if one is open, else clear the selection. (Plugin frames are windows
    /// of their own and close with Esc inside them.)
    ClearSelection,
    /// Double-click a clip: open it in the editor tab (piano roll or step sequencer).
    OpenClip(ClipId),
    /// Ctrl F: focus the browser search.
    FocusBrowserSearch,
    /// Browser segment (Alle / Instr. / Effekte / Dateien).
    SetBrowserSegment(BrowserSegment),
    /// Browser search text.
    SetBrowserQuery(String),
    /// Ctrl , : open the settings (at a section).
    OpenSettings(SettingsSection),
    /// Portable About dialog (native AppKit keeps its standard panel).
    OpenAbout,
    /// The "Ansicht" toggle of a plugin frame: the plugin's own GUI or the parameter grid.
    SetPluginView {
        /// The device.
        device: DeviceId,
        /// The view.
        view: PluginView,
    },
    /// "Oben halten" of a plugin frame (remembered per plugin).
    SetPluginKeepOnTop {
        /// The device.
        device: DeviceId,
        /// On or off.
        on: bool,
    },
    /// Click, Enter or Space on the "Neue Spur" box below the tracks: open the dialog for new tracks.
    OpenNewTrackDialog,
    /// Esc / close in a dialog.
    CloseDialog,
    /// Switch computer-keyboard MIDI on or off (ARCHITECTURE §13.2).
    ToggleKeyboardMidi,
    /// Click on a peak value or clip LED: reset it; `all` with Alt (S04).
    ResetMeters {
        /// The strip, `None` = master.
        track: Option<TrackId>,
        /// Alt: all strips.
        all: bool,
    },
    /// Drag on a panel edge: the browser or inspector width, or the dock height, in design px
    /// (clamped to the token range; the dock also to 70 % of the window at layout time).
    ResizePanel {
        /// Which panel.
        panel: Panel,
        /// New width (browser, inspector) or height (dock).
        size: f32,
    },
    /// Double-click on a panel edge: back to the token default (dock: 320).
    ResetPanelSize(Panel),
    /// Double-click on a device head: collapse or expand it; with Alt every device of its chain.
    ToggleDeviceCollapsed {
        /// The device.
        device: DeviceId,
        /// Alt: all devices of the chain follow.
        all: bool,
    },
    /// A click on a device's page switch.
    SetDevicePage {
        /// The device.
        device: DeviceId,
        /// The page.
        page: usize,
    },
    /// Ctrl C on the device chain: remember the selected devices.
    CopyDevices,
    /// ← → on the device chain: select the neighbour of the selected device.
    SelectAdjacentDevice(i32),
    /// Click on an insert slot with an internal device: open the Geräte tab on its track and
    /// scroll the device into the middle.
    RevealDevice(DeviceId),
    /// Click on a name in the chain head: scroll to the device.
    ScrollToDevice(DeviceId),
    /// A parameter of a device was touched (pressed, dragged, clicked): the inspector shows it,
    /// the status line shows name and value while `dragging` (WP-89).
    TouchParam {
        /// The device.
        device: DeviceId,
        /// The parameter's model id.
        param: komponist_model::params::ParamId,
        /// The pointer holds the control and moves it.
        dragging: bool,
    },
}

/// Which editor a grid or snap action is for (the toolbars name theirs, instead of the action
/// depending on the keyboard focus).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GridTarget {
    /// The arrangement.
    Arrangement,
    /// The piano roll.
    PianoRoll,
}

/// How a click combines with the existing selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectMode {
    /// Plain click: replace.
    Replace,
    /// Ctrl-click: toggle the clicked items.
    Toggle,
    /// Shift-click: extend from the anchor to the clicked item (range).
    Extend,
    /// Box selection added to the current one.
    Add,
}

/// A change of the selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelectionChange {
    /// Tracks (headers, mixer strips; selection is synchronous between them).
    Tracks {
        /// Clicked or boxed tracks.
        tracks: Vec<TrackId>,
        /// Combination.
        mode: SelectMode,
    },
    /// Clips in the arrangement.
    Clips {
        /// Clips.
        clips: Vec<ClipId>,
        /// Combination.
        mode: SelectMode,
    },
    /// Notes in the open clip.
    Notes {
        /// Notes.
        notes: Vec<NoteId>,
        /// Combination.
        mode: SelectMode,
    },
    /// The step cursor (row header click selects the row, step click the step).
    Step {
        /// The step clip.
        clip: ClipId,
        /// Row.
        row: usize,
        /// Step, `None` = row only.
        step: Option<usize>,
    },
    /// Devices in a chain.
    Devices {
        /// Devices.
        devices: Vec<DeviceId>,
        /// Combination.
        mode: SelectMode,
    },
}

/// Range of an export (S08 "Bereich").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportRange {
    /// "Ganzer Song": tick 0 to the end of the last clip.
    Song,
    /// "Loop": the loop range (disabled while the loop range is empty).
    Loop,
    /// "Auswahl": the selection's time range (disabled without one).
    Selection,
}

/// Sample format of the exported WAV (S08 "Bittiefe").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BitDepth {
    /// 16 bit PCM.
    Int16,
    /// 24 bit PCM.
    Int24,
    /// 32 bit float.
    Float32,
}

/// The export dialog's settings (S08). The last used ones are persisted in the settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportSettings {
    /// Range.
    pub range: ExportRange,
    /// "Nachlauf" in seconds (default 2.0), rendered after the range so reverbs ring out.
    pub tail_seconds: f32,
    /// Sample rate in Hz (44100 / 48000 / 96000, default 48000).
    pub sample_rate: u32,
    /// Bit depth (default 24 bit).
    pub bit_depth: BitDepth,
    /// Normalise the peak to −1.0 dBFS.
    pub normalize: bool,
    /// TPDF dither for 16 and 24 bit.
    pub dither: bool,
    /// Target folder.
    pub folder: PathBuf,
    /// File name including `.wav` (default: project name).
    pub file_name: String,
}
