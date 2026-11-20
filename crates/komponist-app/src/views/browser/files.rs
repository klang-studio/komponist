//! File-system rows; directory enumeration and header reads never run on the UI thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use komponist_i18n::Lang;
use komponist_model::AudioFile;

use super::model::{Row, RowKind};

#[derive(Debug, PartialEq)]
struct Entry {
    path: PathBuf,
    name: String,
    folded_name: String,
    directory: bool,
    file: Option<AudioFile>,
}

#[derive(Debug)]
struct CachedRows {
    query: String,
    language: Lang,
    rows: Arc<[Row]>,
}

#[derive(Debug, Default)]
pub(super) struct Files {
    directory: Option<PathBuf>,
    entries: Vec<Entry>,
    pending: Option<Receiver<Result<Vec<Entry>, String>>>,
    error: Option<String>,
    refreshed: Option<Instant>,
    cached_rows: Option<CachedRows>,
}

impl Files {
    pub fn rows(
        &mut self,
        ctx: &egui::Context,
        directory: PathBuf,
        t: Lang,
        query: &str,
    ) -> Arc<[Row]> {
        if self.directory.as_ref() != Some(&directory) {
            self.directory = Some(directory.clone());
            self.entries.clear();
            self.pending = None;
            self.refreshed = None;
            self.error = None;
            self.cached_rows = None;
        }
        if let Some(result) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            match result {
                Ok(entries) => {
                    // An empty result replaces the loading row even if there are no new entries.
                    if self.entries.is_empty() || self.entries != entries || self.error.is_some() {
                        self.entries = entries;
                        self.error = None;
                        self.cached_rows = None;
                    }
                }
                Err(error) => {
                    if self.error.as_ref() != Some(&error) {
                        self.error = Some(error);
                        self.cached_rows = None;
                    }
                }
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
            if self.entries.is_empty() && self.error.is_none() {
                self.cached_rows = None;
            }
        }
        if let Some(cached) = &self.cached_rows
            && cached.query == query
            && cached.language == t
        {
            return Arc::clone(&cached.rows);
        }
        let rows = Arc::from(self.build_rows(&directory, t, query));
        self.cached_rows = Some(CachedRows {
            query: query.to_owned(),
            language: t,
            rows: Arc::clone(&rows),
        });
        rows
    }

    fn build_rows(&self, directory: &std::path::Path, t: Lang, query: &str) -> Vec<Row> {
        let mut rows = vec![Row::new(RowKind::Section, t.browser_audio_files(), "", 0)];
        rows.push(Row::new(
            RowKind::Plain,
            crate::dialogs::format::tilde(directory),
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
            .filter(|entry| entry.folded_name.contains(&query))
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
            folded_name: name.to_lowercase(),
            name,
            directory,
            file,
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.folded_name.cmp(&b.folded_name))
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_directory_reuses_filtered_and_unfiltered_rows_until_the_inputs_change() {
        let ctx = egui::Context::default();
        let directory = std::env::temp_dir();
        let total = 20_000;
        let entries = (0..total)
            .map(|index| {
                let name = format!("Sample {index:05}.wav");
                let path = directory.join(&name);
                Entry {
                    file: Some(AudioFile::new(&path, None, 48_000, 48_000, 1)),
                    path,
                    folded_name: name.to_lowercase(),
                    name,
                    directory: false,
                }
            })
            .collect();
        let mut cache = Files {
            directory: Some(directory.clone()),
            entries,
            refreshed: Some(Instant::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        let mut get = |lang, query| cache.rows(&ctx, directory.clone(), lang, query);
        let fixed_rows = 2 + usize::from(directory.parent().is_some());
        let mut measure = |query| {
            let start = Instant::now();
            let cold = get(Lang::De, query);
            let build_time = start.elapsed();
            let start = Instant::now();
            for _ in 0..100 {
                assert!(Arc::ptr_eq(&cold, &get(Lang::De, query)));
            }
            eprintln!(
                "{total} entries, query {query:?}: initial rows {build_time:?}, cached frame {:?}",
                start.elapsed() / 100,
            );
            cold
        };
        let all = measure("");
        assert_eq!(all.len(), fixed_rows + total);

        let filtered = measure("SAMPLE 01999");
        assert_eq!(filtered.len(), fixed_rows + 1);
        assert_eq!(filtered.last().unwrap().name, "Sample 01999.wav");
        let english = get(Lang::En, "SAMPLE 01999");
        assert!(!Arc::ptr_eq(&filtered, &english));
        assert_eq!(english[0].name, Lang::En.browser_audio_files());

        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Vec::new())).unwrap();
        cache.pending = Some(receiver);
        let refreshed = cache.rows(&ctx, directory, Lang::En, "SAMPLE 01999");
        assert_eq!(
            refreshed.last().unwrap().name,
            Lang::En.browser_no_audio_files()
        );
    }
}
