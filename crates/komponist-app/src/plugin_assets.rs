//! Document edits and the file picker for CLAP asset slots.

use std::path::{Path, PathBuf};

use komponist_ipc::device_assets::{AssetKind, Binding, Bindings, MAX_PATH_BYTES};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{DeviceId, edits};
use komponist_plugins::AssetEdit;

use crate::app::App;
use crate::dialogs::file::{Purpose, is_audio_file};
use crate::ui_state::StatusKind;

impl App {
    pub(crate) fn pick_plugin_asset(&mut self, device: DeviceId, slot: u32) {
        if let Some(kind) = self.plugin_asset_kind(device, slot) {
            self.open_file_browser(Purpose::PluginAsset { device, slot, kind });
        }
    }

    pub(crate) fn set_plugin_asset(&mut self, device: DeviceId, slot: u32, path: Option<&Path>) {
        let Some(kind) = self.plugin_asset_kind(device, slot) else {
            return;
        };
        let dir = self.project_dir();
        let link = match path {
            None => None,
            Some(path) => match selectable_link(path, kind, dir.as_deref()) {
                Some(link) => Some(link),
                None => return self.report_unavailable_plugin_file(),
            },
        };
        self.update_plugin_assets(device, [(slot, link)]);
    }

    pub(crate) fn drop_plugin_assets(&mut self, device: DeviceId, files: &[PathBuf]) {
        let Some(assets) = self.engine.host().asset_slots(device) else {
            return;
        };
        if assets.slots.len() != files.len() {
            return;
        }
        let dir = self.project_dir();
        let links = assets
            .slots
            .iter()
            .zip(files)
            .map(|(slot, path)| {
                let link = selectable_link(path, slot.kind, dir.as_deref())?;
                Some((slot.id, Some(link)))
            })
            .collect::<Option<Vec<_>>>();
        if let Some(links) = links {
            self.update_plugin_assets(device, links);
        } else {
            self.report_unavailable_plugin_file();
        }
    }

    fn report_unavailable_plugin_file(&mut self) {
        self.set_status(self.lang.plugin_file_unavailable(), StatusKind::Error);
    }

    fn update_plugin_assets(
        &mut self,
        device: DeviceId,
        updates: impl IntoIterator<Item = (u32, Option<AssetLink>)>,
    ) {
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        let dir = self.project_dir();
        let mut links = target.asset_links.clone();
        let mut changes = Vec::new();
        let mut reloads = Vec::new();
        for (slot, link) in updates {
            let previous = links.get(&slot);
            if previous.is_none() && link.is_none() {
                continue;
            }
            let resolved = previous.and_then(|old| old.resolve(dir.as_deref()));
            changes.push(AssetEdit {
                slot,
                attaching: link.is_some(),
                relinking: previous.is_some() && resolved.is_none() && link.is_some(),
            });
            if let Some(link) = link {
                if resolved.as_ref() == Some(&link.abs)
                    && let Some(path) = link.abs.to_str()
                {
                    reloads.push(Binding {
                        id: slot,
                        path: Some(path.into()),
                        reload: true,
                    });
                }
                links.insert(slot, link);
            } else {
                links.remove(&slot);
            }
        }
        if changes.is_empty() {
            return;
        }
        // A bridge may still be waiting for the audio thread to deliver a knob turn.
        // The sampler journal has its persistent value and records it before the file edit.
        self.flush_sampler_params(device);
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        let state = self.engine.host().prepare_asset_edit(target, &changes);
        let Ok(state) = state else {
            let name = self.device_name(device);
            self.set_status(self.lang.status_state_load_failed(&name), StatusKind::Error);
            return;
        };
        let applied = self.apply_with("undo-plugin-file-link", |project| {
            let mut project = edits::set_device_asset_links(project, device, links)?;
            if let Some(state) = state {
                project = edits::set_device_state(&project, device, Some(state))?;
            }
            Ok((project, ()))
        });
        if applied.is_some() && !reloads.is_empty() {
            self.engine
                .host_mut()
                .set_asset_bindings(device, &Bindings { slots: reloads });
        }
    }

    fn plugin_asset_kind(&self, device: DeviceId, slot: u32) -> Option<AssetKind> {
        if let Some(live) = self.engine.host().asset_slots(device)
            && let Some(slot) = live.slots.iter().find(|declared| declared.id == slot)
        {
            return Some(slot.kind);
        }
        // A project link can be repaired before the plugin finishes loading.
        self.project()
            .find_device(device)?
            .2
            .asset_links
            .contains_key(&slot)
            .then_some(AssetKind::File)
    }
}

fn selectable_link(path: &Path, kind: AssetKind, dir: Option<&Path>) -> Option<AssetLink> {
    let link = AssetLink::new(path, dir);
    (link.abs.is_file()
        && (kind != AssetKind::AudioSample || is_audio_file(&link.abs))
        && link
            .abs
            .to_str()
            .is_some_and(|path| path.len() <= MAX_PATH_BYTES))
    .then_some(link)
}
