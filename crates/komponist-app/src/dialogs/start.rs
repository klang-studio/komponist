//! S09 start screen (`Screens.dc.html` `isStart`, `S09.recentList`, notes `S09`;
//! `Prototyp.dc.html` start screen). komponist starts here when no project is given.
//!
//! Left: the wordmark "komponist" with version, the actions "Neues Projekt" (Strg N, S10),
//! "Projekt öffnen …" (Strg O) and "Aus Vorlage · Beat-Skizze", and at the bottom the system rows
//! Audio, MIDI and Plugins; a click on a row opens the settings at its section. Right: the recent
//! projects with filter. The last project is preselected; ↑ ↓ choose, Enter opens, Entf removes
//! the entry from the list (not the file); a click opens; a project file dropped on the window
//! opens.

use std::path::PathBuf;
use std::time::SystemTime;

use egui::{Color32, Key, Modifiers, Rect, Role, Sense, Ui, UiBuilder, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, radius, size, typography};
use komponist_ui::widgets::button::{Height, Kind, Trailing};
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, px, state};

use super::metrics::{MONO_SMALL, WORDMARK, start as m};
use super::{ActionButton, dot, format, label_node, section_label, styled};
use crate::project_io::ProjectSummary;
use crate::ui_state::{SettingsSection, StartView};

/// Texts of S09.
pub mod names {
    /// The wordmark (rule 1: "klang-studio" in the screens).
    pub const WORDMARK: &str = "komponist";
    /// "Neues Projekt".
    pub const NEW: &str = "Neues Projekt";
    /// "Projekt öffnen …".
    pub const OPEN: &str = "Projekt öffnen …";
    /// "Aus Vorlage".
    pub const TEMPLATE: &str = "Aus Vorlage";
    /// "System".
    pub const SYSTEM: &str = "System";
    /// "Zuletzt geöffnet".
    pub const RECENT: &str = "Zuletzt geöffnet";
    /// The filter field's placeholder and name.
    pub const FILTER: &str = "Filtern";
    /// The keyboard hint under the list.
    pub const HINT: &str = "↑ ↓ wählen · Enter öffnet · Entf entfernt aus der Liste · Projekte \
                            lassen sich auch per Drag and Drop öffnen";
    /// The list without entries.
    pub const NO_RECENT: &str = "Noch keine Projekte geöffnet.";
    /// The list with a filter that matches nothing.
    pub const NO_MATCH: &str = "Kein Projekt passt zum Filter.";
    /// A recent file that is gone.
    pub const FILE_MISSING: &str = "Datei fehlt";
}

/// A recent project row.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentRow {
    /// The file.
    pub path: PathBuf,
    /// Project name ("Nachtfahrt").
    pub name: String,
    /// The path as shown ("~/Musik/klang/Nachtfahrt.komponist").
    pub path_label: String,
    /// "124 BPM · 6 Spuren", or "Datei fehlt".
    pub meta: String,
    /// "vor 2 Stunden", "gestern", "12. Okt.".
    pub date: String,
    /// Track hues (colour bars).
    pub hues: Vec<u16>,
    /// The file cannot be read.
    pub missing: bool,
}

/// The colour of a system row's dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Ready (`state.play`, green).
    Ok,
    /// Not ready (`state.error`, red).
    Error,
    /// Neutral (`text.3`).
    Idle,
}

/// A system row ("Audio · PipeWire · Scarlett 2i2 · 48 kHz · 256").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemRow {
    /// "Audio", "MIDI", "Plugins".
    pub label: &'static str,
    /// What it says.
    pub info: String,
    /// Its dot.
    pub health: Health,
    /// The settings section a click opens.
    pub section: SettingsSection,
}

/// Everything the start screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// "0.1.0 · Linux · CLAP und VST3".
    pub version: String,
    /// Shortcut of "Neues Projekt" ("Strg N").
    pub new_shortcut: String,
    /// Shortcut of "Projekt öffnen …" ("Strg O").
    pub open_shortcut: String,
    /// The recent projects, newest first.
    pub recent: Vec<RecentRow>,
    /// Audio, MIDI, Plugins.
    pub system: Vec<SystemRow>,
    /// A message under the list (an error when opening failed).
    pub message: Option<String>,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// "Neues Projekt".
    New,
    /// "Projekt öffnen …".
    Open,
    /// "Aus Vorlage".
    Template,
    /// Open a recent project (click, Enter, a dropped file).
    OpenRecent(PathBuf),
    /// Entf: remove the entry from the list.
    RemoveRecent(PathBuf),
    /// A system row: the settings at its section.
    Settings(SettingsSection),
}

/// The version line: "0.1.0 · Linux · CLAP und VST3".
pub fn version_line() -> String {
    format!("{} · Linux · CLAP und VST3", env!("CARGO_PKG_VERSION"))
}

/// Indices of the rows that match `filter` (name or path, case-insensitive).
pub fn filtered(recent: &[RecentRow], filter: &str) -> Vec<usize> {
    let f = filter.trim().to_lowercase();
    recent
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            f.is_empty()
                || r.name.to_lowercase().contains(&f)
                || r.path_label.to_lowercase().contains(&f)
        })
        .map(|(i, _)| i)
        .collect()
}

/// The row of a recent file from its summary (`None`: unreadable) at `now`.
pub fn recent_row(path: PathBuf, summary: Option<&ProjectSummary>, now: SystemTime) -> RecentRow {
    let path_label = format::tilde(&path);
    let file_name =
        || crate::project_io::name_from_path(&path).unwrap_or_else(|| path_label.clone());
    match summary {
        Some(s) => {
            let date = match (s.modified.and_then(format::local), format::local(now)) {
                (Some(then), Some(now)) => format::relative_date(&then, &now),
                _ => String::new(),
            };
            RecentRow {
                name: s.name.clone(),
                meta: s.meta(),
                date,
                hues: s.hues.clone(),
                missing: false,
                path,
                path_label,
            }
        }
        None => RecentRow {
            name: file_name(),
            meta: names::FILE_MISSING.to_owned(),
            date: String::new(),
            hues: Vec::new(),
            missing: true,
            path,
            path_label,
        },
    }
}

/// The recent projects' summaries, read when the list changes (app-private).
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// The list the summaries belong to.
    pub(crate) paths: Vec<PathBuf>,
    /// One per path; `None` if the file cannot be read.
    pub(crate) summaries: Vec<Option<ProjectSummary>>,
}

impl Cache {
    /// Reads the summaries if `paths` differ from the cached list.
    pub(crate) fn refresh(&mut self, paths: &[PathBuf]) {
        if self.paths == paths {
            return;
        }
        self.paths = paths.to_vec();
        self.summaries = paths.iter().map(|p| ProjectSummary::read(p).ok()).collect();
    }

    /// Forgets the summaries (a file was saved).
    pub(crate) fn invalidate(&mut self) {
        self.paths.clear();
        self.summaries.clear();
    }
}

/// Draws the start screen into `rect`. `keys`: the screen takes ↑ ↓ Enter Entf (no dialog open).
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    model: &Model,
    view: &mut StartView,
    keys: bool,
) -> Vec<Output> {
    let mut out = Vec::new();
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_0));
    let left = Rect::from_min_size(
        rect.min,
        vec2(m::LEFT_WIDTH.min(rect.width()), rect.height()),
    );
    px::fill(ui.painter(), left, 0.0, color32(color::SURFACE_1));
    let right = Rect::from_min_max(pos2(left.right() + 1.0, rect.top()), rect.max);
    left_column(ui, left, model, &mut out);
    right_column(ui, right, model, view, keys, &mut out);
    out
}

fn left_column(ui: &mut Ui, rect: Rect, model: &Model, out: &mut Vec<Output>) {
    let [pad_x, pad_y] = m::PADDING;
    let inner = rect.shrink2(vec2(pad_x, pad_y));
    let mut col = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .id_salt("komponist_start_left")
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    col.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let g = styled(
        &col,
        names::WORDMARK,
        &WORDMARK,
        color32(color::TEXT_1),
        None,
    );
    super::paint_text(&mut col, g, names::WORDMARK);
    col.add_space(m::WORDMARK_GAP);
    let g = styled(
        &col,
        &model.version,
        &typography::VALUE_M,
        color32(color::TEXT_3),
        None,
    );
    super::paint_text(&mut col, g, &model.version);
    col.add_space(m::LEFT_GAP);

    let button = |label, kind, trailing| ActionButton {
        label,
        kind,
        ground: 2,
        height: Height::Large,
        trailing: Some(trailing),
        width: Some(m::BUTTON_WIDTH),
    };
    let buttons = [
        (
            button(
                names::NEW,
                Kind::Primary,
                Trailing::Chip(model.new_shortcut.as_str()),
            ),
            Output::New,
        ),
        (
            button(
                names::OPEN,
                Kind::Secondary,
                Trailing::Chip(model.open_shortcut.as_str()),
            ),
            Output::Open,
        ),
        (
            button(
                names::TEMPLATE,
                Kind::Secondary,
                Trailing::Hint(super::new_project::names::TEMPLATE),
            ),
            Output::Template,
        ),
    ];
    for (i, (b, action)) in buttons.into_iter().enumerate() {
        if i > 0 {
            col.add_space(m::BUTTON_GAP);
        }
        if b.show(&mut col).clicked() {
            out.push(action);
        }
    }

    // The system block sits at the bottom of the column.
    let rows = model.system.len() as f32;
    let row_height = typography::UI.line_height;
    let block = typography::LABEL.line_height + rows * (m::SYSTEM_GAP + row_height);
    let top = inner.bottom() - block;
    let mut sys = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(inner.left(), top), inner.max))
            .id_salt("komponist_start_system")
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    sys.spacing_mut().item_spacing = vec2(0.0, 0.0);
    section_label(&mut sys, names::SYSTEM);
    for row in &model.system {
        sys.add_space(m::SYSTEM_GAP);
        if system_row(&mut sys, inner.width(), row).clicked() {
            out.push(Output::Settings(row.section));
        }
    }
}

/// A system row: dot, label, info in Mono; clickable.
fn system_row(ui: &mut Ui, width: f32, row: &SystemRow) -> egui::Response {
    let ppp = ui.pixels_per_point();
    let label = styled(ui, row.label, &typography::UI, color32(color::TEXT_1), None);
    let info = styled(ui, &row.info, &MONO_SMALL, color32(color::TEXT_2), None);
    let height = typography::UI.line_height;
    let natural = m::DOT + 2.0 * m::SYSTEM_ROW_GAP + label.size().x.ceil() + info.size().x.ceil();
    let (rect, response) = ui.allocate_exact_size(vec2(natural.min(width), height), Sense::click());
    let name = format!("{} {}", row.label, row.info);
    response.widget_info(|| egui::WidgetInfo::labeled(Role::Button, true, &name));
    if response.hovered() {
        ui.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let ink = match row.health {
        Health::Ok => color::STATE_PLAY,
        Health::Error => color::STATE_ERROR,
        Health::Idle => color::TEXT_3,
    };
    dot(
        ui,
        pos2(rect.left() + m::DOT / 2.0, rect.center().y),
        m::DOT,
        color32(ink),
    );
    let mut x = rect.left() + m::DOT + m::SYSTEM_ROW_GAP;
    let painter = ui.painter().with_clip_rect(rect.expand(1.0));
    let label_width = label.size().x;
    painter.galley(
        w::text_pos(rect, &label, x, ppp),
        label,
        Color32::PLACEHOLDER,
    );
    x += label_width + m::SYSTEM_ROW_GAP;
    let info_color = if response.hovered() {
        color32(color::TEXT_1)
    } else {
        Color32::PLACEHOLDER
    };
    painter.galley(w::text_pos(rect, &info, x, ppp), info, info_color);
    if focus::ring_visible(&response) {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    response
}

fn right_column(
    ui: &mut Ui,
    rect: Rect,
    model: &Model,
    view: &mut StartView,
    keys: bool,
    out: &mut Vec<Output>,
) {
    let [pad_x, pad_y] = m::PADDING;
    let inner = rect.shrink2(vec2(pad_x, pad_y));
    let width = inner.width().min(m::RECENT_MAX_WIDTH);
    let shown = filtered(&model.recent, &view.filter);
    if view.selected >= shown.len() {
        view.selected = shown.len().saturating_sub(1);
    }

    // Keys before the filter field sees them: ↑ ↓ Enter work while typing in it, too.
    let filter_id = egui::Id::unique("komponist_start_filter");
    if keys {
        let filter_focused = ui.memory(|mem| mem.has_focus(filter_id));
        let (up, down, enter, delete) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                !filter_focused && i.consume_key(Modifiers::NONE, Key::Delete),
            )
        });
        if up {
            view.selected = view.selected.saturating_sub(1);
        }
        if down && view.selected + 1 < shown.len() {
            view.selected += 1;
        }
        if let Some(&i) = shown.get(view.selected) {
            if enter {
                out.push(Output::OpenRecent(model.recent[i].path.clone()));
            }
            if delete {
                out.push(Output::RemoveRecent(model.recent[i].path.clone()));
            }
        }
    }

    let mut col = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_size(inner.min, vec2(width, inner.height())))
            .id_salt("komponist_start_right")
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    col.spacing_mut().item_spacing = vec2(0.0, 0.0);
    // Header: "Zuletzt geöffnet" and the filter.
    let header = Rect::from_min_size(col.cursor().min, vec2(width, size::CONTROL));
    col.allocate_rect(header, Sense::hover());
    let label = styled(
        &col,
        names::RECENT,
        &typography::LABEL,
        fonts::style_color(&typography::LABEL),
        None,
    );
    let pos = w::text_pos(header, &label, header.left(), col.pixels_per_point());
    let label_rect = Rect::from_min_size(pos, label.size());
    col.painter().galley(pos, label, Color32::PLACEHOLDER);
    label_node(&col, label_rect, names::RECENT, Role::Label);
    let field = Rect::from_min_max(
        pos2(header.right() - m::FILTER_WIDTH, header.top()),
        header.max,
    );
    let mut field_ui = col.new_child(UiBuilder::new().max_rect(field));
    let filter = field_ui.add(
        TextField::new(&mut view.filter, names::FILTER)
            .id(filter_id)
            .hint(names::FILTER)
            .width(m::FILTER_WIDTH),
    );
    if filter.changed() {
        view.selected = 0;
    }
    col.add_space(m::RIGHT_GAP);

    let shown = filtered(&model.recent, &view.filter);
    if shown.is_empty() {
        let text = if model.recent.is_empty() {
            names::NO_RECENT
        } else {
            names::NO_MATCH
        };
        let g = styled(&col, text, &typography::UI, color32(color::TEXT_3), None);
        super::paint_text(&mut col, g, text);
    }
    let builder = UiBuilder::new()
        .accessibility_role(Role::List)
        .accessibility_label(names::RECENT);
    col.scope_builder(builder, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, m::LIST_GAP);
        for (k, &i) in shown.iter().enumerate() {
            let row = &model.recent[i];
            let r = recent_row_ui(ui, width, row, k == view.selected);
            if r.clicked() {
                view.selected = k;
                out.push(Output::OpenRecent(row.path.clone()));
            }
        }
    });
    col.add_space(m::RIGHT_GAP + m::HINT_TOP);
    let g = styled(
        &col,
        names::HINT,
        &typography::SMALL,
        color32(color::TEXT_3),
        Some(width),
    );
    super::paint_text(&mut col, g, names::HINT);
    if let Some(message) = &model.message {
        col.add_space(m::SYSTEM_GAP);
        let g = styled(
            &col,
            message,
            &typography::SMALL,
            color32(color::STATE_ERROR),
            Some(width),
        );
        super::paint_text(&mut col, g, message);
    }
}

/// One recent project (`recent` `s`): colour bars, name and path, meta, date; the selected row on
/// `surface.3` with a 1 px accent outline 1 px outside.
fn recent_row_ui(ui: &mut Ui, width: f32, row: &RecentRow, selected: bool) -> egui::Response {
    let ppp = ui.pixels_per_point();
    let (rect, response) = ui.allocate_exact_size(vec2(width, m::ROW_HEIGHT), Sense::click());
    let name = format!("{}, {}", row.name, row.meta);
    response.widget_info(|| egui::WidgetInfo::selected(Role::ListItem, true, selected, &name));
    let st = state::State::of(ui, &response);
    let painter = ui.painter();
    if selected {
        px::fill(painter, rect, radius::M, color32(color::SURFACE_3));
        px::outline(
            painter,
            rect,
            radius::M,
            m::OUTLINE_OFFSET,
            color32(color::ACCENT),
        );
    } else if st.hovered {
        px::fill(painter, rect, radius::M, color32(color::SURFACE_2));
    }
    if st.focused {
        focus::paint_ring(painter, rect, radius::M);
    }
    let inner = rect.shrink2(vec2(m::ROW_PADDING_X, 0.0));
    // Colour bars.
    let [bw, bh] = m::BAR;
    let mut x = inner.left();
    for &hue in row.hues.iter().take(m::BARS_MAX) {
        let bar = Rect::from_min_size(pos2(x, inner.center().y - bh / 2.0), vec2(bw, bh));
        px::fill(
            painter,
            px::snap_rect(bar, ppp),
            m::BAR_RADIUS,
            color32(TrackPalette::new(hue).base),
        );
        x += bw + m::BAR_GAP;
    }
    let text_x = inner.left() + m::BARS_COLUMN + m::ROW_GAP;
    let date_x = inner.right() - m::DATE_COLUMN;
    let meta_x = date_x - m::ROW_GAP - m::META_COLUMN;
    let text_right = meta_x - m::ROW_GAP;
    // Name and path.
    let name_color = if row.missing {
        color::TEXT_2
    } else {
        color::TEXT_1
    };
    let name_g = super::galley(
        ui,
        &row.name,
        w::font_at(&typography::TRACK, wm::SEMIBOLD),
        typography::TRACK.line_height,
        color32(name_color),
        None,
    );
    let path_g = super::styled_line(
        ui,
        &row.path_label,
        &MONO_SMALL,
        color32(color::TEXT_3),
        text_right - text_x,
    );
    let block = name_g.size().y + m::NAME_GAP + path_g.size().y;
    let top = px::snap(inner.center().y - block / 2.0, ppp);
    let clip = painter.with_clip_rect(Rect::from_x_y_ranges(text_x..=text_right, rect.y_range()));
    let path_top = top + name_g.size().y + m::NAME_GAP;
    clip.galley(pos2(text_x, top), name_g, Color32::PLACEHOLDER);
    clip.galley(
        pos2(text_x, px::snap(path_top, ppp)),
        path_g,
        Color32::PLACEHOLDER,
    );
    // Meta and date.
    let meta_color = if row.missing {
        color::STATE_ERROR
    } else {
        color::TEXT_2
    };
    let meta = styled(ui, &row.meta, &MONO_SMALL, color32(meta_color), None);
    painter.galley(
        w::text_pos(inner, &meta, meta_x, ppp),
        meta,
        Color32::PLACEHOLDER,
    );
    let date = styled(ui, &row.date, &typography::UI, color32(color::TEXT_3), None);
    let date_pos = w::text_pos(inner, &date, inner.right() - date.size().x, ppp);
    painter.galley(date_pos, date, Color32::PLACEHOLDER);
    response
}

impl crate::App {
    /// The start screen's model: version, shortcuts, recent projects (summaries read when the list
    /// changed), system rows, the last error.
    pub(crate) fn start_model(&mut self) -> Model {
        use crate::actions::{Action, ProjectAction, UiAction};
        use komponist_audio::{AudioState, HostKind};
        let label = |app: &Self, action: Action| {
            app.keymap.label_for(&action, app.lang).unwrap_or_default()
        };
        let new_shortcut = label(self, Action::Project(ProjectAction::New));
        let open_shortcut = label(self, Action::Project(ProjectAction::Open));
        let settings_shortcut = label(
            self,
            Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)),
        );
        self.dialogs.start.refresh(&self.recent.projects);
        let now = SystemTime::now();
        let recent = self
            .dialogs
            .start
            .paths
            .iter()
            .zip(&self.dialogs.start.summaries)
            .map(|(p, s)| recent_row(p.clone(), s.as_ref(), now))
            .collect();

        let audio = match self.engine.audio_state() {
            AudioState::NoDevice { .. } => (Health::Error, "Kein Audio-Gerät".to_owned()),
            AudioState::Stopped => (Health::Idle, "Audio angehalten".to_owned()),
            AudioState::Running { config, .. } if config.host == HostKind::Null => {
                (Health::Idle, "Kein Audio-Gerät".to_owned())
            }
            AudioState::Running { config, .. } => {
                let host = super::settings::DRIVERS
                    .iter()
                    .find(|(h, _)| *h == config.host)
                    .map_or("", |(_, l)| l);
                let device = config
                    .device
                    .clone()
                    .unwrap_or_else(|| super::settings::names::DEFAULT_DEVICE.to_owned());
                let rate = crate::panels::statusbar::fmt_rate(config.sample_rate);
                (
                    Health::Ok,
                    format!("{host} · {device} · {rate} · {}", config.buffer_frames),
                )
            }
        };
        let inputs: Vec<String> = self.engine.midi().map_or_else(Vec::new, |hub| {
            let ports = hub.ports();
            let all: Vec<&str> = ports.iter().map(|p| p.name.as_str()).collect();
            ports
                .iter()
                .filter(|p| p.enabled && p.connected)
                .map(|p| super::settings::midi_label(&p.name, &all))
                .collect()
        });
        let midi = if inputs.is_empty() {
            (Health::Idle, "kein MIDI-Gerät erkannt".to_owned())
        } else {
            (Health::Ok, inputs.join(", "))
        };
        let host = self.engine.host();
        let plugins = if host.is_scanning() {
            format!("Scan läuft · Einstellungen {settings_shortcut}")
        } else {
            let found = host
                .last_scan()
                .map_or(host.catalog().entries().len(), |r| r.found);
            format!("{found} gefunden · Einstellungen {settings_shortcut}")
        };
        let system = vec![
            SystemRow {
                label: "Audio",
                info: audio.1,
                health: audio.0,
                section: SettingsSection::Audio,
            },
            SystemRow {
                label: "MIDI",
                info: midi.1,
                health: midi.0,
                section: SettingsSection::Midi,
            },
            SystemRow {
                label: "Plugins",
                info: plugins,
                health: Health::Idle,
                section: SettingsSection::Plugins,
            },
        ];
        let message = self
            .ui
            .status
            .as_ref()
            .filter(|s| s.kind == crate::ui_state::StatusKind::Error)
            .map(|s| s.text.clone());
        Model {
            version: version_line(),
            new_shortcut,
            open_shortcut,
            recent,
            system,
            message,
        }
    }

    /// Draws the start screen into `rect` and applies what the user did.
    pub(crate) fn draw_start(&mut self, ui: &mut Ui, rect: Rect) {
        use crate::actions::{Action, ProjectAction, UiAction};
        let model = self.start_model();
        let keys = self.ui.dialog.is_none() && self.dialogs.overlay.is_none();
        let mut view = std::mem::take(&mut self.ui.start);
        let mut outputs = show(ui, rect, &model, &mut view, keys);
        self.ui.start = view;
        let dropped = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .find(|p| crate::project_io::is_project_file(p))
        });
        if let Some(path) = dropped {
            outputs.push(Output::OpenRecent(path));
        }
        for output in outputs {
            match output {
                Output::New => self.queue.push(Action::Project(ProjectAction::New)),
                Output::Open => self.queue.push(Action::Project(ProjectAction::Open)),
                Output::Template => self.proceed(super::Pending::Template),
                Output::OpenRecent(path) => {
                    self.queue
                        .push(Action::Project(ProjectAction::OpenPath(path)));
                }
                Output::RemoveRecent(path) => self.forget(&path),
                Output::Settings(section) => {
                    self.queue.push(Action::Ui(UiAction::OpenSettings(section)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, path: &str) -> RecentRow {
        RecentRow {
            path: PathBuf::from(path),
            name: name.to_owned(),
            path_label: path.to_owned(),
            meta: String::new(),
            date: String::new(),
            hues: Vec::new(),
            missing: false,
        }
    }

    #[test]
    fn the_filter_matches_name_and_path() {
        let rows = [
            row("Nachtfahrt", "~/Musik/klang/Nachtfahrt.komponist"),
            row("Hafenlicht", "~/Musik/klang/Hafenlicht.komponist"),
            row("Beat-Skizze 14", "~/Musik/klang/skizzen/beat-14.komponist"),
        ];
        assert_eq!(filtered(&rows, ""), [0, 1, 2]);
        assert_eq!(filtered(&rows, "hafen"), [1]);
        assert_eq!(filtered(&rows, "SKIZZEN"), [2]);
        assert!(filtered(&rows, "xyz").is_empty());
    }

    #[test]
    fn unreadable_files_stay_listed_by_their_file_name() {
        let r = recent_row(
            PathBuf::from("/nirgends/Alt.komponist"),
            None,
            SystemTime::now(),
        );
        assert_eq!(r.name, "Alt");
        assert_eq!(r.meta, names::FILE_MISSING);
        assert!(r.missing);
        assert!(version_line().ends_with(" · Linux · CLAP und VST3"));
    }
}
