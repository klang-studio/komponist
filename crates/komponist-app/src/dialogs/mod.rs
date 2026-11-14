//! Dialogs and the start screen (WP-52): S06 missing plugins, S07 settings, S09 start screen,
//! S10's project creation, the unsaved-changes prompt and a file browser for open, save and
//! "+ Ordner".
//!
//! Each dialog is a pure view: a model (plain data the app computes) goes in, the user's
//! intentions come out ([`missing_plugins::Output`], [`settings::Output`], [`start::Output`],
//! [`file::Output`], [`new_project::Answer`]), and the app applies them (`App::draw_dialogs`,
//! `App::draw_start`). Tests render the views with hand-made models.
//!
//! The contract's [`crate::ui_state::Dialog`] holds the settings, export and missing-plugins
//! dialogs. The file browser and the unsaved-changes prompt have no variant there yet (contract
//! change request in the WP-52 report); they are app-private [`Overlay`]s drawn above everything,
//! with the keyboard focus on [`FocusArea::Dialog`] while they are open.
//!
//! **File dialogs:** komponist draws its own small file browser ([`file`]) instead of using
//! native dialogs (`rfd`): `rfd` on Linux needs either a running xdg-desktop-portal (absent under
//! Xvfb and on minimal window managers) or GTK 3 with a nested main loop that blocks the egui
//! frame, and a native dialog is invisible to `egui_kittest` and the AccessKit tree, so the
//! Ctrl O / Ctrl S flows could not be tested. The own browser needs no new dependency, uses the
//! design tokens and keyboard rules of the other dialogs, and works under Xvfb.

pub mod file;
pub mod format;
pub mod metrics;
pub mod missing_plugins;
pub mod new_project;
pub mod settings;
pub mod start;

use std::path::PathBuf;
use std::sync::Arc;

use egui::epaint::CornerRadius;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Color32, Context, FontId, Galley, Id, Rect, Response, Role, Sense, Stroke, Ui, WidgetInfo,
    pos2, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{Icon, color, radius, shadow};
use komponist_ui::state::{self, State};
use komponist_ui::theme::{corner_radius, egui_shadow};
use komponist_ui::widgets::button::{self, Appearance, Button, Kind};
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, px};

use crate::ui_state::FocusArea;

use metrics::dialog as m;

/// App-private dialog state: what the contract's `UiState` has no place for.
#[derive(Default)]
pub(crate) struct DialogState {
    /// The file browser or the unsaved-changes prompt, above every other dialog.
    pub(crate) overlay: Option<Overlay>,
    /// The focus to give back when the overlay closes.
    pub(crate) overlay_focus: Option<FocusArea>,
    /// What continues after the unsaved-changes prompt or the save dialog it opened.
    pub(crate) pending: Option<Pending>,
    /// The S06 check waits for the running plugin scan.
    pub(crate) missing_check: bool,
    /// Closing the window was confirmed (saved or discarded): the next close request passes.
    pub(crate) quit_confirmed: bool,
    /// Close the window in the next frame.
    pub(crate) close_now: bool,
    /// The folder the file browser showed last.
    pub(crate) last_dir: Option<PathBuf>,
    /// Device lists, MIDI activity and scroll targets of the settings dialog.
    pub(crate) settings: settings::Cache,
    /// The recent projects' summaries for the start screen.
    pub(crate) start: start::Cache,
}

/// A dialog without a [`crate::ui_state::Dialog`] variant.
#[derive(Debug)]
pub(crate) enum Overlay {
    /// Open, save or choose a folder.
    File(file::FileBrowser),
    /// "Änderungen speichern?" before the open project is replaced.
    Unsaved,
}

/// A step that waits for the unsaved-changes prompt (or for the save it started).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pending {
    /// Ctrl N: a new empty project.
    New,
    /// "Aus Vorlage": the template project.
    Template,
    /// Ctrl O: the open dialog.
    Open,
    /// Open this file.
    OpenPath(PathBuf),
    /// Close the window (the window's close button).
    Quit,
}

// ------------------------------------------------------------------------------------------------
// Painting helpers shared by the dialogs and the start screen
// ------------------------------------------------------------------------------------------------

/// A modal dialog over the scrim (`color.overlay.scrim`): `fill`, `radius.l`, `shadow.dialog`
/// plus the 1 px `shadow.edge`, no padding. Centred in the window. `should_close()` of the result
/// says Esc or a click on the scrim.
pub(crate) fn modal<R>(
    ctx: &Context,
    name: &str,
    fill: Color32,
    content: impl FnOnce(&mut Ui) -> R,
) -> egui::ModalResponse<R> {
    let frame = egui::Frame::new()
        .fill(fill)
        .corner_radius(corner_radius(radius::L))
        .shadow(egui_shadow(&shadow::DIALOG))
        .stroke(Stroke::new(
            shadow::EDGE.spread,
            color32(shadow::EDGE.color),
        ));
    egui::Modal::new(Id::unique(("komponist_dialog", name)))
        .frame(frame)
        .backdrop_color(color32(color::OVERLAY_SCRIM))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            content(ui)
        })
}

/// The largest size a dialog of `size` may take in the window (room of [`m::MARGIN`] around).
pub(crate) fn fit(ctx: &Context, size: egui::Vec2) -> egui::Vec2 {
    let room = ctx.content_rect().size() - vec2(2.0 * m::MARGIN, 2.0 * m::MARGIN);
    size.min(room.max(vec2(1.0, 1.0)))
}

/// The header bar of S07 and S08: 44 high, `surface.3`, the title in `type.title`, the close
/// button at the right. Returns whether the close button was clicked.
pub(crate) fn header(ui: &mut Ui, width: f32, title: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(width, m::HEADER_HEIGHT), Sense::hover());
    let r = radius::L as u8;
    ui.painter().rect_filled(
        rect,
        CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        },
        color32(color::SURFACE_3),
    );
    let g = w::styled_galley(ui, title, &typography::TITLE);
    let pos = w::text_pos(
        rect,
        &g,
        rect.left() + m::HEADER_PADDING_LEFT,
        ui.pixels_per_point(),
    );
    let title_rect = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color32(color::TEXT_1));
    label_node(ui, title_rect, title, Role::Label);
    let close = Rect::from_min_size(
        pos2(
            rect.right() - m::HEADER_PADDING_RIGHT - m::CLOSE,
            rect.center().y - m::CLOSE / 2.0,
        ),
        vec2(m::CLOSE, m::CLOSE),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(close));
    child
        .add_sized(
            close.size(),
            Button::icon(Icon::X, CLOSE).frameless(3).shortcut("Esc"),
        )
        .clicked()
}

/// Accessible name of the × button of a dialog.
pub const CLOSE: &str = "Schließen";

/// A label node for painted text (AccessKit, tests).
pub(crate) fn label_node(ui: &Ui, rect: Rect, text: &str, role: Role) -> Response {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("komponist_label", text)),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(role, true, text));
    response
}

/// `text` laid out in `font` with `line_height`, wrapped at `wrap` if given.
pub(crate) fn galley(
    ui: &Ui,
    text: &str,
    font: FontId,
    line_height: f32,
    color: Color32,
    wrap: Option<f32>,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            line_height: Some(line_height),
            color,
            ..Default::default()
        },
    );
    if let Some(width) = wrap {
        job.wrap = TextWrapping {
            max_width: width,
            ..Default::default()
        };
    }
    ui.painter().layout_job(job)
}

/// A type style's text in `color`, wrapped at `wrap` if given.
pub(crate) fn styled(
    ui: &Ui,
    text: &str,
    style: &TypeStyle,
    color: Color32,
    wrap: Option<f32>,
) -> Arc<Galley> {
    let mut job = fonts::layout_job(text, style, color);
    if let Some(width) = wrap {
        job.wrap = TextWrapping {
            max_width: width,
            ..Default::default()
        };
    }
    ui.painter().layout_job(job)
}

/// A type style's text on one line, cut with "…" at `max_width` (`text-overflow: ellipsis`).
pub(crate) fn styled_line(
    ui: &Ui,
    text: &str,
    style: &TypeStyle,
    color: Color32,
    max_width: f32,
) -> Arc<Galley> {
    let mut job = fonts::layout_job(text, style, color);
    job.wrap = TextWrapping {
        max_width: max_width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// Adds a single-line text field with the accent ring while it has the focus (the browser search's
/// active state in S05: `inset 0 0 0 1px accent`).
pub(crate) fn text_field(ui: &mut Ui, edit: egui::TextEdit<'_>) -> Response {
    ui.scope(|ui| {
        ui.visuals_mut().selection.stroke =
            Stroke::new(komponist_tokens::line::WIDTH, color32(color::ACCENT));
        ui.add(edit)
    })
    .inner
}

/// Allocates and paints `galley` at the cursor (a label with an AccessKit node).
pub(crate) fn paint_text(ui: &mut Ui, galley: Arc<Galley>, name: &str) -> Response {
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    let pos = px::snap_rect(rect, ui.pixels_per_point()).min;
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
    label_node(ui, rect, name, Role::Label)
}

/// A section label (`type.label`: 10 / 600, capitals, `text.3`): "AUDIO", "SYSTEM".
pub(crate) fn section_label(ui: &mut Ui, text: &str) -> Response {
    let g = styled(
        ui,
        text,
        &typography::LABEL,
        fonts::style_color(&typography::LABEL),
        None,
    );
    paint_text(ui, g, text)
}

/// A 1 px divider in `line.1` across `width`.
pub(crate) fn divider(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    px::hline(
        ui.painter(),
        rect.x_range(),
        rect.top(),
        color32(color::LINE_1),
    );
}

/// A filled status dot of diameter `d` centred at `center`.
pub(crate) fn dot(ui: &Ui, center: egui::Pos2, d: f32, color: Color32) {
    let c = px::pixel_center(center, ui.pixels_per_point());
    ui.painter().circle_filled(c, d / 2.0, color);
}

/// A button of the dialogs and the start screen, sized by the screen markup rather than the
/// design system's 24 px button (`height:28px; padding:0 12px` in S06, 32 px on the start screen).
pub(crate) struct ActionButton<'a> {
    /// Text.
    pub label: &'a str,
    /// Primary (accent) or secondary.
    pub kind: Kind,
    /// Surface the button rests on: a secondary button is one step lighter (S06 on `surface.3`
    /// is `surface.4`).
    pub ground: usize,
    /// Height.
    pub height: f32,
    /// Horizontal padding.
    pub padding_x: f32,
    /// Font of the label.
    pub font: FontId,
    /// Text after the label: a shortcut ("Strg N") or a hint ("Beat-Skizze"), and its colour
    /// (`None` = the label's).
    pub trailing: Option<(&'a str, FontId, Option<Color32>)>,
    /// Fixed width with the trailing text at the right edge (start screen), else as wide as the
    /// text.
    pub width: Option<f32>,
}

impl<'a> ActionButton<'a> {
    /// A 28 px dialog button in `type.ui` (secondary 500, primary 600).
    pub fn dialog(label: &'a str, kind: Kind, ground: usize) -> Self {
        let font = match kind {
            Kind::Primary => w::font_at(&typography::UI, wm::SEMIBOLD),
            Kind::Secondary => w::font_at(&typography::UI, wm::MEDIUM),
        };
        ActionButton {
            label,
            kind,
            ground,
            height: m::BUTTON_HEIGHT,
            padding_x: m::BUTTON_PADDING_X,
            font,
            trailing: None,
            width: None,
        }
    }

    /// Adds the button at the cursor.
    pub fn show(self, ui: &mut Ui) -> Response {
        let ppp = ui.pixels_per_point();
        let line = typography::UI.line_height;
        let text = galley(
            ui,
            self.label,
            self.font.clone(),
            line,
            Color32::PLACEHOLDER,
            None,
        );
        let trailing = self.trailing.as_ref().map(|(t, f, c)| {
            (
                galley(ui, t, f.clone(), line, Color32::PLACEHOLDER, None),
                *c,
            )
        });
        let natural = text.size().x.ceil()
            + trailing
                .as_ref()
                .map_or(0.0, |(g, _)| wm::SHORTCUT_GAP + g.size().x.ceil())
            + 2.0 * self.padding_x;
        let width = self.width.unwrap_or(natural);
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, self.height), w::sense(ui, Sense::click()));
        let label = self.label;
        response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), label));
        let st = State::of(ui, &response);
        let look = button::look(
            Appearance {
                kind: self.kind,
                ..Appearance::default()
            },
            st,
        );
        let (fill, ink) = match self.kind {
            Kind::Primary => (look.fill, look.text),
            Kind::Secondary if st.disabled => (look.fill, look.text),
            Kind::Secondary => (
                Some(state::surface_for(self.ground + 1, st)),
                state::text_for(color::TEXT_1, st),
            ),
        };
        let painter = ui.painter();
        if let Some(fill) = fill {
            px::fill(painter, rect, radius::M, fill);
        }
        let pos = w::text_pos(rect, &text, rect.left() + self.padding_x, ppp);
        let text_right = pos.x + text.size().x;
        painter.galley(pos, text, ink);
        if let Some((g, c)) = trailing {
            let x = if self.width.is_some() {
                rect.right() - self.padding_x - g.size().x
            } else {
                text_right + wm::SHORTCUT_GAP
            };
            let pos = w::text_pos(rect, &g, x, ppp);
            painter.galley(pos, g, c.unwrap_or(ink));
        }
        if look.focus_ring {
            focus::paint_ring(painter, rect, radius::M);
        }
        response
    }
}

/// A row of dialog buttons aligned right in `width`, `gap` apart; returns their responses in
/// order.
pub(crate) fn button_row(ui: &mut Ui, width: f32, buttons: Vec<ActionButton<'_>>) -> Vec<Response> {
    let row = Rect::from_min_size(ui.cursor().min, vec2(width, m::BUTTON_HEIGHT));
    ui.allocate_rect(row, Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = m::BUTTON_GAP;
    let mut out: Vec<Response> = buttons
        .into_iter()
        .rev()
        .map(|b| b.show(&mut child))
        .collect();
    out.reverse();
    out
}

/// The S07 error notice box (`background:#2c1c1e`, i.e. `color.state.error-surface`).
pub(crate) fn error_surface() -> Color32 {
    color32(color::STATE_ERROR_SURFACE)
}

// ------------------------------------------------------------------------------------------------
// The app's side
// ------------------------------------------------------------------------------------------------

/// Repaint interval while S07 shows MIDI inputs (their activity dots blink).
const MIDI_REPAINT: std::time::Duration = std::time::Duration::from_millis(50);

impl crate::App {
    /// Draws the open dialogs (S06, S07; the export dialog is the shell's) and the overlay, and
    /// applies what the user did: contract actions go into the queue, overlay answers apply at
    /// once.
    pub(crate) fn draw_dialogs(&mut self, ctx: &Context) {
        use crate::actions::{Action, UiAction};
        use crate::ui_state::Dialog;
        match self.ui.dialog.clone() {
            Some(Dialog::Settings(section)) => {
                let model = self.settings_model(section);
                let scroll = self.dialogs.settings.scroll_to.take();
                for output in settings::show(ctx, &model, scroll) {
                    match output {
                        settings::Output::Close => {
                            self.queue.push(Action::Ui(UiAction::CloseDialog));
                        }
                        settings::Output::Section(s) => {
                            self.queue.push(Action::Ui(UiAction::OpenSettings(s)));
                            self.dialogs.settings.scroll_to = Some(s);
                        }
                        settings::Output::Act(action) => self.queue.push(action),
                        settings::Output::AddFolder => {
                            self.open_file_browser(file::Purpose::Folder);
                        }
                    }
                }
                if model.midi.as_ref().is_some_and(|m| !m.is_empty()) {
                    ctx.request_repaint_after(MIDI_REPAINT);
                }
            }
            Some(Dialog::MissingPlugins(missing)) => {
                let model = missing_plugins::model(self.project(), &missing);
                if let Some(output) = missing_plugins::show(ctx, &model) {
                    let action = match output {
                        missing_plugins::Output::Continue => {
                            crate::actions::ProjectAction::ContinueWithMissing
                        }
                        missing_plugins::Output::CheckFolders => {
                            crate::actions::ProjectAction::CheckPluginFolders
                        }
                    };
                    self.queue.push(Action::Project(action));
                }
            }
            Some(Dialog::Export(_)) | None => {}
        }
        if !matches!(self.ui.dialog, Some(Dialog::Settings(_))) {
            self.dialogs.settings.open = false;
        }
        match &mut self.dialogs.overlay {
            Some(Overlay::File(browser)) => {
                let purpose = browser.purpose;
                match file::show(ctx, browser) {
                    Some(file::Output::Chosen(path)) => self.file_chosen(purpose, path),
                    Some(file::Output::Cancel) => {
                        self.queue.push(Action::Ui(UiAction::CloseDialog));
                    }
                    None => {}
                }
            }
            Some(Overlay::Unsaved) => {
                let name = self.project().name.clone();
                if let Some(answer) = new_project::show_unsaved(ctx, &name) {
                    self.answer_unsaved(answer);
                }
            }
            None => {}
        }
    }
}
