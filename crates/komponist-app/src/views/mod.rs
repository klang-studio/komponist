//! The views the shell hosts, and the seam where WP-40 … WP-44 plug them in.
//!
//! The shell (WP-30) owns the frame: it lays out the panels, draws the panel headers (with the
//! focus accent line), the dock's tab strip, the empty states, and decides which view shows. A
//! view fills a rect it is given and reports what the user did as [`Action`]s through
//! [`ViewCtx::send`]; it never changes the document. It may change its own view state in
//! [`crate::ui_state::UiState`] (scroll, zoom while dragging) directly.
//!
//! | Slot | Shown in | Owner |
//! |---|---|---|
//! | [`Views::arrangement`] | centre, above the dock | WP-40 (`views/arrangement/`) |
//! | [`Views::piano_roll`] | dock, editor tab, MIDI clip | WP-41 (`views/piano_roll/`) |
//! | [`Views::step_seq`] | dock, editor tab, step clip | WP-42 (`views/step_seq/`) |
//! | [`Views::mixer`] | dock, mixer tab | WP-43 (`views/mixer/`) |
//! | [`Views::inspector`] | right panel | WP-43 (`views/inspector/`) |
//! | [`Views::browser`] | left panel | WP-44 (`views/browser/`) |
//! | [`Views::devices`] | dock, "Geräte" tab | WP-44 (`views/devices/`) |
//!
//! Until a WP lands, its slot holds a [`Placeholder`] that shows the view's name. A WP replaces
//! the placeholder in [`Views::new`] with its own type implementing [`View`].

pub mod arrangement;
pub mod piano_roll;
pub mod step_seq;

use std::sync::Arc;

use egui::{Rect, Ui};
use komponist_model::Project;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::{fonts, px, widgets};

use crate::actions::Action;
use crate::engine_sync::EngineSync;
use crate::keymap::Keymap;
use crate::meters::Meters;
use crate::ui_state::UiState;

/// What a view gets for one frame.
pub struct ViewCtx<'a> {
    /// The project (read-only; changes go through [`Action::Edit`]).
    pub project: &'a Arc<Project>,
    /// The view state; a view may change its own scroll and zoom fields directly.
    pub ui: &'a mut UiState,
    /// Engine telemetry (playhead, transport state), device status, track meters.
    pub engine: &'a EngineSync,
    /// Meter ballistics of the master and the tracks (updated once per frame by the shell).
    pub meters: &'a mut Meters,
    /// The shortcut table, for the shortcut column of menus and tooltips.
    pub keymap: &'a Keymap,
    /// Whether this view's area has the keyboard focus (the shell draws the accent line).
    pub focused: bool,
    /// One-shot requests from the dispatcher to views.
    pub requests: Requests,
    actions: &'a mut Vec<Action>,
}

/// One-shot requests from the dispatcher to the views, valid for one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Requests {
    /// Ctrl F: the browser puts the keyboard focus into its search field.
    pub focus_browser_search: bool,
}

impl<'a> ViewCtx<'a> {
    /// A context for one view; `actions` collects what it sends.
    pub fn new(
        project: &'a Arc<Project>,
        ui: &'a mut UiState,
        engine: &'a EngineSync,
        meters: &'a mut Meters,
        keymap: &'a Keymap,
        actions: &'a mut Vec<Action>,
    ) -> ViewCtx<'a> {
        ViewCtx {
            project,
            ui,
            engine,
            meters,
            keymap,
            focused: false,
            requests: Requests::default(),
            actions,
        }
    }

    /// Reports a user intention; the shell dispatches it after the frame's views ran.
    pub fn send(&mut self, action: Action) {
        self.actions.push(action);
    }
}

/// A view the shell hosts.
pub trait View {
    /// The content of the panel header right of the title (the arrangement's grid, snap and
    /// zoom controls; the browser's and inspector's header actions), in `ui` laid out right of
    /// the title. Nothing by default.
    fn header(&mut self, _ui: &mut Ui, _ctx: &mut ViewCtx<'_>) {}

    /// Fills `ui.max_rect()` (the panel below its header). The shell has painted the panel
    /// background.
    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>);
}

/// A view that is not implemented yet: its name, centred, in `text.3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    name: &'static str,
}

impl Placeholder {
    /// A placeholder showing `name`.
    pub const fn new(name: &'static str) -> Placeholder {
        Placeholder { name }
    }

    /// The name it shows.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl View for Placeholder {
    fn show(&mut self, ui: &mut Ui, _ctx: &mut ViewCtx<'_>) {
        centered_text(ui, ui.max_rect(), self.name, None);
    }
}

/// Paints `title` (and `detail` below it) centred in `rect`: `title` in `type.ui` medium
/// `text.2`, `detail` in `type.ui` `text.3`, 6 px apart (the dock's empty state, placeholders).
/// AccessKit reads them as labels.
pub fn centered_text(ui: &mut Ui, rect: Rect, title: &str, detail: Option<&str>) {
    let ppp = ui.pixels_per_point();
    let style = typography::UI;
    let title_g = widgets::galley(
        ui,
        title,
        widgets::font_at(&style, widgets::metrics::MEDIUM),
        style.line_height,
    );
    let detail_g =
        detail.map(|d| widgets::galley(ui, d, fonts::font_id(&style), style.line_height));
    let gap = komponist_tokens::space::SPACE_1_5;
    let height = title_g.size().y + detail_g.as_ref().map_or(0.0, |g| gap + g.size().y);
    let mut y = rect.center().y - height / 2.0;
    let mut place = |ui: &mut Ui, galley: Arc<egui::Galley>, text: &str, color: [u8; 4]| {
        let x = rect.center().x - galley.size().x / 2.0;
        let pos = egui::pos2(px::snap(x, ppp), px::snap(y, ppp));
        let r = Rect::from_min_size(pos, galley.size());
        y += galley.size().y + gap;
        let response = ui.allocate_rect(r, egui::Sense::hover());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
        ui.painter().galley(pos, galley, color32(color));
    };
    place(ui, title_g, title, color::TEXT_2);
    if let (Some(g), Some(d)) = (detail_g, detail) {
        place(ui, g, d, color::TEXT_3);
    }
}

/// The views of the shell, one slot each (see the module docs).
pub struct Views {
    /// Centre: arrangement (WP-40).
    pub arrangement: Box<dyn View>,
    /// Dock editor tab, MIDI clips (WP-41).
    pub piano_roll: Box<dyn View>,
    /// Dock editor tab, step clips (WP-42).
    pub step_seq: Box<dyn View>,
    /// Dock mixer tab (WP-43).
    pub mixer: Box<dyn View>,
    /// Right panel (WP-43).
    pub inspector: Box<dyn View>,
    /// Left panel (WP-44).
    pub browser: Box<dyn View>,
    /// Dock "Geräte" tab (WP-44).
    pub devices: Box<dyn View>,
}

impl Views {
    /// The views of this build: placeholders until WP-40 … WP-44 replace them here.
    pub fn new() -> Views {
        Views {
            arrangement: Box::new(arrangement::Arrangement::new()),
            piano_roll: Box::new(piano_roll::PianoRoll::new()),
            step_seq: Box::new(step_seq::StepSequencer::new()),
            mixer: Box::new(Placeholder::new("Mixer")),
            inspector: Box::new(Placeholder::new("Inspector")),
            browser: Box::new(Placeholder::new("Browser")),
            devices: Box::new(Placeholder::new("Geräte")),
        }
    }
}

impl Default for Views {
    fn default() -> Views {
        Views::new()
    }
}
