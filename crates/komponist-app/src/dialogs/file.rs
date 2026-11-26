//! A small file browser for "Projekt öffnen" (Ctrl O), "Speichern" (Ctrl S the first time, "Speichern
//! unter"), "+ Ordner" in S07 and the export folder in S08 ("Ändern"; why not native dialogs:
//! `dialogs` module docs).
//!
//! The screens have no file dialog; this one follows the other dialogs: header with title and ×,
//! a `surface.1` body, a footer with "Abbrechen" and the primary action. The body has the folder
//! (editable path, "‹" for the parent folder), the list (folders first, then `.komponist` files;
//! hidden entries are left out) and, when saving, the file name.
//!
//! Keys: ↑ ↓ choose a row, Enter opens a folder or takes the file (when saving: saves under the
//! typed name), Rücktaste goes to the parent folder (outside text fields), Esc cancels. A
//! double-click opens a folder or takes a file. Saving over an existing file asks once more: the
//! primary button becomes "Ersetzen".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::{Color32, Context, Id, Key, Modifiers, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_i18n::Lang;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::widgets::button::{Button, Kind};
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::{self as w};
use komponist_ui::{icons, px, state};

use super::metrics::dialog as m;
use super::{ActionButton, button_row, header, label_node, modal, styled};
use crate::project_io::{is_project_file, with_extension};

/// What the browser is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Open a `.komponist` file.
    Open,
    /// Save under a file name.
    Save,
    SavePreset(komponist_model::DeviceId),
    OpenPreset(komponist_model::DeviceId),
    /// Choose a folder (S07 "+ Ordner").
    Folder,
    /// Choose the export folder (S08 "Ändern").
    ExportFolder,
    /// Choose a file for a plugin's declared asset slot.
    PluginAsset {
        device: komponist_model::DeviceId,
        slot: u32,
        kind: komponist_ipc::device_assets::AssetKind,
    },
    /// Import linked audio files, with multiple selection.
    ImportAudio,
    /// Relink one audio clip using a supported source file.
    ReplaceAudio(komponist_model::ClipId),
}

impl Purpose {
    fn is_save(self) -> bool {
        matches!(self, Self::Save | Self::SavePreset(_))
    }

    /// Whether a folder is chosen (the list shows folders only).
    pub fn is_folder(self) -> bool {
        matches!(self, Purpose::Folder | Purpose::ExportFolder)
    }
}

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File or folder name.
    pub name: String,
    /// Full path.
    pub path: PathBuf,
    /// A folder.
    pub is_dir: bool,
}

/// What the user did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// The file to open or save, or the folder.
    Chosen(PathBuf),
    /// Audio files in list order.
    ChosenMany(Vec<PathBuf>),
    /// Cancelled.
    Cancel,
}

/// State of an open file browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBrowser {
    /// The language of the messages the browser makes ([`FileBrowser::show`](show) follows the
    /// UI's).
    pub lang: Lang,
    /// What it is for.
    pub purpose: Purpose,
    /// The folder shown.
    pub dir: PathBuf,
    /// Its rows.
    pub entries: Vec<Entry>,
    /// The highlighted row.
    pub selected: Option<usize>,
    /// Multi-selection for audio import; directories are never included.
    pub multi_selected: BTreeSet<usize>,
    /// The file name (saving).
    pub name: String,
    /// The folder field's text.
    pub location: String,
    /// Saving would replace this file; the next confirmation does it.
    pub replace: Option<PathBuf>,
    /// A message under the list (folder not readable, file exists).
    pub message: Option<String>,
    /// Focus the file name field in the next frame.
    focus_name: bool,
}

impl FileBrowser {
    /// A browser for `purpose` showing `dir`, with `name` in the file name field (saving); its
    /// messages are in `lang`.
    pub fn new(lang: Lang, purpose: Purpose, dir: PathBuf, name: String) -> FileBrowser {
        let mut b = FileBrowser {
            lang,
            purpose,
            dir: dir.clone(),
            entries: Vec::new(),
            selected: None,
            multi_selected: BTreeSet::new(),
            name,
            location: super::format::tilde(&dir),
            replace: None,
            message: None,
            focus_name: purpose.is_save(),
        };
        b.go(dir);
        b
    }

    /// Title of the dialog.
    pub fn title(&self, t: Lang) -> &'static str {
        match self.purpose {
            Purpose::Open => t.file_open_title(),
            Purpose::Save => t.file_save_title(),
            Purpose::SavePreset(_) => t.devices_preset_save_file(),
            Purpose::OpenPreset(_) => t.devices_preset_load_file(),
            Purpose::Folder => t.file_folder_title(),
            Purpose::ExportFolder => t.export_folder_title(),
            Purpose::PluginAsset { .. } => t.plugin_file_choose(),
            Purpose::ImportAudio => t.menu_import_audio(),
            Purpose::ReplaceAudio(_) => t.audio_file_replace(),
        }
    }

    /// Label of the primary button.
    pub fn primary(&self, t: Lang) -> &'static str {
        match self.purpose {
            Purpose::Open => t.file_open(),
            Purpose::Save | Purpose::SavePreset(_) if self.replace.is_some() => t.file_replace(),
            Purpose::Save | Purpose::SavePreset(_) => t.file_save(),
            Purpose::Folder | Purpose::ExportFolder => t.file_choose_folder(),
            Purpose::PluginAsset { .. } | Purpose::ReplaceAudio(_) | Purpose::OpenPreset(_) => {
                t.file_load()
            }
            Purpose::ImportAudio => t.audio_import_confirm(),
        }
    }

    /// Shows folder `dir`; an unreadable folder keeps the current list and says why.
    pub fn go(&mut self, dir: PathBuf) {
        match list(&dir, self.purpose) {
            Ok(entries) => {
                self.entries = entries;
                self.dir = dir;
                self.selected = None;
                self.multi_selected.clear();
                self.message = None;
                self.replace = None;
            }
            Err(e) => {
                self.message = Some(self.lang.file_unreadable(dir.display(), e));
            }
        }
        self.location = super::format::tilde(&self.dir);
    }

    /// "‹": the parent folder. On Windows the parent of a drive root is the list of drives (an
    /// empty `dir`); there is nothing above that, and `/` has no parent elsewhere.
    pub fn parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.go(parent);
        } else if !self.dir.as_os_str().is_empty() && !crate::dirs::roots().is_empty() {
            self.go(PathBuf::new());
        }
    }

    /// Moves the highlight by `dir` rows (↑ ↓).
    pub fn step(&mut self, dir: i32) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let next = match self.selected {
            None if dir > 0 => 0,
            None => last,
            Some(i) => (i as i64 + i64::from(dir)).clamp(0, last as i64) as usize,
        };
        self.select(next);
    }

    /// Highlights row `index`; a file's name goes into the file name field when saving.
    pub fn select(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if self.purpose.is_save() && !entry.is_dir {
            self.name = entry.name.clone();
            self.replace = None;
        }
        self.selected = Some(index);
        self.multi_selected.clear();
        if self.purpose == Purpose::ImportAudio && !entry.is_dir {
            self.multi_selected.insert(index);
        }
    }

    /// Import-only Ctrl/Command toggle and Shift range, preserving the keyboard anchor.
    pub fn select_with(&mut self, index: usize, modifiers: Modifiers) {
        if self.purpose != Purpose::ImportAudio
            || !(modifiers.command || modifiers.ctrl || modifiers.shift)
        {
            self.select(index);
            return;
        }
        if self.entries.get(index).is_none_or(|e| e.is_dir) {
            self.select(index);
            return;
        }
        if modifiers.shift {
            let anchor = self.selected.unwrap_or(index);
            self.multi_selected.clear();
            self.multi_selected.extend(
                (anchor.min(index)..=anchor.max(index)).filter(|&i| !self.entries[i].is_dir),
            );
        } else if !self.multi_selected.remove(&index) {
            self.multi_selected.insert(index);
        }
        self.selected = Some(index);
    }

    /// Double-click or Enter on row `index`: a folder opens, a file is taken.
    pub fn activate(&mut self, index: usize) -> Option<Output> {
        let entry = self.entries.get(index)?.clone();
        if entry.is_dir {
            self.go(entry.path);
            return None;
        }
        match self.purpose {
            Purpose::Open
            | Purpose::OpenPreset(_)
            | Purpose::PluginAsset { .. }
            | Purpose::ReplaceAudio(_) => Some(Output::Chosen(entry.path)),
            Purpose::ImportAudio => Some(Output::ChosenMany(vec![entry.path])),
            Purpose::Save | Purpose::SavePreset(_) => {
                self.name = entry.name;
                self.confirm()
            }
            Purpose::Folder | Purpose::ExportFolder => None,
        }
    }

    /// The primary button (or Enter without a highlighted row).
    pub fn confirm(&mut self) -> Option<Output> {
        match self.purpose {
            Purpose::ImportAudio => {
                let paths: Vec<_> = self
                    .multi_selected
                    .iter()
                    .filter_map(|&i| self.entries.get(i))
                    .filter(|e| !e.is_dir)
                    .map(|e| e.path.clone())
                    .collect();
                if paths.is_empty() {
                    self.selected.and_then(|i| self.activate(i))
                } else {
                    Some(Output::ChosenMany(paths))
                }
            }
            Purpose::Open
            | Purpose::OpenPreset(_)
            | Purpose::PluginAsset { .. }
            | Purpose::ReplaceAudio(_) => {
                let i = self.selected?;
                self.activate(i)
            }
            Purpose::Folder | Purpose::ExportFolder => {
                let chosen = self
                    .selected
                    .and_then(|i| self.entries.get(i))
                    .filter(|e| e.is_dir)
                    .map_or_else(|| self.dir.clone(), |e| e.path.clone());
                // The list of drives (Windows) is no folder.
                (!chosen.as_os_str().is_empty()).then_some(Output::Chosen(chosen))
            }
            Purpose::Save | Purpose::SavePreset(_) => {
                let name = self.name.trim();
                if name.is_empty() || self.dir.as_os_str().is_empty() {
                    return None;
                }
                let path = self.dir.join(name);
                let path = if matches!(self.purpose, Purpose::SavePreset(_)) {
                    if is_user_preset_file(&path) {
                        path
                    } else {
                        let mut name = path.as_os_str().to_os_string();
                        name.push(".kppreset");
                        PathBuf::from(name)
                    }
                } else {
                    with_extension(&path)
                };
                if path.is_dir() {
                    self.go(path);
                    return None;
                }
                // A name with folders in it must name folders that exist; none are created.
                if let Some(parent) = path.parent()
                    && !parent.is_dir()
                {
                    self.message =
                        Some(self.lang.file_no_such_folder(super::format::tilde(parent)));
                    self.replace = None;
                    return None;
                }
                if path.exists() && self.replace.as_ref() != Some(&path) {
                    let file = path
                        .file_name()
                        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
                    self.message = Some(self.lang.file_exists(file));
                    self.replace = Some(path);
                    return None;
                }
                Some(Output::Chosen(path))
            }
        }
    }

    /// Enter in the folder field: a folder opens, a project file is taken (opening).
    pub fn go_to_location(&mut self) -> Option<Output> {
        let path = expand_tilde(self.location.trim());
        if path.is_dir() {
            self.go(path);
            None
        } else if matches!(
            self.purpose,
            Purpose::Open
                | Purpose::OpenPreset(_)
                | Purpose::PluginAsset { .. }
                | Purpose::ImportAudio
                | Purpose::ReplaceAudio(_)
        ) && path.is_file()
        {
            if self.purpose == Purpose::ImportAudio {
                Some(Output::ChosenMany(vec![path]))
            } else {
                Some(Output::Chosen(path))
            }
        } else {
            self.message = Some(self.lang.file_no_such_path(path.display()));
            None
        }
    }
}

fn is_user_preset_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("kppreset"))
}

/// Formats supported by the shared audio decoder, matched case-insensitively.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|extension| {
            ["wav", "wave", "flac", "aif", "aiff", "mp3", "ogg", "oga"]
                .iter()
                .any(|e| extension.eq_ignore_ascii_case(e))
        })
}

/// `~` and `~/…` with the home folder (on Windows `~\…` too).
fn expand_tilde(text: &str) -> PathBuf {
    expand_tilde_in(text, crate::dirs::home_dir())
}

/// [`expand_tilde`] for a given home folder.
fn expand_tilde_in(text: &str, home: Option<PathBuf>) -> PathBuf {
    let is_separator = |c: char| c == '/' || (cfg!(windows) && c == std::path::MAIN_SEPARATOR);
    match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(is_separator) => {
            home.join(rest.trim_start_matches(is_separator))
        }
        _ => PathBuf::from(text),
    }
}

/// The rows of folder `dir` for `purpose`: folders, then project files (not when choosing a
/// folder), each sorted by name without case; hidden entries are left out. An empty `dir` lists
/// the drives (Windows; [`crate::dirs::roots`]).
pub fn list(dir: &Path, purpose: Purpose) -> std::io::Result<Vec<Entry>> {
    if dir.as_os_str().is_empty() {
        return Ok(crate::dirs::roots()
            .into_iter()
            .map(|path| Entry {
                name: path.display().to_string(),
                path,
                is_dir: true,
            })
            .collect());
    }
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || is_hidden(&entry) {
            continue;
        }
        let path = entry.path();
        // Follows symlinks.
        let is_dir = path.is_dir();
        if is_dir {
            dirs.push(Entry { name, path, is_dir });
        } else if match purpose {
            Purpose::OpenPreset(_) | Purpose::SavePreset(_) => is_user_preset_file(&path),
            Purpose::PluginAsset { kind, .. } => {
                kind == komponist_ipc::device_assets::AssetKind::File || is_audio_file(&path)
            }
            Purpose::ImportAudio | Purpose::ReplaceAudio(_) => is_audio_file(&path),
            _ => !purpose.is_folder() && is_project_file(&path),
        } {
            files.push(Entry { name, path, is_dir });
        }
    }
    let key = |e: &Entry| e.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.extend(files);
    Ok(dirs)
}

/// Whether Windows marks the entry hidden (`$RECYCLE.BIN`, `AppData`); never on other systems,
/// where a leading dot does it.
fn is_hidden(entry: &std::fs::DirEntry) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        entry
            .metadata()
            .is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = entry;
        false
    }
}

/// Draws the browser; returns what the user did in this frame.
pub fn show(ctx: &Context, b: &mut FileBrowser) -> Option<Output> {
    let t = komponist_ui::lang(ctx);
    b.lang = t;
    let mut out = None;
    let size = super::fit(ctx, vec2(m::WIDTH, f32::INFINITY));
    let width = size.x;
    let response = modal(ctx, "file", color32(color::SURFACE_2), |ui| {
        ui.set_width(width);
        let title = b.title(t);
        ui.response()
            .widget_info(|| WidgetInfo::labeled(Role::Dialog, true, title));
        if header(ui, width, b.title(t)) {
            out = Some(Output::Cancel);
        }
        let body = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(egui::Margin::same(m::PADDING as i8));
        body.show(ui, |ui| {
            let inner = width - 2.0 * m::PADDING;
            ui.set_width(inner);
            ui.spacing_mut().item_spacing = vec2(space::SPACE_2, space::SPACE_2);
            if let Some(o) = location_row(ui, t, b) {
                out = Some(o);
            }
            if let Some(o) = file_list(ui, t, b, inner) {
                out = Some(o);
            }
            if b.purpose.is_save() && name_row(ui, t, b) {
                out = b.confirm();
            }
            if let Some(message) = &b.message {
                let ink = if b.replace.is_some() {
                    color::TEXT_2
                } else {
                    color::STATE_ERROR
                };
                let g = styled(ui, message, &typography::SMALL, color32(ink), Some(inner));
                super::paint_text(ui, g, message);
            }
        });
        let footer = egui::Frame::new().inner_margin(egui::Margin::symmetric(
            m::PADDING as i8,
            m::FOOTER_PADDING_Y as i8,
        ));
        footer.show(ui, |ui| {
            let r = button_row(
                ui,
                width - 2.0 * m::PADDING,
                vec![
                    ActionButton::dialog(t.dialog_cancel(), Kind::Secondary, 2),
                    ActionButton::dialog(b.primary(t), Kind::Primary, 2),
                ],
            );
            if r[0].clicked() {
                out = Some(Output::Cancel);
            }
            if r[1].clicked() {
                out = b.confirm();
            }
        });
    });
    if out.is_none() && response.should_close() {
        out = Some(Output::Cancel);
    }
    out
}

pub(super) fn field_label(ui: &mut Ui, text: &str) -> egui::Response {
    let g = styled(ui, text, &typography::UI, color32(color::TEXT_2), None);
    let (rect, _) =
        ui.allocate_exact_size(vec2(m::FIELD_LABEL_WIDTH, size::CONTROL), Sense::hover());
    let pos = w::text_pos(rect, &g, rect.left(), ui.pixels_per_point());
    ui.painter().galley(pos, g, Color32::PLACEHOLDER);
    label_node(ui, rect, text, Role::Label)
}

/// "Ordner", "‹" and the editable path.
fn location_row(ui: &mut Ui, t: Lang, b: &mut FileBrowser) -> Option<Output> {
    let mut out = None;
    ui.horizontal(|ui| {
        let label = field_label(ui, t.file_folder());
        if ui
            .add(Button::icon(Icon::ChevronLeft, t.file_parent()))
            .clicked()
        {
            b.parent();
        }
        let r = ui.add(TextField::new(&mut b.location, t.file_folder()).style(typography::VALUE_M));
        let r = r.labelled_by(label.id);
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            out = b.go_to_location();
        }
    });
    out
}

/// The list of folders and files.
fn file_list(ui: &mut Ui, t: Lang, b: &mut FileBrowser, width: f32) -> Option<Output> {
    let mut out = None;
    let typing = ui.ctx().egui_wants_keyboard_input();
    // Keys of the list (not while a text field has them, except Enter in the name field, which
    // the name row handles).
    if !typing {
        let (up, down, enter, back) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Backspace),
            )
        });
        if up {
            b.step(-1);
        }
        if down {
            b.step(1);
        }
        if back {
            b.parent();
        }
        if enter {
            out = if b.purpose == Purpose::ImportAudio {
                b.confirm()
            } else {
                match b.selected {
                    Some(i) => b.activate(i),
                    None => b.confirm(),
                }
            };
        }
    }
    let height = m::FILE_LIST_HEIGHT;
    let frame = egui::Frame::new()
        .fill(color32(color::SURFACE_2))
        .corner_radius(komponist_ui::theme::corner_radius(radius::M))
        .inner_margin(egui::Margin::same(space::SPACE_1 as i8));
    let builder = egui::UiBuilder::new()
        .accessibility_role(Role::List)
        .accessibility_label(t.file_list());
    ui.scope_builder(builder, |ui| {
        frame.show(ui, |ui| {
            let inner = width - 2.0 * space::SPACE_1;
            ui.set_width(inner);
            ui.set_height(height);
            let scroll_to = b.selected;
            egui::ScrollArea::vertical()
                .id_salt(("komponist_file_list", b.dir.display().to_string()))
                .auto_shrink([false, false])
                .max_height(height)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if b.entries.is_empty() {
                        let text = match b.purpose {
                            Purpose::Folder | Purpose::ExportFolder => t.file_no_folders(),
                            Purpose::ImportAudio => t.file_no_samples(),
                            Purpose::PluginAsset { .. } => t.plugin_file_none(),
                            Purpose::ReplaceAudio(_) => t.audio_file_no_audio(),
                            Purpose::OpenPreset(_) | Purpose::SavePreset(_) => {
                                t.devices_preset_files_empty()
                            }
                            _ => t.file_no_projects(),
                        };
                        let g = styled(ui, text, &typography::SMALL, color32(color::TEXT_3), None);
                        super::paint_text(ui, g, text);
                    }
                    let mut clicked = None;
                    let mut activated = None;
                    for (i, entry) in b.entries.iter().enumerate() {
                        let r = row(
                            ui,
                            inner,
                            entry,
                            b.selected == Some(i) || b.multi_selected.contains(&i),
                        );
                        if scroll_to == Some(i) && r.rect.height() > 0.0 {
                            r.scroll_to_me(None);
                        }
                        if r.double_clicked() {
                            activated = Some(i);
                        } else if r.clicked() {
                            clicked = Some(i);
                        }
                    }
                    if let Some(i) = clicked {
                        b.select_with(i, ui.input(|input| input.modifiers));
                    }
                    if let Some(i) = activated {
                        out = b.activate(i);
                    }
                });
        });
    });
    out
}

/// One row: folder icon (folders), name; highlighted rows `accent.surface`.
fn row(ui: &mut Ui, width: f32, entry: &Entry, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, size::MENU_ROW), Sense::click());
    let st = state::State::of(ui, &response);
    let fill = if selected {
        Some(color32(color::ACCENT_SURFACE))
    } else if st.hovered {
        Some(color32(color::SURFACE_3))
    } else {
        None
    };
    if let Some(fill) = fill {
        px::fill(ui.painter(), rect, radius::S, fill);
    }
    let ppp = ui.pixels_per_point();
    let mut x = rect.left() + space::SPACE_2;
    let icon = Rect::from_min_size(
        pos2(x, rect.center().y - size::ICON / 2.0),
        vec2(size::ICON, size::ICON),
    );
    if entry.is_dir {
        icons::paint(
            ui,
            px::snap_rect(icon, ppp),
            Icon::Folder,
            color32(color::TEXT_2),
        );
    }
    x = icon.right() + space::SPACE_2;
    let g = styled(
        ui,
        &entry.name,
        &typography::UI,
        color32(color::TEXT_1),
        None,
    );
    let clip = ui.painter().with_clip_rect(rect);
    clip.galley(w::text_pos(rect, &g, x, ppp), g, Color32::PLACEHOLDER);
    let name = entry.name.clone();
    response.widget_info(|| WidgetInfo::labeled(Role::ListItem, true, &name));
    w::accesskit(&response, |n| n.set_selected(selected));
    response
}

/// "Dateiname" and its field (saving); `true` when Enter was pressed in it (save).
fn name_row(ui: &mut Ui, t: Lang, b: &mut FileBrowser) -> bool {
    ui.horizontal(|ui| {
        let label = field_label(ui, t.file_name());
        let id = Id::unique("komponist_file_name");
        let r = ui
            .add(TextField::new(&mut b.name, t.file_name()).id(id))
            .labelled_by(label.id);
        if b.focus_name {
            r.request_focus();
            b.focus_name = false;
        }
        if r.changed() {
            b.replace = None;
            b.message = None;
        }
        r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("komponist-file-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_folders_then_projects_without_hidden_entries() {
        let dir = scratch("list");
        for d in ["b-ordner", "A-ordner", ".versteckt"] {
            std::fs::create_dir(dir.join(d)).unwrap();
        }
        for f in [
            "z.komponist",
            "Hafenlicht.komponist",
            "notiz.txt",
            ".x.komponist",
        ] {
            std::fs::write(dir.join(f), "{}").unwrap();
        }
        let names =
            |p| -> Vec<String> { list(&dir, p).unwrap().into_iter().map(|e| e.name).collect() };
        assert_eq!(
            names(Purpose::Open),
            [
                "A-ordner",
                "b-ordner",
                "Hafenlicht.komponist",
                "z.komponist"
            ]
        );
        assert_eq!(names(Purpose::Folder), ["A-ordner", "b-ordner"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tilde_stands_for_the_home_folder() {
        let home = PathBuf::from(if cfg!(windows) {
            r"C:\Users\kim"
        } else {
            "/home/kim"
        });
        assert_eq!(expand_tilde_in("~", Some(home.clone())), home);
        assert_eq!(
            expand_tilde_in("~/Musik", Some(home.clone())),
            home.join("Musik")
        );
        // `~kim` is no home shorthand; without a home folder nothing expands.
        assert_eq!(
            expand_tilde_in("~kim", Some(home.clone())),
            PathBuf::from("~kim")
        );
        assert_eq!(expand_tilde_in("~/Musik", None), PathBuf::from("~/Musik"));
    }

    /// Windows: `~\Musik` works too, and above a drive root the browser lists the drives.
    #[cfg(windows)]
    #[test]
    fn windows_has_backslashes_and_drives() {
        let home = Some(PathBuf::from(r"C:\Users\kim"));
        assert_eq!(
            expand_tilde_in(r"~\Musik", home),
            PathBuf::from(r"C:\Users\kim\Musik")
        );
        let drives = crate::dirs::roots();
        assert!(!drives.is_empty());
        let root = drives[0].clone();
        let mut b = FileBrowser::new(Lang::De, Purpose::Folder, root.clone(), String::new());
        assert_eq!(b.dir, root);
        // "‹" at a drive root: the list of drives; there is nothing above that.
        b.parent();
        assert!(b.dir.as_os_str().is_empty());
        assert_eq!(b.entries.len(), drives.len());
        assert!(b.entries.iter().all(|e| e.is_dir));
        b.parent();
        assert!(b.dir.as_os_str().is_empty());
        // The list of drives is no folder to choose; a drive is.
        assert_eq!(b.confirm(), None);
        b.select(0);
        assert_eq!(b.confirm(), Some(Output::Chosen(drives[0].clone())));
        // Opening a drive shows its folders.
        assert_eq!(b.activate(0), None);
        assert_eq!(b.dir, drives[0]);
    }

    #[test]
    fn navigation_and_choosing() {
        let dir = scratch("nav");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/Lied.komponist"), "{}").unwrap();
        let mut b = FileBrowser::new(Lang::De, Purpose::Open, dir.clone(), String::new());
        assert_eq!(b.entries.len(), 1);
        b.step(1);
        assert_eq!(b.selected, Some(0));
        // Enter on a folder opens it, on a file takes it.
        assert_eq!(b.confirm(), None);
        assert_eq!(b.dir, dir.join("sub"));
        b.step(1);
        assert_eq!(
            b.confirm(),
            Some(Output::Chosen(dir.join("sub/Lied.komponist")))
        );
        b.parent();
        assert_eq!(b.dir, dir);
        // A folder that cannot be read keeps the list and says why.
        b.go(dir.join("fehlt"));
        assert_eq!(b.dir, dir);
        assert!(b.message.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_adds_the_extension_and_asks_before_replacing() {
        let dir = scratch("save");
        let mut b = FileBrowser::new(Lang::De, Purpose::Save, dir.clone(), "Hafenlicht".into());
        assert_eq!(b.primary(Lang::De), "Speichern");
        assert_eq!(
            b.confirm(),
            Some(Output::Chosen(dir.join("Hafenlicht.komponist")))
        );
        std::fs::write(dir.join("Hafenlicht.komponist"), "{}").unwrap();
        assert_eq!(b.confirm(), None);
        assert_eq!(b.primary(Lang::De), "Ersetzen");
        assert!(b.message.as_deref().unwrap().contains("gibt es schon"));
        assert_eq!(
            b.confirm(),
            Some(Output::Chosen(dir.join("Hafenlicht.komponist")))
        );
        b.name = "  ".into();
        b.replace = None;
        assert_eq!(b.confirm(), None);
        // A name with folders that do not exist: nothing is created, the dialog says why.
        b.name = "Unbenannt.komponist/tmp/neu/Lied".into();
        assert_eq!(b.confirm(), None);
        assert!(b.message.as_deref().unwrap().starts_with("Den Ordner "));
        assert!(!dir.join("Unbenannt.komponist").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn choosing_a_folder_takes_the_highlighted_or_the_shown_one() {
        let dir = scratch("folder");
        std::fs::create_dir(dir.join("clap")).unwrap();
        let mut b = FileBrowser::new(Lang::De, Purpose::Folder, dir.clone(), String::new());
        assert_eq!(b.confirm(), Some(Output::Chosen(dir.clone())));
        b.step(1);
        assert_eq!(b.confirm(), Some(Output::Chosen(dir.join("clap"))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
