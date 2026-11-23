//! The app's side of crash recovery (#227; [`crate::recovery`], crash-recovery proposal (internal
//! notes) §2.3, §3): the copy of the open document each frame, the prompt at start and before a
//! project with a copy opens, and recovering a copy.
//!
//! - Every frame [`App::update_recovery`] tells the store whether the shown document has unsaved
//!   changes; it writes the copy in the background and deletes it when the document is clean, on
//!   the start screen (after „Nicht speichern“), when another document opens and on quit.
//! - At start ([`App::offer_recovery`], before the start screen, a project argument, a file from the
//!   desktop or `--demo` take over) the copies other sessions left are asked for one after the
//!   other, newest first, with „1 von 2“ in the header. „Verwerfen“ and „Später“ go on to the next
//!   copy and then to what was waiting (the project argument); „Wiederherstellen“ opens the copy
//!   instead and ends the round (the other copies wait for the start screen and the next start).
//! - Opening a project that has a waiting copy (a recent row, Enter, the file browser, a project
//!   argument) asks for that copy first; „Später“ and „Verwerfen“ then open the file.
//! - A recovered copy opens with the original path and name, dirty, its undo history starting
//!   there; Strg S writes the original file (variant d: asks for a place in the old folder). The
//!   copy keeps its id and stays until the first save.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use komponist_model::Document;

use crate::actions::{Action, ProjectAction};
use crate::app::App;
use crate::dialogs::recovery::{Answer, Model, Variant};
use crate::dialogs::start::RecentRow;
use crate::dialogs::{Overlay, Pending, format};
use crate::recovery::{Clock, DocumentState, FileState, Id, PanicHandle, Recovery, Store};
use crate::ui_state::{Screen, StatusKind};

/// The prompt's place in a round of copies and what waits behind it.
#[derive(Debug)]
pub(crate) struct Prompt {
    /// The copy asked for.
    pub(crate) id: Id,
    /// Its number in the round (1-based) and the round's size.
    pub(crate) counter: (usize, usize),
    /// The copies still to ask for in this round.
    pub(crate) rest: Vec<Id>,
    /// What continues after the round, unless a copy is recovered.
    pub(crate) after: Option<Pending>,
}

/// Recovery state of the app that is not the store's.
#[derive(Debug, Default)]
pub(crate) struct RecoveryUi {
    /// The open prompt.
    pub(crate) prompt: Option<Prompt>,
    /// The open document came from a copy whose project file is gone (variant d): Save asks
    /// for a place first.
    pub(crate) save_asks: bool,
    /// The start's scan could not read the folder: said after the round and what waited behind
    /// it, so an open does not replace it before anyone saw it.
    pub(crate) scan_error: Option<String>,
    /// `--open-plugin` waits for the document the start's round opens.
    pub(crate) open_plugin: Option<String>,
}

impl App {
    /// Recovery copies in `root` (`main`: [`crate::recovery::default_root`]; tests: a scratch
    /// folder), on the system clock.
    pub fn with_recovery(self, root: impl Into<PathBuf>) -> App {
        self.with_recovery_clock(root, Arc::new(Instant::now))
    }

    /// [`App::with_recovery`] on `clock` (tests move a fake clock).
    pub fn with_recovery_clock(mut self, root: impl Into<PathBuf>, clock: Clock) -> App {
        self.recovery = Recovery::new(Store::new(root), clock);
        self
    }

    /// The recovery state (tests).
    pub fn recovery(&self) -> &Recovery {
        &self.recovery
    }

    /// Waits until the recovery writer has done everything asked so far (tests).
    pub fn flush_recovery(&self, timeout: std::time::Duration) -> bool {
        self.recovery.flush(timeout)
    }

    /// What the panic hook needs ([`crate::recovery::install_panic_hook`]); call it on the UI
    /// thread. `None` while recovery is off.
    pub fn recovery_panic_handle(&self) -> Option<PanicHandle> {
        self.recovery.panic_handle()
    }

    /// One frame of the copy (module docs).
    pub(crate) fn update_recovery(&mut self, ctx: &egui::Context) {
        if !self.recovery.enabled() {
            return;
        }
        if !self.recovery.has_waker() {
            // A write that failed is retried after a backoff: the writer wakes an idle app.
            let ctx = ctx.clone();
            self.recovery.set_waker(move || ctx.request_repaint());
        }
        let dirty = self.ui.screen == Screen::Project
            && (self.doc.is_dirty() || !self.deferred_plugin_states().is_empty());
        let deferred = self.deferred_plugin_states().to_vec();
        let (failure, due) = self.recovery.update(DocumentState {
            dirty,
            project: self.doc.project(),
            deferred: &deferred,
            path: self.project_path.as_deref(),
        });
        if let Some(error) = failure {
            let reason =
                crate::project_io::ProjectIoError::Io(PathBuf::new(), error).reason(self.lang);
            self.set_status(
                self.lang.status_recovery_write_failed(reason),
                StatusKind::Error,
            );
        }
        if let Some(due) = due {
            ctx.request_repaint_after(due);
        }
    }

    /// At start: asks for the copies other sessions left, then opens `after` (the project
    /// argument). Without copies `after` opens at once (module docs).
    pub fn offer_recovery(&mut self, after: Option<PathBuf>) {
        let after = after.map(|path| Pending::OpenPath(std::path::absolute(&path).unwrap_or(path)));
        self.recovery.scan();
        if let Some(error) = self.recovery.take_scan_error() {
            let reason =
                crate::project_io::ProjectIoError::Io(PathBuf::new(), error).reason(self.lang);
            self.recovery_ui.scan_error = Some(self.lang.status_recovery_scan_failed(reason));
        }
        let mut ids: Vec<Id> = self
            .recovery
            .candidates
            .iter()
            .map(|c| c.id.clone())
            .collect();
        if ids.is_empty() {
            if let Some(next) = after {
                self.continue_after_recovery(next);
            }
            self.finish_round();
            return;
        }
        let total = ids.len();
        let first = ids.remove(0);
        self.ask_recovery(Prompt {
            id: first,
            counter: (1, total),
            rest: ids,
            after,
        });
    }

    /// The copy the open prompt asks for (tests).
    pub fn recovery_prompt_id(&self) -> Option<Id> {
        self.recovery_ui.prompt.as_ref().map(|p| p.id.clone())
    }

    /// Whether the recovery prompt is open.
    pub fn recovery_prompt_open(&self) -> bool {
        self.recovery_ui.prompt.is_some()
    }

    /// While the prompt is open, a file to open waits behind it (a document from the Dock at
    /// launch, #96, a file dropped on the start screen): `true` while the prompt is open. The first
    /// open that waits stays; a later one does not replace it.
    pub(crate) fn defer_behind_recovery(&mut self, path: &Path) -> bool {
        let Some(prompt) = &mut self.recovery_ui.prompt else {
            return false;
        };
        prompt
            .after
            .get_or_insert_with(|| Pending::OpenPath(path.to_path_buf()));
        true
    }

    /// Asks for the copy of `path` before opening it; `false` if it has none.
    pub(crate) fn ask_before_opening(&mut self, path: &Path) -> bool {
        if self.recovery_prompt_open() {
            return false;
        }
        let Some(id) = self.recovery.candidate_for(path).map(|c| c.id.clone()) else {
            return false;
        };
        self.ask_recovery(Prompt {
            id,
            counter: (1, 1),
            rest: Vec::new(),
            after: Some(Pending::OpenPath(path.to_path_buf())),
        });
        true
    }

    /// Asks for the copy `id` from its row on the start screen; „Später“ and „Verwerfen“ then open
    /// its project file, if it has one.
    pub(crate) fn ask_recovery_of(&mut self, id: Id) {
        if self.recovery_prompt_open() {
            return;
        }
        let Some(candidate) = self.recovery.candidate(&id) else {
            return;
        };
        let after = candidate
            .project_path()
            .map(|path| Pending::OpenPath(path.to_path_buf()));
        self.ask_recovery(Prompt {
            id,
            counter: (1, 1),
            rest: Vec::new(),
            after,
        });
    }

    fn ask_recovery(&mut self, prompt: Prompt) {
        self.recovery_ui.prompt = Some(prompt);
        self.open_overlay(Overlay::Recovery);
    }

    /// The prompt's model, `None` without a prompt.
    pub(crate) fn recovery_model(&self) -> Option<Model> {
        let prompt = self.recovery_ui.prompt.as_ref()?;
        let candidate = self.recovery.candidate(&prompt.id)?;
        let t = self.lang;
        let now = format::local(SystemTime::now());
        let when = |time: SystemTime| match (format::local(time), now) {
            (Some(then), Some(now)) => format::day_and_time(t, &then, &now),
            _ => String::new(),
        };
        let state = candidate.file_state();
        let untitled = state == FileState::Never;
        let variant = match (&candidate.loaded, state) {
            (Err(error), _) => Variant::Unreadable {
                reason: error.reason(t),
                untitled,
            },
            (Ok(_), FileState::Never) => Variant::Untitled,
            (Ok(_), FileState::Gone) => Variant::FileGone,
            (Ok(_), FileState::Saved { changed: true, .. }) => Variant::FileNewer,
            (Ok(_), FileState::Saved { .. }) => Variant::Saved,
        };
        let file_time = match state {
            FileState::Saved { modified, .. } => Some(when(modified)),
            _ => None,
        };
        let (n, total) = prompt.counter;
        Some(Model {
            variant,
            name: candidate
                .name()
                .map_or_else(|| t.new_project_default_name().to_owned(), str::to_owned),
            copy_time: when(candidate.written()),
            file_time,
            path: candidate.project_path().map(format::tilde),
            counter: (total > 1).then_some((n, total)),
        })
    }

    /// The prompt was answered (module docs).
    pub(crate) fn answer_recovery(&mut self, answer: Answer) {
        let Some(prompt) = self.recovery_ui.prompt.take() else {
            return;
        };
        if matches!(self.dialogs.overlay, Some(Overlay::Recovery)) {
            self.close_overlay();
        }
        match answer {
            Answer::Recover => {
                self.recover(&prompt.id);
                self.finish_round();
                return;
            }
            Answer::Discard => {
                if let Err(error) = self.recovery.discard(&prompt.id) {
                    eprintln!("komponist: recovery copy not discarded: {error}");
                }
            }
            Answer::Later => {}
        }
        let mut rest = prompt.rest;
        // A copy may have gone meanwhile (recovered or discarded through another way).
        rest.retain(|id| self.recovery.candidate(id).is_some());
        if rest.is_empty() {
            if let Some(next) = prompt.after {
                self.continue_after_recovery(next);
            }
            self.finish_round();
            return;
        }
        let next = rest.remove(0);
        self.ask_recovery(Prompt {
            id: next,
            counter: (prompt.counter.0 + 1, prompt.counter.1),
            rest,
            after: prompt.after,
        });
    }

    /// `--open-plugin NAME` at start: the plugin window of the first device whose plugin is
    /// `NAME`, in the document that opens. While the recovery prompt delays the start, it waits
    /// for the document the round opens (a recovered copy or the project argument).
    pub fn open_plugin_at_start(&mut self, name: String) {
        if self.recovery_prompt_open() {
            self.recovery_ui.open_plugin = Some(name);
        } else if !self.open_plugin_named(&name) {
            eprintln!("komponist: --open-plugin: no device with plugin \"{name}\"");
        }
    }

    /// The end of a round of prompts: what waited for the document that opened.
    fn finish_round(&mut self) {
        if let Some(name) = self.recovery_ui.open_plugin.take()
            && !self.open_plugin_named(&name)
        {
            eprintln!("komponist: --open-plugin: no device with plugin \"{name}\"");
        }
        self.report_scan_error();
    }

    /// The scan's failure, once the round and what waited behind it are done: an error of the open
    /// stays and the scan's follows it.
    fn report_scan_error(&mut self) {
        let Some(text) = self.recovery_ui.scan_error.take() else {
            return;
        };
        match self.ui.status.as_mut() {
            Some(status) if status.kind == StatusKind::Error => {
                status.text = format!("{} · {text}", status.text);
            }
            _ => self.set_status(text, StatusKind::Error),
        }
    }

    /// What waited behind the prompt; an open request goes straight to the file (its copy was
    /// asked for already).
    fn continue_after_recovery(&mut self, next: Pending) {
        match next {
            Pending::OpenPath(path) => self.open_path_now(path),
            other => self.proceed(other),
        }
    }

    /// Opens the copy `id` (§3.4): original path and name, dirty, the S06 check as for any
    /// opened project, the status naming the time and the file. The copy becomes this session's
    /// entry.
    fn recover(&mut self, id: &Id) {
        let Some(candidate) = self.recovery.take_candidate(id) else {
            return;
        };
        let Ok((project, repairs)) = &candidate.loaded else {
            return;
        };
        let state = candidate.file_state();
        let original = candidate.project_path().map(Path::to_path_buf);
        let written = candidate.written();
        let project_dir = original
            .as_deref()
            .and_then(Path::parent)
            .map(komponist_model::paths::absolute);
        let host = self.engine.host();
        let migration = host.migrate_project_for_open(
            project,
            project_dir.as_deref().unwrap_or_else(|| Path::new(".")),
        );
        let mut doc = Document::new(Arc::unwrap_or_clone(Arc::clone(project)));
        if let Ok(Some(migrated)) = migration {
            doc = Document::new(migrated);
        }
        // Nothing of it is in a file: unsaved until the first save.
        doc.forget_saved();
        let repaired = repairs.total();
        let current = Arc::clone(doc.project());
        self.open_document(doc, original.clone());
        if let Some(entry) = Store::adopt(candidate) {
            self.recovery.document_replaced(Some((entry, current)));
        }
        self.dialogs.repaired = repaired;
        self.recovery_ui.save_asks = state == FileState::Gone;
        if let Some(path) = original.as_deref() {
            if self.recovery_ui.save_asks {
                // Strg S asks for a place, in the old folder if it is still there.
                self.dialogs.last_dir = path.parent().filter(|d| d.is_dir()).map(Path::to_path_buf);
            } else {
                self.remember(path.to_path_buf());
            }
        }
        self.check_missing_plugins();
        {
            let t = self.lang;
            let time = match (format::local(written), format::local(SystemTime::now())) {
                (Some(then), Some(now)) => format::day_and_time(t, &then, &now),
                _ => String::new(),
            };
            let shortcut = self
                .keymap
                .label_for(&Action::Project(ProjectAction::Save), t)
                .unwrap_or_default();
            let text = match original.as_deref() {
                Some(_) if self.recovery_ui.save_asks => {
                    t.status_recovered_elsewhere(time, shortcut)
                }
                Some(path) => t.status_recovered(
                    time,
                    shortcut,
                    path.file_name()
                        .map_or_else(String::new, |f| f.to_string_lossy().into_owned()),
                ),
                None => t.status_recovered_untitled(time, shortcut),
            };
            // What the S06 check found (missing plugins or files) stays, after the recovery.
            match self.ui.status.as_mut() {
                Some(status) if status.kind == StatusKind::Error => {
                    status.text = format!("{text} · {}", status.text);
                }
                _ => self.set_status(text, StatusKind::Info),
            }
        }
    }

    /// The start screen's rows with the waiting copies (§3.5): a chip on the recent row of a
    /// project with a copy (the newest copy, the candidates come newest first); any other copy (an
    /// untitled project, a file that left the list, an older second copy of the same project) gets
    /// its own row at the top, newest first.
    pub(crate) fn recovery_rows(&self, rows: &mut Vec<RecentRow>, now: SystemTime) {
        let t = self.lang;
        let mut own = Vec::new();
        for candidate in &self.recovery.candidates {
            // Both paths come from this app as absolute paths; no file system access per frame.
            let listed = candidate.project_path().and_then(|path| {
                rows.iter_mut()
                    .find(|row| !row.copy_only && row.recovery.is_none() && row.path == path)
            });
            if let Some(row) = listed {
                row.recovery = Some(candidate.id.clone());
                continue;
            }
            let date = match (format::local(candidate.written()), format::local(now)) {
                (Some(then), Some(now)) => format::relative_date(t, &then, &now),
                _ => String::new(),
            };
            let name = candidate
                .name()
                .map_or_else(|| t.new_project_default_name().to_owned(), str::to_owned);
            let (meta, hues, missing) = match &candidate.loaded {
                Ok((project, _)) => (
                    crate::project_io::ProjectSummary::of(project, None).meta(t),
                    project.tracks.iter().map(|track| track.hue.0).collect(),
                    false,
                ),
                Err(error) => (error.reason(t), Vec::new(), true),
            };
            own.push(RecentRow {
                path: candidate
                    .project_path()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
                path_label: candidate
                    .project_path()
                    .map_or_else(|| t.recovery_file_never().to_owned(), format::tilde),
                name,
                meta,
                date,
                hues,
                missing,
                recovery: Some(candidate.id.clone()),
                copy_only: true,
            });
        }
        rows.splice(0..0, own);
    }
}
