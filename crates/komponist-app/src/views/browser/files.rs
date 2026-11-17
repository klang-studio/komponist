//! File-system rows; directory enumeration and header reads never run on the UI thread.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use komponist_i18n::Lang;
use komponist_model::AudioFile;

use super::model::{Row, RowKind};

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    name: String,
    directory: bool,
    file: Option<AudioFile>,
}

#[derive(Debug, Default)]
pub(super) struct Files {
    directory: Option<PathBuf>,
    entries: Vec<Entry>,
    pending: Option<Receiver<Result<Vec<Entry>, String>>>,
    error: Option<String>,
    refreshed: Option<Instant>,
}

impl Files {
    pub fn rows(
        &mut self,
        ctx: &egui::Context,
        directory: PathBuf,
        t: Lang,
        query: &str,
    ) -> Vec<Row> {
        if self.directory.as_ref() != Some(&directory) {
            self.directory = Some(directory.clone());
            self.entries.clear();
            self.pending = None;
            self.refreshed = None;
            self.error = None;
        }
        if let Some(result) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            match result {
                Ok(entries) => {
                    self.entries = entries;
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
            self.pending = None;
            self.refreshed = Some(Instant::now());
        }
        if self.pending.is_none()
            && self
                .refreshed
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
        {
            let (tx, rx) = mpsc::channel();
            let dir = directory.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(read_directory(&dir));
                ctx.request_repaint();
            });
            self.pending = Some(rx);
        }
        let mut rows = vec![Row::new(RowKind::Section, t.browser_audio_files(), "", 0)];
        rows.push(Row::new(
            RowKind::Plain,
            crate::dialogs::format::tilde(&directory),
            "",
            0,
        ));
        if let Some(parent) = directory.parent() {
            rows.push(Row::new(
                RowKind::Directory(parent.to_path_buf()),
                t.browser_parent_folder(),
                "",
                0,
            ));
        }
        let query = query.to_lowercase();
        for entry in self
            .entries
            .iter()
            .filter(|entry| entry.name.to_lowercase().contains(&query))
        {
            if entry.directory {
                rows.push(Row::new(
                    RowKind::Directory(entry.path.clone()),
                    &entry.name,
                    "",
                    0,
                ));
            } else {
                let meta = entry.file.as_ref().map_or_else(
                    || t.browser_file_unreadable().to_owned(),
                    |file| {
                        let seconds = file.duration_secs().round() as u64;
                        let duration = format!("{:02}:{:02}", seconds / 60, seconds % 60);
                        let channels = match file.channels {
                            1 => t.browser_audio_mono().to_owned(),
                            2 => t.browser_audio_stereo().to_owned(),
                            n => t.browser_audio_channels(n),
                        };
                        t.browser_file_meta(duration, channels)
                    },
                );
                rows.push(Row::new(
                    RowKind::AudioFile {
                        path: entry.path.clone(),
                        file: entry.file.clone(),
                    },
                    &entry.name,
                    meta,
                    0,
                ));
            }
        }
        if let Some(error) = &self.error {
            rows.push(Row::new(
                RowKind::Plain,
                t.browser_files_unreadable(error),
                "",
                0,
            ));
        } else if self.entries.is_empty() {
            rows.push(Row::new(
                RowKind::Plain,
                if self.pending.is_some() {
                    t.browser_files_loading()
                } else {
                    t.browser_no_audio_files()
                },
                "",
                0,
            ));
        }
        rows
    }
}

fn read_directory(dir: &std::path::Path) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let directory = path.is_dir();
        if !directory && !crate::dialogs::file::is_audio_file(&path) {
            continue;
        }
        let file = if directory {
            None
        } else {
            crate::audio_import::linked_file(&path, None).ok()
        };
        entries.push(Entry {
            path,
            name,
            directory,
            file,
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}
