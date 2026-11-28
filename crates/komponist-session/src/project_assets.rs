//! Project and history path rebasing for linked media and plugin assets.

use std::path::{Path, PathBuf};

use komponist_model::{Project, edits};

/// Rewrites a project or history entry for the destination folder while it still resolves
/// through the current project directory. Recording copies already have their final paths.
pub fn rebase_project_links(
    project: &Project,
    old_dir: Option<&Path>,
    new_dir: &Path,
    destination: impl Fn(PathBuf) -> PathBuf,
    asset_destination: impl Fn(PathBuf) -> PathBuf,
) -> Option<Project> {
    let after_samples = edits::rewrite_audio_files(project, |file| {
        let path = destination(file.resolve(old_dir)?);
        Some(file.moved_to(&path, new_dir))
    });
    let base = after_samples.as_ref().unwrap_or(project);
    let mut result = None;
    for device in base
        .tracks
        .iter()
        .flat_map(|track| &track.channel.devices)
        .chain(&base.master.devices)
    {
        if device.asset_links.is_empty() {
            continue;
        }
        let links = device
            .asset_links
            .iter()
            .map(|(&id, link)| {
                let link = link.runtime_path(old_dir).map_or_else(
                    || link.clone(),
                    |source| link.moved_to(&asset_destination(source), Some(new_dir)),
                );
                (id, link)
            })
            .collect();
        if links != device.asset_links {
            result = Some(
                edits::set_device_asset_links(result.as_ref().unwrap_or(base), device.id, links)
                    .ok()?,
            );
        }
    }
    result.or(after_samples)
}
