//! Document edits and the file picker for CLAP asset slots.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use komponist_ipc::device_assets::{AssetKind, Binding, Bindings, MAX_PATH_BYTES, edit_state};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::project::SavedPluginState;
use komponist_model::{Device, DeviceId, edits};
use komponist_plugins::{AssetEdit, AssetEditPreparation};

use crate::app::App;
use crate::dialogs::file::{Purpose, is_audio_file};
use crate::ui_state::StatusKind;

pub(crate) struct PendingAssetEdit {
    request: u64,
    source: Arc<Device>,
    project_path: Option<PathBuf>,
    updates: BTreeMap<u32, Option<AssetLink>>,
    links: BTreeMap<u32, AssetLink>,
    reloads: Vec<Binding>,
    approved_files: Vec<PathBuf>,
}

impl PendingAssetEdit {
    fn matches_source(&self, current: &Device, project_path: Option<&Path>) -> bool {
        self.project_path.as_deref() == project_path
            && self.source.plugin == current.plugin
            && self.source.state == current.state
            && self.source.parameter_bases == current.parameter_bases
            && self.source.asset_links == current.asset_links
    }
}

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
        let pending = self
            .dialogs
            .samples
            .pending_asset_edits
            .get(&device)
            .filter(|edit| edit.matches_source(target, self.project_path()));
        let mut staged = pending.map_or_else(BTreeMap::new, |edit| edit.updates.clone());
        for (slot, link) in updates {
            if link.is_none() && !target.asset_links.contains_key(&slot) {
                staged.remove(&slot);
            } else {
                staged.insert(slot, link);
            }
        }
        if pending.is_some_and(|edit| edit.updates == staged) {
            return;
        }
        if staged.is_empty() {
            // Clearing an uncommitted assignment withdraws it without making a history step.
            if pending.is_some() {
                self.cancel_plugin_asset_edit(device);
            }
            return;
        }

        let dir = self.project_dir();
        let mut links = target.asset_links.clone();
        let mut changes = Vec::new();
        let mut reloads = Vec::new();
        let mut approved_files = Vec::new();
        for (&slot, link) in &staged {
            let previous = target.asset_links.get(&slot);
            let resolved = previous.and_then(|old| old.resolve(dir.as_deref()));
            changes.push(AssetEdit {
                slot,
                attaching: link.is_some(),
                relinking: previous.is_some() && resolved.is_none() && link.is_some(),
            });
            if let Some(link) = link {
                approved_files.extend(std::fs::canonicalize(&link.abs).ok());
                if resolved.as_ref() == Some(&link.abs)
                    && let Some(path) = link.abs.to_str()
                {
                    reloads.push(Binding {
                        id: slot,
                        path: Some(path.into()),
                        reload: true,
                    });
                }
                links.insert(slot, link.clone());
            } else {
                links.remove(&slot);
            }
        }
        // A bridge may still be waiting for the audio thread to deliver a knob turn.
        // The sampler journal has its persistent value and records it before the file edit.
        self.flush_sampler_params(device);
        let uses_prepare = self.engine.host().supports_asset_edit_state(device);
        if uses_prepare {
            if self.has_pending_sampler_params(device) {
                self.report_asset_edit_failed(device);
                return;
            }
            // Keep any preceding parameter gesture separate from the file edit's history.
            if self.doc.in_gesture() {
                self.end_gesture();
            }
        }
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        let source = Arc::clone(target);
        let project_path = self.project_path().map(Path::to_path_buf);
        self.cancel_plugin_asset_edit(device);
        let samples = &mut self.dialogs.samples;
        samples.asset_edit_request = samples.asset_edit_request.wrapping_add(1).max(1);
        let request = samples.asset_edit_request;
        let edit = PendingAssetEdit {
            request,
            source,
            project_path,
            updates: staged,
            links,
            reloads,
            approved_files,
        };
        match self
            .engine
            .host_mut()
            .prepare_asset_edit(&edit.source, &changes, request)
        {
            Ok(AssetEditPreparation::Ready(state)) => self.commit_plugin_asset_edit(edit, state),
            Ok(AssetEditPreparation::Pending) => {
                self.dialogs
                    .samples
                    .pending_asset_edits
                    .insert(device, edit);
            }
            Err(_) => self.report_asset_edit_failed(device),
        }
    }

    fn cancel_plugin_asset_edit(&mut self, device: DeviceId) {
        if self
            .dialogs
            .samples
            .pending_asset_edits
            .remove(&device)
            .is_some()
        {
            self.engine.host_mut().cancel_asset_edit(device);
        }
    }

    pub(crate) fn cancel_plugin_asset_edits(&mut self) {
        for (device, _) in std::mem::take(&mut self.dialogs.samples.pending_asset_edits) {
            self.engine.host_mut().cancel_asset_edit(device);
        }
    }

    pub(crate) fn finish_plugin_asset_edit(
        &mut self,
        device: DeviceId,
        request: u64,
        result: &Result<Option<SavedPluginState>, edit_state::Rejected>,
    ) {
        let pending = &mut self.dialogs.samples.pending_asset_edits;
        if pending
            .get(&device)
            .is_none_or(|edit| edit.request != request)
        {
            return;
        }
        let edit = pending.remove(&device).expect("matching asset edit");
        let Some((_, _, current)) = self.project().find_device(device) else {
            return;
        };
        if self.doc.in_gesture()
            || self.has_pending_sampler_params(device)
            || !edit.matches_source(current, self.project_path())
            || !self.engine.host().supports_asset_edit_state(device)
        {
            self.report_asset_edit_failed(device);
            return;
        }
        match result {
            Ok(state) => self.commit_plugin_asset_edit(edit, state.clone()),
            Err(edit_state::Rejected) => self.report_asset_edit_failed(device),
        }
    }

    fn report_asset_edit_failed(&mut self, device: DeviceId) {
        let name = self.device_name(device);
        self.set_status(self.lang.status_state_load_failed(&name), StatusKind::Error);
    }

    fn commit_plugin_asset_edit(
        &mut self,
        edit: PendingAssetEdit,
        state: Option<SavedPluginState>,
    ) {
        let device = edit.source.id;
        let applied = self.apply_with("undo-plugin-file-link", |project| {
            let mut project = edits::set_device_asset_links(project, device, edit.links)?;
            if let Some(state) = state {
                project = edits::set_saved_plugin_state(&project, device, &state)?;
            }
            Ok((project, ()))
        });
        if applied.is_some() {
            self.dialogs
                .samples
                .approved_plugin_files
                .extend(edit.approved_files);
            if !edit.reloads.is_empty() {
                self.engine.host_mut().set_asset_bindings(
                    device,
                    &Bindings {
                        slots: edit.reloads,
                    },
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_host::{BuiltinHost, DeviceHost};
    use crate::engine_sync::EngineSync;
    use komponist_engine::Processor;
    use komponist_model::{DeviceRole, Document, PluginRef};
    use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};

    struct PreparingHost(BuiltinHost);

    impl DeviceHost for PreparingHost {
        fn create_processor(
            &mut self,
            _device: &Device,
            _setup: ProcessorSetup,
        ) -> Option<Box<dyn Processor>> {
            None
        }

        fn release(&mut self, _device: DeviceId) {}

        fn status(&self, _device: DeviceId) -> DeviceStatus {
            DeviceStatus::Ok
        }

        fn latency(&self, _device: DeviceId) -> u32 {
            0
        }

        fn poll(&mut self) -> Vec<PluginManagerEvent> {
            Vec::new()
        }

        fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
            self.0.role(plugin)
        }

        fn catalog(&self) -> &Catalog {
            self.0.catalog()
        }

        fn supports_asset_edit_state(&self, _device: DeviceId) -> bool {
            true
        }

        fn prepare_asset_edit(
            &mut self,
            _device: &Device,
            _changes: &[AssetEdit],
            _request: u64,
        ) -> Result<AssetEditPreparation, DeviceId> {
            Ok(AssetEditPreparation::Pending)
        }
    }

    #[test]
    fn pending_asset_edits_merge_slots_and_survive_empty_redo() {
        use crate::actions::EditAction;

        let project = komponist_model::fixtures::demo_project();
        let device = project.tracks[0].channel.devices[0].id;
        let mut app = App::new(
            Document::new(project),
            EngineSync::null(Box::new(PreparingHost(BuiltinHost::new()))),
            Default::default(),
            None,
        );
        let first = AssetLink::new(Path::new("/samples/first.wav"), None);
        let second = AssetLink::new(Path::new("/samples/second.wav"), None);
        let replaced = AssetLink::new(Path::new("/samples/replaced.wav"), None);
        let cancelled = AssetLink::new(Path::new("/samples/cancelled.wav"), None);

        app.update_plugin_assets(device, [(0, Some(first))]);
        let stale = app.dialogs.samples.pending_asset_edits[&device].request;
        app.update_plugin_assets(device, [(1, Some(second.clone())), (2, Some(cancelled))]);
        app.update_plugin_assets(device, [(0, Some(replaced.clone())), (2, None)]);
        let request = app.dialogs.samples.pending_asset_edits[&device].request;
        assert_ne!(stale, request);

        app.dispatch(crate::Action::Edit(EditAction::Redo));
        assert_eq!(
            app.dialogs.samples.pending_asset_edits[&device].request,
            request
        );
        app.finish_plugin_asset_edit(device, stale, &Ok(None));
        assert!(app.doc.top_step().is_none());
        app.finish_plugin_asset_edit(device, request, &Ok(None));
        assert_eq!(
            app.project().find_device(device).unwrap().2.asset_links,
            [(0, replaced), (1, second)].into()
        );
        assert!(app.doc.undo());
        assert!(
            app.project()
                .find_device(device)
                .unwrap()
                .2
                .asset_links
                .is_empty()
        );
        assert!(app.doc.top_step().is_none());
    }

    #[test]
    fn stale_asset_preparations_leave_links_and_history_untouched() {
        let mut project = komponist_model::fixtures::demo_project();
        let track = Arc::make_mut(&mut project.tracks[0]);
        let entry = Arc::make_mut(&mut track.channel.devices[0]);
        entry.state = Some(Arc::from([1, 2]));
        let device = entry.id;
        let mut app = App::new(
            Document::new(project),
            EngineSync::null(Box::new(PreparingHost(BuiltinHost::new()))),
            Default::default(),
            None,
        );
        let result = Ok(Some(Arc::<[u8]>::from([3, 4]).into()));
        let first = AssetLink::new(Path::new("/samples/first.wav"), None);
        let second = AssetLink::new(Path::new("/samples/second.wav"), None);

        app.update_plugin_assets(device, [(0, Some(first.clone()))]);
        let cleared_request = app.dialogs.samples.pending_asset_edits[&device].request;
        app.update_plugin_assets(device, [(1, None)]);
        assert_eq!(
            app.dialogs.samples.pending_asset_edits[&device].request,
            cleared_request,
        );
        app.update_plugin_assets(device, [(0, None)]);
        assert!(
            !app.dialogs
                .samples
                .pending_asset_edits
                .contains_key(&device)
        );
        app.finish_plugin_asset_edit(device, cleared_request, &result);
        assert!(
            app.project()
                .find_device(device)
                .unwrap()
                .2
                .asset_links
                .is_empty()
        );
        assert!(app.doc.top_step().is_none());

        app.update_plugin_assets(device, [(0, Some(first))]);
        let first_request = app.dialogs.samples.pending_asset_edits[&device].request;
        app.update_plugin_assets(device, [(0, Some(second.clone()))]);
        let second_request = app.dialogs.samples.pending_asset_edits[&device].request;
        app.finish_plugin_asset_edit(device, first_request, &result);
        assert!(
            app.project()
                .find_device(device)
                .unwrap()
                .2
                .asset_links
                .is_empty()
        );
        assert_eq!(
            app.dialogs.samples.pending_asset_edits[&device].request,
            second_request
        );
        assert!(app.doc.top_step().is_none());

        let latest = Arc::<[u8]>::from([5, 6]).into();
        app.doc
            .apply("manual edit", |project| {
                edits::set_saved_plugin_state(project, device, &latest)
            })
            .unwrap();
        let manual_step = app.doc.top_step();
        app.finish_plugin_asset_edit(device, second_request, &result);
        let current = app.project().find_device(device).unwrap().2;
        assert!(current.asset_links.is_empty());
        assert!(current.has_saved_state(&latest));
        assert_eq!(app.doc.top_step(), manual_step);

        app.update_plugin_assets(device, [(0, Some(second.clone()))]);
        let valid_request = app.dialogs.samples.pending_asset_edits[&device].request;
        app.finish_plugin_asset_edit(device, valid_request, &result);
        let current = app.project().find_device(device).unwrap().2;
        assert_eq!(current.asset_links.get(&0), Some(&second));
        assert!(current.has_saved_state(result.as_ref().unwrap().as_ref().unwrap()));
        assert_ne!(app.doc.top_step(), manual_step);
        assert!(app.doc.undo());
        assert_eq!(app.doc.top_step(), manual_step);
        let current = app.project().find_device(device).unwrap().2;
        assert!(current.asset_links.is_empty());
        assert!(current.has_saved_state(&latest));
    }
}
