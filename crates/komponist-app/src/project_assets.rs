//! Collect and Save for linked media and plugin assets.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use komponist_model::asset_collection::{Collector, is_audio_file, samples_folder_name};
use komponist_model::{AudioFile, ClipContent, Project};

use crate::app::App;
use crate::dialogs::file::Purpose;
use crate::ui_state::StatusKind;

impl App {
    /// "Sammeln und speichern": collects into the samples folder of the project file, then saves.
    pub(crate) fn collect_and_save(&mut self) {
        // A recovered copy whose file is gone has no file to collect next to (as Ctrl S).
        let file = self
            .project_path()
            .map(Path::to_path_buf)
            .filter(|_| !self.recovery_ui.save_asks);
        let Some(file) = file else {
            // The folder of the samples depends on the file: ask for it, then collect.
            self.dialogs.samples.collect_after_save = true;
            self.open_file_browser(Purpose::Save);
            return;
        };
        let mut collector = Collector::new(&file);
        if !self.save_to_collected(&file, &mut collector) {
            return;
        }
        for path in &collector.skipped {
            eprintln!(
                "komponist: not collected, not an audio file: {}",
                path.display()
            );
        }
        let save_warning = self
            .ui
            .status
            .as_ref()
            .filter(|status| status.kind == StatusKind::Error)
            .map(|status| status.text.clone());
        let copied = collector.log.iter().filter(|c| c.copied).count();
        let folder = samples_folder_name(&file);
        let text = match copied {
            0 => self.lang.status_collected_none(&folder),
            n => self.lang.status_collected(n, &folder),
        };
        let (mut text, mut kind) = if collector.skipped.is_empty() {
            (text, StatusKind::Success)
        } else {
            let files = collector
                .skipped
                .iter()
                .map(|p| p.file_name().unwrap_or(p.as_os_str()).to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!(
                    "{text} · {}",
                    self.lang
                        .status_collect_skipped(collector.skipped.len(), files)
                ),
                StatusKind::Error,
            )
        };
        if !collector.unapproved_assets.is_empty() {
            let files = collector
                .unapproved_assets
                .iter()
                .map(|path| {
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                })
                .collect::<Vec<_>>()
                .join(", ");
            let warning = self.lang.status_collect_unapproved(files);
            text = format!("{text} · {warning}");
            kind = StatusKind::Error;
        }
        if let Some(warning) = save_warning {
            text = format!("{warning} · {text}");
            kind = StatusKind::Error;
        }
        self.set_status(text, kind);
    }
}

/// All audio take files of `project`, including inactive passes, in track and clip order.
fn audio_files(project: &Project) -> Vec<&AudioFile> {
    project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .flat_map(|c| match &c.content {
            ClipContent::Audio(a) => a.takes.as_slice(),
            _ => &[],
        })
        .map(|take| &take.file)
        .collect()
}

/// Collect the current sample and audio-clip sources before rewriting the file or history.
pub(crate) fn collect_project_files(
    project: &Project,
    collector: &mut Collector,
    project_dir: Option<&Path>,
    approved_plugin_files: &HashSet<PathBuf>,
) -> std::io::Result<()> {
    for device in project
        .tracks
        .iter()
        .flat_map(|track| track.channel.devices.iter())
        .chain(project.master.devices.iter())
    {
        for link in device.asset_links.values() {
            if let Some(found) = link.resolve(project_dir) {
                let canonical = std::fs::canonicalize(&found)?;
                if is_audio_file(&found) || approved_plugin_files.contains(&canonical) {
                    collector.collect_asset_file(&found)?;
                } else if !(found.starts_with(collector.samples_dir())
                    && std::fs::canonicalize(collector.samples_dir())
                        .is_ok_and(|folder| canonical.starts_with(folder)))
                {
                    // Existing files in this project's collection need no copy. A stored
                    // link alone never authorizes copying another non-audio file.
                    collector.unapproved_assets.push(found);
                }
            }
        }
    }
    for file in audio_files(project) {
        if let Some(found) = file.resolve(project_dir) {
            collector.collect_file(&found)?;
        }
    }
    Ok(())
}
