//! View state of the app, per panel (ARCHITECTURE §9.3).
//!
//! Everything here is UI state: not part of the document, not undone, not saved with the project.
//! Selection, focus, zoom, scroll, open panels and dialogs live here. Sizes are design px (= egui
//! points); their defaults and ranges come from `komponist-tokens` (`size.*`), never literals.
//!
//! Thread: UI thread only.

use std::collections::BTreeSet;
use std::time::Instant;

use komponist_model::{
    ClipContent, ClipId, DeviceId, Grid, NoteId, PPQ, Project, Tick, TickRange, TrackId,
};
use komponist_plugins::MissingPlugin;
use komponist_tokens::size;

use crate::actions::{Action, ExportSettings};
use crate::metrics;
use crate::settings::Favorites;

/// Which panel has keyboard focus. Shortcuts are looked up per area (`Keymap::lookup`); the focused
/// panel shows a 1 px accent line at the top of its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FocusArea {
    /// Transport bar.
    Transport,
    /// Browser panel.
    Browser,
    /// Arrangement (track headers and timeline).
    #[default]
    Arrangement,
    /// Inspector panel.
    Inspector,
    /// Piano roll in the dock's editor tab.
    PianoRoll,
    /// Step sequencer in the dock's editor tab.
    StepSequencer,
    /// Device chain in the dock's "Geräte" tab.
    Devices,
    /// Mixer in the dock's "Mixer" tab.
    Mixer,
    /// A modal dialog (settings, export, missing plugins).
    Dialog,
}

impl FocusArea {
    /// Whether the area is a canvas. Tab toggles arrangement ↔ mixer only from a canvas; elsewhere
    /// Tab moves focus between widgets (ARCHITECTURE §13.1).
    pub fn is_canvas(self) -> bool {
        matches!(
            self,
            FocusArea::Arrangement
                | FocusArea::PianoRoll
                | FocusArea::StepSequencer
                | FocusArea::Mixer
        )
    }
}

/// The whole view state.
#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    /// Start screen (S09) or a project.
    pub screen: Screen,
    /// Keyboard focus.
    pub focus: FocusArea,
    /// Panel visibility and sizes (layout 2a with dock).
    pub layout: PanelLayout,
    /// What is selected.
    pub selection: Selection,
    /// Arrangement view.
    pub arrangement: ArrangementView,
    /// Which clip the editor tab shows.
    pub editor: EditorView,
    /// Piano roll view.
    pub piano_roll: PianoRollView,
    /// Step sequencer view.
    pub step_seq: StepSeqView,
    /// Mixer view.
    pub mixer: MixerView,
    /// Device chain view.
    pub devices: DevicesView,
    /// Browser panel.
    pub browser: BrowserView,
    /// Inspector panel.
    pub inspector: InspectorView,
    /// Start screen (S09).
    pub start: StartView,
    /// The open modal dialog.
    pub dialog: Option<Dialog>,
    /// Open plugin frame windows.
    pub plugin_windows: Vec<PluginWindow>,
    /// Message in the status bar.
    pub status: Option<StatusMessage>,
    /// Computer keyboard MIDI active (ARCHITECTURE §13.2).
    pub keyboard_midi: bool,
}

impl UiState {
    /// The initial state: start screen, default panel sizes from the tokens, nothing selected.
    pub fn new() -> UiState {
        UiState {
            screen: Screen::Start,
            focus: FocusArea::default(),
            layout: PanelLayout::default(),
            selection: Selection::default(),
            arrangement: ArrangementView::default(),
            editor: EditorView::default(),
            piano_roll: PianoRollView::default(),
            step_seq: StepSeqView::default(),
            mixer: MixerView::default(),
            devices: DevicesView::default(),
            browser: BrowserView::default(),
            inspector: InspectorView::default(),
            start: StartView::default(),
            dialog: None,
            plugin_windows: Vec::new(),
            status: None,
            keyboard_midi: false,
        }
    }

    /// The editor kind the dock's editor tab shows for the open clip in `project`: the step
    /// sequencer for a step clip, else the piano roll (also without a clip).
    pub fn editor_area(&self, project: &Project) -> FocusArea {
        let step = self
            .editor
            .clip
            .and_then(|id| project.find_clip(id))
            .is_some_and(|(_, clip)| matches!(clip.content, ClipContent::Step(_)));
        if step {
            FocusArea::StepSequencer
        } else {
            FocusArea::PianoRoll
        }
    }

    /// The focus area of the dock's active tab ([`UiState::editor_area`] for the editor tab).
    pub fn dock_area(&self, project: &Project) -> FocusArea {
        match self.layout.dock_tab {
            DockTab::Editor => self.editor_area(project),
            DockTab::Devices => FocusArea::Devices,
            DockTab::Mixer => FocusArea::Mixer,
        }
    }
}

impl FocusArea {
    /// Whether the area lives in the dock.
    pub fn in_dock(self) -> bool {
        matches!(
            self,
            FocusArea::PianoRoll | FocusArea::StepSequencer | FocusArea::Devices | FocusArea::Mixer
        )
    }
}

impl Default for PanelLayout {
    /// Everything open at the token defaults, editor tab, not maximised.
    fn default() -> PanelLayout {
        PanelLayout {
            browser_open: true,
            browser_width: size::browser::DEFAULT,
            inspector_open: true,
            inspector_width: size::inspector::DEFAULT,
            dock_open: true,
            dock_height: size::dock::DEFAULT,
            dock_maximized: false,
            dock_tab: DockTab::Editor,
            before_mixer: None,
        }
    }
}

impl Default for ArrangementView {
    /// The prototype's zoom (72 px per bar), scrolled to the start, grid one bar, snap on.
    fn default() -> ArrangementView {
        ArrangementView {
            px_per_bar: metrics::arrangement::PX_PER_BAR,
            scroll_tick: 0.0,
            scroll_y: 0.0,
            v_zoom: 1.0,
            grid: Grid::Bar,
            snap: true,
        }
    }
}

impl Default for PianoRollView {
    /// Pointer tool, 1/16 grid with snap, keys `size.key` high, new notes 1/8 long, velocity lane
    /// shown.
    fn default() -> PianoRollView {
        PianoRollView {
            tool: Tool::Pointer,
            grid: Grid::Sixteenth,
            snap: true,
            px_per_bar: metrics::piano_roll::PX_PER_BAR,
            scroll_tick: 0.0,
            key_height: size::key::DEFAULT,
            top_key: metrics::piano_roll::TOP_KEY,
            note_length: Tick(PPQ / 2),
            velocity_lane: true,
        }
    }
}

impl Default for UiState {
    /// [`UiState::new`].
    fn default() -> UiState {
        UiState::new()
    }
}

/// Top-level screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Screen {
    /// S09: no project open.
    #[default]
    Start,
    /// A project is open (S01 layout).
    Project,
}

/// Collapsible side panels and the dock (Ctrl 1 / 2 / 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    /// Browser (left).
    Browser,
    /// Inspector (right).
    Inspector,
    /// Dock (bottom).
    Dock,
}

/// Tabs of the dock ("Editor · Geräte · Mixer · +").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DockTab {
    /// Piano roll or step sequencer for the open clip; titled with the clip name and colour.
    #[default]
    Editor,
    /// Device chain of the selected track.
    Devices,
    /// Mixer.
    Mixer,
}

/// Layout 2a: sizes in design px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelLayout {
    /// Browser visible.
    pub browser_open: bool,
    /// Browser width (`size.browser`: default 260, 200 ..= 420).
    pub browser_width: f32,
    /// Inspector visible.
    pub inspector_open: bool,
    /// Inspector width (`size.inspector`: default 280, 240 ..= 360).
    pub inspector_width: f32,
    /// Dock expanded (collapsed = the 28 px tab bar only).
    pub dock_open: bool,
    /// Dock height (`size.dock`: default 320, 200 … 70 % of the window; double-click = default).
    pub dock_height: f32,
    /// Dock maximised (arrangement hidden; Ctrl ↑, Tab for the mixer).
    pub dock_maximized: bool,
    /// Active dock tab.
    pub dock_tab: DockTab,
    /// Tab and maximise state to return to when Tab leaves the mixer.
    pub before_mixer: Option<(DockTab, bool)>,
}

/// The current selection. Track selection is shared by arrangement, mixer and inspector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// Selected tracks.
    pub tracks: BTreeSet<TrackId>,
    /// Anchor of Shift-range selection of tracks.
    pub anchor_track: Option<TrackId>,
    /// Selected clips.
    pub clips: BTreeSet<ClipId>,
    /// Selected notes (of the open clip).
    pub notes: BTreeSet<NoteId>,
    /// Step sequencer cursor.
    pub step: Option<StepCursor>,
    /// Selected devices.
    pub devices: BTreeSet<DeviceId>,
    /// Time range of the selection (span of the selected clips, or a ruler selection); export range
    /// "Auswahl".
    pub time_range: Option<TickRange>,
}

/// Selected row and step in the step sequencer ("STEP n · row").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepCursor {
    /// The step clip.
    pub clip: ClipId,
    /// Row.
    pub row: usize,
    /// Step, `None` = row selected only.
    pub step: Option<usize>,
}

/// Arrangement view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArrangementView {
    /// Horizontal zoom in design px per bar (prototype `barW`, 24 ..= 160 at 4/4).
    pub px_per_bar: f32,
    /// Horizontal scroll: tick at the left edge of the timeline.
    pub scroll_tick: f64,
    /// Vertical scroll in design px.
    pub scroll_y: f32,
    /// Vertical zoom factor applied to track heights (Alt + / Alt −), 1.0 = stored heights.
    pub v_zoom: f32,
    /// Snap grid.
    pub grid: Grid,
    /// Snap on (Shift while dragging disables it temporarily).
    pub snap: bool,
}

/// The editor tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EditorView {
    /// The open clip; the editor kind follows its content (piano roll for MIDI, step sequencer for
    /// steps). `None` = "Kein Clip".
    pub clip: Option<ClipId>,
}

/// Piano roll tools (1 / 2 / 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tool {
    /// "Zeiger": select, move, resize; double-click adds or deletes.
    #[default]
    Pointer,
    /// "Stift": click adds a note of [`PianoRollView::note_length`], drag draws its length.
    Pencil,
    /// "Radierer": click or drag deletes.
    Eraser,
}

/// Piano roll view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PianoRollView {
    /// Active tool (an empty clip preselects the pencil).
    pub tool: Tool,
    /// Grid ("Raster").
    pub grid: Grid,
    /// Snap on.
    pub snap: bool,
    /// Horizontal zoom in design px per bar.
    pub px_per_bar: f32,
    /// Horizontal scroll: clip-local tick at the left edge.
    pub scroll_tick: f64,
    /// Key height in design px (`size.key`: default 12, 4 ..= 24; Alt + / Alt −).
    pub key_height: f32,
    /// MIDI key at the top edge.
    pub top_key: u8,
    /// Length of new pencil notes ("Länge"), default one 1/8 (prototype `lastLen`).
    pub note_length: Tick,
    /// Velocity lane (64 px) visible.
    pub velocity_lane: bool,
}

/// Lane below the step grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StepLane {
    /// "Vel".
    #[default]
    Velocity,
    /// "Wahrsch.".
    Probability,
    /// "Ratch".
    Ratchet,
}

/// Step sequencer view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StepSeqView {
    /// Lane tab.
    pub lane: StepLane,
}

/// Mixer view.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MixerView {
    /// Horizontal scroll in design px.
    pub scroll_x: f32,
}

/// Device chain view.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DevicesView {
    /// Horizontal scroll in design px.
    pub scroll_x: f32,
}

/// Browser segments ("Alle · Instr. · Effekte · Dateien").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BrowserSegment {
    /// All.
    #[default]
    All,
    /// Instruments.
    Instruments,
    /// Effects.
    Effects,
    /// Files (project takes and exports).
    Files,
}

/// Browser panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserView {
    /// Segment.
    pub segment: BrowserSegment,
    /// Search text (Ctrl F; typing filters at once, Esc clears).
    pub query: String,
    /// Keyboard-highlighted row in the current list (↑ ↓, Enter adds to the selected track).
    pub highlighted: Option<usize>,
    /// Collapsed folder rows by section and label ("Effekte/EQ und Filter",
    /// `views::browser::model::folder_key`); the browser closes all folders when it first shows.
    pub collapsed: BTreeSet<String>,
    /// The favourites (starred rows), mirrored from [`crate::settings::Settings::favorites`];
    /// changed through `SettingsAction::ToggleFavorite`.
    pub favorites: Favorites,
}

/// Inspector sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InspectorSection {
    /// "Clip".
    Clip,
    /// "Spur".
    Track,
    /// "Geräte".
    Devices,
    /// "Kanal".
    Channel,
    /// "Projekt" (shown when nothing is selected).
    Project,
}

/// Inspector panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InspectorView {
    /// Collapsed sections.
    pub collapsed: BTreeSet<InspectorSection>,
}

/// Start screen (S09).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartView {
    /// Filter text of the recent list.
    pub filter: String,
    /// Selected row of the filtered recent list (the last project is preselected).
    pub selected: usize,
}

/// Sections of the settings dialog (S07 navigation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsSection {
    /// "Audio".
    #[default]
    Audio,
    /// "MIDI".
    Midi,
    /// "Plugins".
    Plugins,
    /// "Darstellung".
    Display,
    /// "Tastatur".
    Keyboard,
}

/// A modal dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// S07 settings at a section.
    Settings(SettingsSection),
    /// S08 export.
    Export(ExportDialog),
    /// S06 missing plugins after opening a project.
    MissingPlugins(Vec<MissingPlugin>),
}

/// State of the export dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportDialog {
    /// Settings being edited (prefilled with the last used ones).
    pub settings: ExportSettings,
    /// Progress while rendering, `None` while configuring.
    pub progress: Option<ExportProgress>,
    /// Where the export is: configuring, waiting for devices, rendering.
    pub stage: ExportStage,
}

/// The stage of the export the dialog shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportStage {
    /// Configuring ("Exportieren" not pressed yet).
    #[default]
    Settings,
    /// Waiting until every device of the project runs, or "Ohne sie exportieren".
    Devices,
    /// The worker renders the file.
    Rendering,
}

/// Progress of a running export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportProgress {
    /// Rendered fraction `0.0 ..= 1.0`.
    pub fraction: f32,
    /// Peak level so far in dBFS ("Spitzenpegel").
    pub peak_db: f32,
}

/// What a plugin frame window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginView {
    /// The plugin's own GUI (X11 child of the frame).
    Gui,
    /// The generic parameter grid ("Ansicht" toggle, plugins without GUI, pisynth).
    Params,
}

/// An open plugin frame window (S05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginWindow {
    /// The device.
    pub device: DeviceId,
    /// What it shows.
    pub view: PluginView,
    /// Size of the plugin area below the 28 px header, physical px (from the plugin).
    pub width: u32,
    /// Height of the plugin area, physical px.
    pub height: u32,
    /// "Oben halten": the frame stays above the main window.
    pub keep_on_top: bool,
    /// Where the frame opens (outer position in points), from the last time this plugin's frame
    /// closed.
    pub pos: Option<(i32, i32)>,
}

impl PluginWindow {
    /// A window for `device` showing `view`, with the plugin's remembered "Oben halten" and
    /// position.
    pub fn new(device: DeviceId, view: PluginView, memory: Option<&crate::settings::FrameMemory>) -> Self {
        PluginWindow {
            device,
            view,
            width: 0,
            height: 0,
            keep_on_top: memory.is_some_and(|m| m.keep_on_top),
            pos: memory.and_then(|m| m.pos),
        }
    }
}

/// Kind of a status bar message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusKind {
    /// Neutral information.
    Info,
    /// Success (green).
    Success,
    /// Error (red).
    Error,
}

/// A message in the status bar.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusMessage {
    /// German UI text, verbatim from the screens where specified.
    pub text: String,
    /// Kind.
    pub kind: StatusKind,
    /// When it appeared.
    pub since: Instant,
    /// Optional action link ("Im Ordner zeigen") with its label.
    pub link: Option<(String, Box<Action>)>,
}
