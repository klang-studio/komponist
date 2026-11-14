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

use std::path::{Path, PathBuf};

use egui::{Color32, Context, Id, Key, Modifiers, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::button::{Button, Kind};
use komponist_ui::widgets::{self as w};
use komponist_ui::{icons, px, state};

use super::metrics::dialog as m;
use super::{ActionButton, button_row, header, label_node, modal, styled};
use crate::project_io::{is_project_file, with_extension};

/// Texts of the file browser.
pub mod names {
    /// Title when opening.
    pub const OPEN_TITLE: &str = "Projekt öffnen";
    /// Title when saving.
    pub const SAVE_TITLE: &str = "Projekt speichern";
    /// Title when choosing a plugin folder.
    pub const FOLDER_TITLE: &str = "Plugin-Ordner hinzufügen";
    /// Primary action when opening.
    pub const OPEN: &str = "Öffnen";
    /// Primary action when saving.
    pub const SAVE: &str = "Speichern";
    /// Primary action when the file exists.
    pub const REPLACE: &str = "Ersetzen";
    /// Primary action when choosing a folder.
    pub const CHOOSE_FOLDER: &str = "Ordner wählen";
    /// "Abbrechen".
    pub const CANCEL: &str = "Abbrechen";
    /// Label of the folder field.
    pub const FOLDER: &str = "Ordner";
    /// The "‹" button.
    pub const PARENT: &str = "Übergeordneter Ordner";
    /// Label of the file name field.
    pub const FILE_NAME: &str = "Dateiname";
    /// The list.
    pub const LIST: &str = "Dateien";
    /// Empty list when opening or saving.
    pub const NO_PROJECTS: &str = "Keine Projekte in diesem Ordner";
    /// Empty list when choosing a folder.
    pub const NO_FOLDERS: &str = "Keine Unterordner";
}

/// What the browser is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Open a `.komponist` file.
    Open,
    /// Save under a file name.
    Save,
    /// Choose a folder (S07 "+ Ordner").
    Folder,
    /// Choose the export folder (S08 "Ändern").
    ExportFolder,
}

impl Purpose {
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
    /// Cancelled.
    Cancel,
}

/// State of an open file browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileBrowser {
    /// What it is for.
    pub purpose: Purpose,
    /// The folder shown.
    pub dir: PathBuf,
    /// Its rows.
    pub entries: Vec<Entry>,
    /// The highlighted row.
    pub selected: Option<usize>,
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
    /// A browser for `purpose` showing `dir`, with `name` in the file name field (saving).
    pub fn new(purpose: Purpose, dir: PathBuf, name: String) -> FileBrowser {
        let mut b = FileBrowser {
            purpose,
            dir: dir.clone(),
            entries: Vec::new(),
            selected: None,
            name,
            location: super::format::tilde(&dir),
            replace: None,
            message: None,
            focus_name: purpose == Purpose::Save,
        };
        b.go(dir);
        b
    }

    /// Title of the dialog.
    pub fn title(&self) -> &'static str {
        match self.purpose {
            Purpose::Open => names::OPEN_TITLE,
            Purpose::Save => names::SAVE_TITLE,
            Purpose::Folder => names::FOLDER_TITLE,
            Purpose::ExportFolder => super::export::names::FOLDER_TITLE,
        }
    }

    /// Label of the primary button.
    pub fn primary(&self) -> &'static str {
        match self.purpose {
            Purpose::Open => names::OPEN,
            Purpose::Save if self.replace.is_some() => names::REPLACE,
            Purpose::Save => names::SAVE,
            Purpose::Folder | Purpose::ExportFolder => names::CHOOSE_FOLDER,
        }
    }

    /// Shows folder `dir`; an unreadable folder keeps the current list and says why.
    pub fn go(&mut self, dir: PathBuf) {
        match list(&dir, self.purpose) {
            Ok(entries) => {
                self.entries = entries;
                self.dir = dir;
                self.selected = None;
                self.message = None;
                self.replace = None;
            }
            Err(e) => {
                self.message = Some(format!("{} lässt sich nicht lesen: {e}", dir.display()));
            }
        }
        self.location = super::format::tilde(&self.dir);
    }

    /// "‹": the parent folder.
    pub fn parent(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.go(parent);
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
        if self.purpose == Purpose::Save && !entry.is_dir {
            self.name = entry.name.clone();
            self.replace = None;
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
            Purpose::Open => Some(Output::Chosen(entry.path)),
            Purpose::Save => {
                self.name = entry.name;
                self.confirm()
            }
            Purpose::Folder | Purpose::ExportFolder => None,
        }
    }

    /// The primary button (or Enter without a highlighted row).
    pub fn confirm(&mut self) -> Option<Output> {
        match self.purpose {
            Purpose::Open => {
                let i = self.selected?;
                self.activate(i)
            }
            Purpose::Folder | Purpose::ExportFolder => {
                let chosen = self
                    .selected
                    .and_then(|i| self.entries.get(i))
                    .filter(|e| e.is_dir)
                    .map_or_else(|| self.dir.clone(), |e| e.path.clone());
                Some(Output::Chosen(chosen))
            }
            Purpose::Save => {
                let name = self.name.trim();
                if name.is_empty() {
                    return None;
                }
                let path = with_extension(&self.dir.join(name));
                if path.is_dir() {
                    self.go(path);
                    return None;
                }
                // A name with folders in it must name folders that exist; none are created.
                if let Some(parent) = path.parent()
                    && !parent.is_dir()
                {
                    self.message = Some(format!(
                        "Den Ordner {} gibt es nicht",
                        super::format::tilde(parent)
                    ));
                    self.replace = None;
                    return None;
                }
                if path.exists() && self.replace.as_ref() != Some(&path) {
                    let file = path
                        .file_name()
                        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
                    self.message = Some(format!("„{file}“ gibt es schon. Ersetzen?"));
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
        } else if self.purpose == Purpose::Open && path.is_file() {
            Some(Output::Chosen(path))
        } else {
            self.message = Some(format!("{} gibt es nicht", path.display()));
            None
        }
    }
}

/// `~` and `~/…` with the home folder.
fn expand_tilde(text: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    }
}

/// The rows of folder `dir` for `purpose`: folders, then project files (not when choosing a
/// folder), each sorted by name without case; hidden entries are left out.
pub fn list(dir: &Path, purpose: Purpose) -> std::io::Result<Vec<Entry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // Follows symlinks.
        let is_dir = path.is_dir();
        if is_dir {
            dirs.push(Entry { name, path, is_dir });
        } else if !purpose.is_folder() && is_project_file(&path) {
            files.push(Entry { name, path, is_dir });
        }
    }
    let key = |e: &Entry| e.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.extend(files);
    Ok(dirs)
}

/// Draws the browser; returns what the user did in this frame.
pub fn show(ctx: &Context, b: &mut FileBrowser) -> Option<Output> {
    let mut out = None;
    let size = super::fit(ctx, vec2(m::WIDTH, f32::INFINITY));
    let width = size.x;
    let response = modal(ctx, "file", color32(color::SURFACE_2), |ui| {
        ui.set_width(width);
        if header(ui, width, b.title()) {
            out = Some(Output::Cancel);
        }
        let body = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(egui::Margin::same(m::PADDING as i8));
        body.show(ui, |ui| {
            let inner = width - 2.0 * m::PADDING;
            ui.set_width(inner);
            ui.spacing_mut().item_spacing = vec2(space::SPACE_2, space::SPACE_2);
            if let Some(o) = location_row(ui, b) {
                out = Some(o);
            }
            if let Some(o) = file_list(ui, b, inner) {
                out = Some(o);
            }
            if b.purpose == Purpose::Save && name_row(ui, b) {
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
                    ActionButton::dialog(names::CANCEL, Kind::Secondary, 2),
                    ActionButton::dialog(b.primary(), Kind::Primary, 2),
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

fn field_label(ui: &mut Ui, text: &str) -> egui::Response {
    let g = styled(ui, text, &typography::UI, color32(color::TEXT_2), None);
    let (rect, _) =
        ui.allocate_exact_size(vec2(m::FIELD_LABEL_WIDTH, size::CONTROL), Sense::hover());
    let pos = w::text_pos(rect, &g, rect.left(), ui.pixels_per_point());
    ui.painter().galley(pos, g, Color32::PLACEHOLDER);
    label_node(ui, rect, text, Role::Label)
}

/// "Ordner", "‹" and the editable path.
fn location_row(ui: &mut Ui, b: &mut FileBrowser) -> Option<Output> {
    let mut out = None;
    ui.horizontal(|ui| {
        let label = field_label(ui, names::FOLDER);
        if ui
            .add(Button::icon(Icon::ChevronLeft, names::PARENT))
            .clicked()
        {
            b.parent();
        }
        let r = ui.add(
            TextField::new(&mut b.location, names::FOLDER)
                .style(typography::VALUE_M),
        );
        let r = r.labelled_by(label.id);
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            out = b.go_to_location();
        }
    });
    out
}

/// The list of folders and files.
fn file_list(ui: &mut Ui, b: &mut FileBrowser, width: f32) -> Option<Output> {
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
            out = match b.selected {
                Some(i) => b.activate(i),
                None => b.confirm(),
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
        .accessibility_label(names::LIST);
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
                            Purpose::Folder | Purpose::ExportFolder => names::NO_FOLDERS,
                            _ => names::NO_PROJECTS,
                        };
                        let g = styled(ui, text, &typography::SMALL, color32(color::TEXT_3), None);
                        super::paint_text(ui, g, text);
                    }
                    let mut clicked = None;
                    let mut activated = None;
                    for (i, entry) in b.entries.iter().enumerate() {
                        let r = row(ui, inner, entry, b.selected == Some(i));
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
                        b.select(i);
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
fn name_row(ui: &mut Ui, b: &mut FileBrowser) -> bool {
    ui.horizontal(|ui| {
        let label = field_label(ui, names::FILE_NAME);
        let id = Id::unique("komponist_file_name");
        let r = ui
            .add(TextField::new(&mut b.name, names::FILE_NAME).id(id))
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
    fn navigation_and_choosing() {
        let dir = scratch("nav");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/Lied.komponist"), "{}").unwrap();
        let mut b = FileBrowser::new(Purpose::Open, dir.clone(), String::new());
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
        let mut b = FileBrowser::new(Purpose::Save, dir.clone(), "Hafenlicht".into());
        assert_eq!(b.primary(), "Speichern");
        assert_eq!(
            b.confirm(),
            Some(Output::Chosen(dir.join("Hafenlicht.komponist")))
        );
        std::fs::write(dir.join("Hafenlicht.komponist"), "{}").unwrap();
        assert_eq!(b.confirm(), None);
        assert_eq!(b.primary(), "Ersetzen");
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
        let mut b = FileBrowser::new(Purpose::Folder, dir.clone(), String::new());
        assert_eq!(b.confirm(), Some(Output::Chosen(dir.clone())));
        b.step(1);
        assert_eq!(b.confirm(), Some(Output::Chosen(dir.join("clap"))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
