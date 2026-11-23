//! S09 start screen (`Screens.dc.html` `isStart`, `S09.recentList`, notes `S09`;
//! `Prototyp.dc.html` start screen). komponist starts here when no project is given.
//!
//! Left: the wordmark "komponist" with version, the actions "Neues Projekt" (Strg N, S10),
//! "Projekt öffnen …" (Strg O) and "Aus Vorlage · Beat-Skizze", and at the bottom the system rows
//! Audio, MIDI and Plugins; a click on a row opens the settings at its section. Right: the recent
//! projects with filter. The last project is preselected; ↑ ↓ choose, Enter opens, Entf removes
//! the entry from the list (not the file); a click opens; a project file dropped on the window
//! opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::SystemTime;

use egui::{Color32, Key, Modifiers, Rect, Role, Sense, Ui, UiBuilder, pos2, vec2};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, radius, size, typography};
use komponist_ui::widgets::button::{Height, Kind, Trailing};
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, px, state};

use super::metrics::{MONO_SMALL, WORDMARK, start as m};
use super::{ActionButton, dot, format, label_node, section_label, styled};
use crate::platform::Platform;
use crate::project_io::ProjectSummary;
use crate::ui_state::{SettingsSection, StartView};

/// A recent project row.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentRow {
    /// The file.
    pub path: PathBuf,
    /// Project name ("Night Drive").
    pub name: String,
    /// The path as shown ("~/Musik/klang/Night Drive.komponist").
    pub path_label: String,
    /// "124 BPM · 6 Spuren", or "Datei fehlt".
    pub meta: String,
    /// "vor 2 Stunden", "gestern", "12. Okt.".
    pub date: String,
    /// Track hues (colour bars).
    pub hues: Vec<u16>,
    /// The file cannot be read.
    pub missing: bool,
    /// A recovery copy of this project waits (#227): the row shows the chip „Sicherungskopie“, and
    /// opening it asks for the copy first.
    pub recovery: Option<crate::recovery::Id>,
    /// A row of a copy alone, not of the recent list (an untitled project has an empty `path`).
    pub copy_only: bool,
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
    /// "0.1.0 · Linux · CLAP und VST3" ("· macOS ·", "· Windows ·").
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
    /// Open a row with a recovery copy: ask for the copy, then open the file (if it has one).
    Recovery(crate::recovery::Id),
    /// A system row: the settings at its section.
    Settings(SettingsSection),
}

/// The version line: "0.1.0 · Linux · CLAP und VST3", "0.1.0 · macOS · CLAP und VST3" on macOS,
/// "0.1.0 · Windows · CLAP und VST3" on Windows (which shortcuts like Linux, so [`Platform`] cannot
/// tell them apart).
pub fn version_line(t: Lang) -> String {
    if cfg!(windows) {
        t.start_version_windows(env!("CARGO_PKG_VERSION"))
    } else {
        version_line_for(t, Platform::current())
    }
}

/// [`version_line`] for a given platform (Linux or macOS).
pub fn version_line_for(t: Lang, platform: Platform) -> String {
    let version = env!("CARGO_PKG_VERSION");
    platform.pick(t.start_version(version), t.start_version_mac(version))
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
pub fn recent_row(
    t: Lang,
    path: PathBuf,
    summary: Option<&ProjectSummary>,
    now: SystemTime,
) -> RecentRow {
    let path_label = format::tilde(&path);
    let file_name =
        || crate::project_io::name_from_path(&path).unwrap_or_else(|| path_label.clone());
    match summary {
        Some(s) => {
            let date = match (s.modified.and_then(format::local), format::local(now)) {
                (Some(then), Some(now)) => format::relative_date(t, &then, &now),
                _ => String::new(),
            };
            RecentRow {
                name: s.name.clone(),
                meta: s.meta(t),
                date,
                hues: s.hues.clone(),
                missing: false,
                path,
                path_label,
                recovery: None,
                copy_only: false,
            }
        }
        None => RecentRow {
            name: file_name(),
            meta: t.start_file_missing().to_owned(),
            date: String::new(),
            hues: Vec::new(),
            missing: true,
            path,
            path_label,
            recovery: None,
            copy_only: false,
        },
    }
}

#[derive(Debug)]
enum SummaryState {
    Pending,
    Ready(Option<ProjectSummary>),
}

#[derive(Debug)]
struct SummaryLoad {
    results: Receiver<(usize, Option<ProjectSummary>)>,
    obsolete: Arc<AtomicBool>,
}

/// The recent projects' summaries. At most one worker reads files, even if a path on a slow
/// volume outlives an invalidation. Superseded results cannot change the visible list.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    paths: Vec<PathBuf>,
    summaries: Vec<SummaryState>,
    load: Option<SummaryLoad>,
}

impl Cache {
    fn refresh(
        &mut self,
        ctx: &egui::Context,
        paths: &[PathBuf],
        read_summary: impl Fn(&Path) -> Option<ProjectSummary> + Send + 'static,
    ) {
        if self.paths != paths {
            self.paths = paths.to_vec();
            self.invalidate();
        }
        if let Some(load) = &self.load {
            loop {
                match load.results.try_recv() {
                    Ok((index, summary)) => {
                        if !load.obsolete.load(Ordering::Relaxed) {
                            self.summaries[index] = SummaryState::Ready(summary);
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.load = None;
                        break;
                    }
                    Err(TryRecvError::Empty) => return,
                }
            }
        }

        let pending: Vec<_> = self
            .paths
            .iter()
            .zip(&self.summaries)
            .enumerate()
            .filter(|(_, (_, summary))| matches!(summary, SummaryState::Pending))
            .map(|(index, (path, _))| (index, path.clone()))
            .collect();
        if pending.is_empty() {
            return;
        }
        let (sender, results) = mpsc::channel();
        let obsolete = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&obsolete);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for (index, path) in pending {
                if cancelled.load(Ordering::Relaxed)
                    || sender.send((index, read_summary(&path))).is_err()
                {
                    break;
                }
                ctx.request_repaint();
            }
            // Also wake the UI when a cancelled load frees the only worker slot.
            drop(sender);
            ctx.request_repaint();
        });
        self.load = Some(SummaryLoad { results, obsolete });
    }

    fn rows(&self, t: Lang, now: SystemTime) -> Vec<RecentRow> {
        self.paths
            .iter()
            .zip(&self.summaries)
            .map(|(path, summary)| match summary {
                SummaryState::Pending => {
                    let mut row = recent_row(t, path.clone(), None, now);
                    row.meta = t.start_file_loading().to_owned();
                    row.missing = false;
                    row
                }
                SummaryState::Ready(summary) => recent_row(t, path.clone(), summary.as_ref(), now),
            })
            .collect()
    }

    /// Forgets the summaries and signals a previous batch to stop after its current read.
    pub(crate) fn invalidate(&mut self) {
        self.summaries.clear();
        self.summaries
            .resize_with(self.paths.len(), || SummaryState::Pending);
        if let Some(load) = &self.load {
            load.obsolete.store(true, Ordering::Relaxed);
        }
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
    let t = komponist_ui::lang(ui.ctx());
    let mut out = Vec::new();
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_0));
    let left = Rect::from_min_size(
        rect.min,
        vec2(m::LEFT_WIDTH.min(rect.width()), rect.height()),
    );
    px::fill(ui.painter(), left, 0.0, color32(color::SURFACE_1));
    let right = Rect::from_min_max(pos2(left.right() + 1.0, rect.top()), rect.max);
    left_column(ui, t, left, model, &mut out);
    right_column(ui, t, right, model, view, keys, &mut out);
    out
}

fn left_column(ui: &mut Ui, t: Lang, rect: Rect, model: &Model, out: &mut Vec<Output>) {
    let [pad_x, pad_y] = m::PADDING;
    let inner = rect.shrink2(vec2(pad_x, pad_y));
    let mut col = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .id_salt("komponist_start_left")
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    col.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let wordmark = crate::panels::transport::WORDMARK;
    let g = styled(&col, wordmark, &WORDMARK, color32(color::TEXT_1), None);
    super::paint_text(&mut col, g, wordmark);
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
                t.start_new(),
                Kind::Primary,
                Trailing::Chip(model.new_shortcut.as_str()),
            ),
            Output::New,
        ),
        (
            button(
                t.start_open(),
                Kind::Secondary,
                Trailing::Chip(model.open_shortcut.as_str()),
            ),
            Output::Open,
        ),
        (
            button(
                t.start_template(),
                Kind::Secondary,
                Trailing::Hint(t.new_project_template_name()),
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
    section_label(&mut sys, t.start_system());
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
    t: Lang,
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
    // A file row is found by its path, a copy's own row by its copy (two untitled copies have the
    // same empty path).
    let remembered_index = view.selected_path.as_ref().and_then(|path| {
        shown.iter().position(|&index| {
            let row = &model.recent[index];
            row.path == *path && copy_key(row) == view.selected_copy.as_ref()
        })
    });
    let selection_disappeared = view.selected_path.is_some() && remembered_index.is_none();
    view.selected =
        remembered_index.unwrap_or_else(|| view.selected.min(shown.len().saturating_sub(1)));

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
        // A disappearing row may change the fallback selection. Do not act on that replacement
        // before it has been shown to the user.
        if !selection_disappeared && let Some(&i) = shown.get(view.selected) {
            let row = &model.recent[i];
            if enter {
                out.push(open(row));
            }
            // A copy's own row has no list entry to remove.
            if delete && !row.copy_only {
                out.push(Output::RemoveRecent(row.path.clone()));
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
        t.start_recent(),
        &typography::LABEL,
        fonts::style_color(&typography::LABEL),
        None,
    );
    let pos = w::text_pos(header, &label, header.left(), col.pixels_per_point());
    let label_rect = Rect::from_min_size(pos, label.size());
    col.painter().galley(pos, label, Color32::PLACEHOLDER);
    label_node(&col, label_rect, t.start_recent(), Role::Label);
    let field = Rect::from_min_max(
        pos2(header.right() - m::FILTER_WIDTH, header.top()),
        header.max,
    );
    let mut field_ui = col.new_child(UiBuilder::new().max_rect(field));
    let filter = field_ui.add(
        TextField::new(&mut view.filter, t.start_filter())
            .id(filter_id)
            .hint(t.start_filter())
            .width(m::FILTER_WIDTH),
    );
    if filter.changed() {
        view.reset_selection();
    }
    col.add_space(m::RIGHT_GAP);

    let shown = filtered(&model.recent, &view.filter);
    if shown.is_empty() {
        let text = if model.recent.is_empty() {
            t.start_no_recent()
        } else {
            t.start_no_match()
        };
        let g = styled(&col, text, &typography::UI, color32(color::TEXT_3), None);
        super::paint_text(&mut col, g, text);
    }
    let builder = UiBuilder::new()
        .accessibility_role(Role::List)
        .accessibility_label(t.start_recent());
    col.scope_builder(builder, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, m::LIST_GAP);
        for (k, &i) in shown.iter().enumerate() {
            let row = &model.recent[i];
            let r = recent_row_ui(ui, width, row, k == view.selected);
            if r.clicked() {
                view.selected = k;
                out.push(open(row));
            }
        }
    });
    let selected = shown.get(view.selected).map(|&index| &model.recent[index]);
    view.selected_path = selected.map(|row| row.path.clone());
    view.selected_copy = selected.and_then(copy_key).cloned();
    col.add_space(m::RIGHT_GAP + m::HINT_TOP);
    let g = styled(
        &col,
        t.start_hint(),
        &typography::SMALL,
        color32(color::TEXT_3),
        Some(width),
    );
    super::paint_text(&mut col, g, t.start_hint());
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

/// What opening `row` means: its file, or, with a waiting copy, the prompt for that copy first.
fn open(row: &RecentRow) -> Output {
    match &row.recovery {
        Some(id) => Output::Recovery(id.clone()),
        None => Output::OpenRecent(row.path.clone()),
    }
}

/// The copy that tells a copy's own row apart (`None` for a row of the recent list).
fn copy_key(row: &RecentRow) -> Option<&crate::recovery::Id> {
    row.recovery.as_ref().filter(|_| row.copy_only)
}

/// One recent project (`recent` `s`): colour bars, name and path, meta, date; the selected row on
/// `surface.3` with a 1 px accent outline 1 px outside. A row with a recovery copy shows the chip
/// „Sicherungskopie“ at the right end of the name column (#227).
fn recent_row_ui(ui: &mut Ui, width: f32, row: &RecentRow, selected: bool) -> egui::Response {
    let t = komponist_ui::lang(ui.ctx());
    let ppp = ui.pixels_per_point();
    let (rect, response) = ui.allocate_exact_size(vec2(width, m::ROW_HEIGHT), Sense::click());
    let mut name = format!("{}, {}", row.name, row.meta);
    if row.recovery.is_some() {
        name = t.start_recovery_row(name);
    }
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
    let mut text_right = meta_x - m::ROW_GAP;
    // The chip only where the name keeps its room; in a narrow window the row's accessible name
    // still says that a copy waits.
    if row.recovery.is_some() {
        let chip = recovery_chip_galley(ui, t);
        let left = text_right - chip_size(&chip).x;
        if left - m::ROW_GAP - text_x >= m::CHIP_MIN_NAME_WIDTH {
            paint_recovery_chip(ui, chip, inner, text_right);
            text_right = left - m::ROW_GAP;
        }
    }
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

/// The text of the chip „Sicherungskopie“ (`type.ui`, `accent`).
fn recovery_chip_galley(ui: &Ui, t: Lang) -> std::sync::Arc<egui::Galley> {
    styled(
        ui,
        t.start_recovery_chip(),
        &typography::UI,
        color32(color::ACCENT),
        None,
    )
}

/// The chip's size around its text.
fn chip_size(g: &egui::Galley) -> egui::Vec2 {
    let [pad_x, pad_y] = m::CHIP_PADDING;
    vec2(g.size().x.ceil() + 2.0 * pad_x, g.size().y + 2.0 * pad_y)
}

/// Paints the chip (`accent.surface`) with its right edge at `right`, centred in `row`.
fn paint_recovery_chip(ui: &Ui, g: std::sync::Arc<egui::Galley>, row: Rect, right: f32) {
    let ppp = ui.pixels_per_point();
    let [pad_x, _] = m::CHIP_PADDING;
    let size = chip_size(&g);
    let chip = px::snap_rect(
        Rect::from_min_size(pos2(right - size.x, row.center().y - size.y / 2.0), size),
        ppp,
    );
    px::fill(
        ui.painter(),
        chip,
        radius::S,
        color32(color::ACCENT_SURFACE),
    );
    ui.painter().galley(
        w::text_pos(chip, &g, chip.left() + pad_x, ppp),
        g,
        Color32::PLACEHOLDER,
    );
}

impl crate::App {
    /// Builds the start screen model from summaries available on this frame.
    pub(crate) fn start_model(&mut self, ctx: &egui::Context) -> Model {
        use crate::actions::{Action, ProjectAction, UiAction};
        use komponist_audio::{AudioState, HostKind};
        let t = self.lang;
        let label = |app: &Self, action: Action| {
            app.keymap.label_for(&action, app.lang).unwrap_or_default()
        };
        let new_shortcut = label(self, Action::Project(ProjectAction::New));
        let open_shortcut = label(self, Action::Project(ProjectAction::Open));
        let settings_shortcut = label(
            self,
            Action::Ui(UiAction::OpenSettings(SettingsSection::Audio)),
        );
        self.dialogs
            .start
            .refresh(ctx, &self.recent.projects, |path| {
                ProjectSummary::read(path).ok()
            });
        let now = SystemTime::now();
        let mut recent = self.dialogs.start.rows(t, now);
        self.recovery_rows(&mut recent, now);

        let audio = match self.engine.audio_state() {
            AudioState::NoDevice { .. } => (Health::Error, t.start_audio_none().to_owned()),
            AudioState::Stopped => (Health::Idle, t.start_audio_stopped().to_owned()),
            AudioState::Running { config, .. } if config.host == HostKind::Null => {
                (Health::Idle, t.start_audio_none().to_owned())
            }
            AudioState::Running { config, .. } => {
                let host = super::settings::DRIVERS
                    .iter()
                    .find(|(h, _)| *h == config.host)
                    .map_or("", |(_, l)| l);
                let device = config
                    .device
                    .clone()
                    .unwrap_or_else(|| t.settings_default_device().to_owned());
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
            (Health::Idle, t.start_midi_none().to_owned())
        } else {
            (Health::Ok, inputs.join(", "))
        };
        let host = self.engine.host();
        let plugins = if host.is_scanning() {
            t.start_plugins_scanning(&settings_shortcut)
        } else {
            let found = host
                .last_scan()
                .map_or(host.catalog().entries().len(), |r| r.found);
            t.start_plugins_found(found, &settings_shortcut)
        };
        let system = vec![
            SystemRow {
                label: t.start_row_audio(),
                info: audio.1,
                health: audio.0,
                section: SettingsSection::Audio,
            },
            SystemRow {
                label: t.start_row_midi(),
                info: midi.1,
                health: midi.0,
                section: SettingsSection::Midi,
            },
            SystemRow {
                label: t.start_row_plugins(),
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
            version: version_line(t),
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
        let model = self.start_model(ui.ctx());
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
                Output::Recovery(id) => self.ask_recovery_of(id),
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
            recovery: None,
            copy_only: false,
        }
    }

    #[test]
    fn the_filter_matches_name_and_path() {
        let rows = [
            row("Night Drive", "~/Musik/klang/Night Drive.komponist"),
            row("Hafenlicht", "~/Musik/klang/Hafenlicht.komponist"),
            row("Beat-Skizze 14", "~/Musik/klang/skizzen/beat-14.komponist"),
        ];
        assert_eq!(filtered(&rows, ""), [0, 1, 2]);
        assert_eq!(filtered(&rows, "hafen"), [1]);
        assert_eq!(filtered(&rows, "SKIZZEN"), [2]);
        assert!(filtered(&rows, "xyz").is_empty());
    }

    #[test]
    fn pending_rows_keep_their_paths_and_a_superseded_read_cannot_replace_a_new_row() {
        use std::time::{Duration, Instant};

        let timeout = Duration::from_secs(3);
        let ctx = egui::Context::default();
        let (started, reading) = mpsc::channel();
        let (release, finish) = mpsc::channel();
        let mut cache = Cache::default();
        let old_path = PathBuf::from("old.komponist");
        cache.refresh(&ctx, std::slice::from_ref(&old_path), move |path| {
            started.send(path.to_owned()).unwrap();
            finish.recv_timeout(timeout).ok()?;
            Some(ProjectSummary::of(
                &komponist_model::Project::new("Obsolete"),
                None,
            ))
        });
        assert_eq!(reading.recv_timeout(timeout).unwrap(), old_path);
        let pending = cache.rows(Lang::De, SystemTime::now());
        assert_eq!(pending[0].path, old_path);
        assert_eq!(pending[0].meta, Lang::De.start_file_loading());
        assert!(!pending[0].missing);

        // Keep the reading blocked while the list changes and verify the replacement row is
        // immediately available without queueing another worker.
        let new_path = PathBuf::from("replacement.komponist");
        let missing_path = PathBuf::from("missing.komponist");
        let expected = [new_path.clone(), missing_path];
        cache.invalidate();
        cache.refresh(&ctx, &expected, |_| {
            panic!("an obsolete worker is still reading")
        });
        let pending = cache.rows(Lang::En, SystemTime::now());
        assert_eq!(pending.len(), expected.len());
        assert_eq!(pending[0].path, new_path);
        assert_eq!(pending[0].meta, Lang::En.start_file_loading());
        assert!(!pending[1].missing);

        release.send(()).unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            cache.refresh(&ctx, &expected, |path| {
                (path == Path::new("replacement.komponist")).then(|| {
                    ProjectSummary::of(&komponist_model::Project::new("New summary"), None)
                })
            });
            let rows = cache.rows(Lang::De, SystemTime::now());
            assert_ne!(rows[0].name, "Obsolete");
            if rows[1].missing {
                assert_eq!(rows[0].name, "New summary");
                assert_eq!(rows[1].meta, Lang::De.start_file_missing());
                assert_eq!(
                    cache.rows(Lang::En, SystemTime::now())[1].meta,
                    Lang::En.start_file_missing(),
                );
                break;
            }
            assert!(Instant::now() < deadline, "summary worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn unreadable_files_stay_listed_by_their_file_name() {
        let r = recent_row(
            Lang::De,
            PathBuf::from("/nirgends/Alt.komponist"),
            None,
            SystemTime::now(),
        );
        assert_eq!(r.name, "Alt");
        assert_eq!(r.meta, komponist_i18n::de::START_FILE_MISSING);
        assert!(r.missing);
        let pc = |t| version_line_for(t, Platform::Pc);
        let mac = |t| version_line_for(t, Platform::Mac);
        assert!(pc(Lang::De).ends_with(" · Linux · CLAP und VST3"));
        assert!(pc(Lang::En).ends_with(" · Linux · CLAP and VST3"));
        assert!(mac(Lang::De).ends_with(" · macOS · CLAP und VST3"));
        assert!(mac(Lang::En).ends_with(" · macOS · CLAP and VST3"));
        #[cfg(not(windows))]
        assert_eq!(
            version_line(Lang::De),
            Platform::current().pick(pc(Lang::De), mac(Lang::De))
        );
        #[cfg(windows)]
        {
            assert!(version_line(Lang::De).ends_with(" · Windows · CLAP und VST3"));
            assert!(version_line(Lang::En).ends_with(" · Windows · CLAP and VST3"));
        }
    }
}
