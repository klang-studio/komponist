//! File actions and previews for host-managed CLAP asset slots.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Context;
use komponist_engine::Command;
use komponist_model::plugin_assets::AssetLink;
use komponist_model::project::SavedPluginState;
use komponist_model::{ChannelRef, Device, DeviceId, TrackId, edits};
use komponist_plugin_api::device_assets::{
    AssetKind, Binding, Bindings, MAX_PATH_BYTES, edit_state,
};
use komponist_plugins::{AssetEdit, AssetEditPreparation, DeviceStatus};

use crate::app::App;
use crate::dialogs::file::{Purpose, is_audio_file};
use crate::ui_state::StatusKind;

#[derive(Default)]
pub(crate) struct State {
    /// Canonical plugin files the user explicitly picked or dropped for this document.
    pub(crate) approved_plugin_files: HashSet<PathBuf>,
    pending_asset_edits: HashMap<DeviceId, PendingAssetEdit>,
    /// Edits the device could not prepare yet, retried every frame until the deadline.
    waiting_asset_edits: HashMap<DeviceId, WaitingAssetEdit>,
    asset_edit_request: u64,
    /// Parameters edited since the device's last confirmed document snapshot.
    /// Devices that prepare their own file edits need this because those edits use saved state.
    unsnapshotted_params: HashMap<DeviceId, BTreeSet<u32>>,
    auditions: Vec<(Instant, TrackId, u8)>,
    waveform_preview: Option<WaveformPreview>,
}

impl State {
    /// Takes every waiting edit: those past their deadline (dropped for good) and those to retry.
    fn take_waiting_asset_edits(
        &mut self,
        now: Instant,
    ) -> (Vec<DeviceId>, Vec<(DeviceId, WaitingAssetEdit)>) {
        let (expired, retry) = std::mem::take(&mut self.waiting_asset_edits)
            .into_iter()
            .partition::<Vec<_>, _>(|(_, edit)| now >= edit.deadline);
        (
            expired.into_iter().map(|(device, _)| device).collect(),
            retry,
        )
    }

    pub(crate) fn forget_device_state(&mut self) {
        self.approved_plugin_files.clear();
        self.pending_asset_edits.clear();
        self.waiting_asset_edits.clear();
        self.unsnapshotted_params.clear();
        self.auditions.clear();
    }
}

/// How long a click on a pad holds its note (a gate pad needs a note off).
const AUDITION: Duration = Duration::from_millis(250);

/// Velocity of a pad click.
const AUDITION_VELOCITY: u8 = 100;

enum WaveformPreview {
    Sample { track: TrackId, key: u8 },
    Asset { device: DeviceId, slot: u32 },
}

/// How long an edit waits for a pending snapshot, preset or restore before it fails.
const ASSET_EDIT_WAIT: Duration = Duration::from_secs(5);

struct WaitingAssetEdit {
    updates: BTreeMap<u32, Option<AssetLink>>,
    deadline: Instant,
}

struct PendingAssetEdit {
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
            .plugin_assets
            .pending_asset_edits
            .get(&device)
            .filter(|edit| edit.matches_source(target, self.project_path()));
        let waiting = self.dialogs.plugin_assets.waiting_asset_edits.get(&device);
        let mut staged = pending
            .map(|edit| edit.updates.clone())
            .or_else(|| waiting.map(|edit| edit.updates.clone()))
            .unwrap_or_default();
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
            self.dialogs
                .plugin_assets
                .waiting_asset_edits
                .remove(&device);
            return;
        }
        self.begin_plugin_asset_edit(device, staged, None);
    }

    /// Retries the edits that waited for their device, and fails those past their deadline.
    pub(crate) fn retry_waiting_asset_edits(&mut self) {
        let (expired, retry) = self
            .dialogs
            .plugin_assets
            .take_waiting_asset_edits(Instant::now());
        for device in expired {
            self.report_asset_edit_failed(device);
        }
        for (device, edit) in retry {
            self.begin_plugin_asset_edit(device, edit.updates, Some(edit.deadline));
        }
    }

    /// Prepares and commits the staged file updates, or parks them while the device is busy.
    fn begin_plugin_asset_edit(
        &mut self,
        device: DeviceId,
        staged: BTreeMap<u32, Option<AssetLink>>,
        deadline: Option<Instant>,
    ) {
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
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
        let uses_prepare = self.engine.host().supports_asset_edit_state(device);
        if uses_prepare {
            // Keep any preceding parameter gesture separate from the file edit's history.
            if self.doc.in_gesture() {
                self.end_gesture();
            }
            // The device prepares from the document's saved state: a knob turn that has no
            // snapshot there yet would be lost, so the edit waits for it.
            if self.has_unsnapshotted_param_edits(device) {
                return self.wait_for_asset_edit(device, staged, deadline);
            }
        }
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        let source = Arc::clone(target);
        let project_path = self.project_path().map(Path::to_path_buf);
        self.cancel_plugin_asset_edit(device);
        let assets = &mut self.dialogs.plugin_assets;
        assets.asset_edit_request = assets.asset_edit_request.wrapping_add(1).max(1);
        let request = assets.asset_edit_request;
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
                    .plugin_assets
                    .pending_asset_edits
                    .insert(device, edit);
            }
            Ok(AssetEditPreparation::Busy) => {
                self.wait_for_asset_edit(device, edit.updates, deadline)
            }
            Err(_) => self.report_asset_edit_failed(device),
        }
    }

    fn wait_for_asset_edit(
        &mut self,
        device: DeviceId,
        updates: BTreeMap<u32, Option<AssetLink>>,
        deadline: Option<Instant>,
    ) {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + ASSET_EDIT_WAIT);
        self.dialogs
            .plugin_assets
            .waiting_asset_edits
            .insert(device, WaitingAssetEdit { updates, deadline });
    }

    fn cancel_plugin_asset_edit(&mut self, device: DeviceId) {
        if self
            .dialogs
            .plugin_assets
            .pending_asset_edits
            .remove(&device)
            .is_some()
        {
            self.engine.host_mut().cancel_asset_edit(device);
        }
    }

    pub(crate) fn cancel_plugin_asset_edits(&mut self) {
        self.dialogs.plugin_assets.waiting_asset_edits.clear();
        for (device, _) in std::mem::take(&mut self.dialogs.plugin_assets.pending_asset_edits) {
            self.engine.host_mut().cancel_asset_edit(device);
        }
    }

    pub(crate) fn finish_plugin_asset_edit(
        &mut self,
        device: DeviceId,
        request: u64,
        result: &Result<Option<SavedPluginState>, edit_state::Rejected>,
    ) {
        let pending = &mut self.dialogs.plugin_assets.pending_asset_edits;
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
            || self.has_unsnapshotted_param_edits(device)
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
                .plugin_assets
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

    /// A parameter edit of `device` went to the plugin; its snapshot is not in the document yet.
    pub(crate) fn remember_param_edit(&mut self, device: DeviceId, id: u32) {
        if self.engine.host().supports_asset_edit_state(device) {
            self.dialogs
                .plugin_assets
                .unsnapshotted_params
                .entry(device)
                .or_default()
                .insert(id);
        }
    }

    pub(crate) fn pending_param_bases(&self, device: DeviceId) -> BTreeMap<u32, f64> {
        let host = self.engine.host();
        self.dialogs
            .plugin_assets
            .unsnapshotted_params
            .get(&device)
            .into_iter()
            .flatten()
            .filter_map(|&id| {
                let value = host.param_base(device, id)?;
                value.is_finite().then_some((id, value))
            })
            .collect()
    }

    pub(crate) fn has_unsnapshotted_param_edits(&self, device: DeviceId) -> bool {
        self.dialogs
            .plugin_assets
            .unsnapshotted_params
            .contains_key(&device)
    }

    pub(crate) fn forget_param_edits(&mut self, device: DeviceId) {
        self.dialogs
            .plugin_assets
            .unsnapshotted_params
            .remove(&device);
    }

    /// A snapshot in the document that the host certifies as the current parameter bases
    /// settles the remembered edits.
    pub(crate) fn reconcile_param_edits(&mut self, device: DeviceId, state: &Arc<[u8]>) {
        if self.has_unsnapshotted_param_edits(device)
            && self.engine.host().is_current_param_snapshot(device, state)
        {
            self.forget_param_edits(device);
        }
    }

    pub(crate) fn audition_note(&mut self, device: DeviceId, key: u8) {
        let Some((ChannelRef::Track(track), _, _)) = self.project().find_device(device) else {
            return;
        };
        if key > 127 {
            return;
        }
        self.engine.send(Command::LiveNote {
            track,
            key,
            velocity: AUDITION_VELOCITY,
        });
        self.dialogs
            .plugin_assets
            .auditions
            .push((Instant::now() + AUDITION, track, key));
    }

    pub(crate) fn preview_waveform(
        &mut self,
        device: DeviceId,
        slot: u32,
        key: u8,
        time: Option<f64>,
    ) {
        let Some((ChannelRef::Track(track), _, found)) = self.project().find_device(device) else {
            return;
        };
        if key > 127
            || !found.asset_links.contains_key(&slot)
            || !self
                .engine
                .host()
                .asset_slots(device)
                .is_some_and(|assets| {
                    assets
                        .slots
                        .iter()
                        .any(|asset| asset.id == slot && asset.kind == AssetKind::AudioSample)
                })
            || self.engine.host().status(device) != DeviceStatus::Ok
            || komponist_plugin_api::waveform_preview::encode_time(time).is_none()
        {
            return;
        }
        self.stop_waveform_preview();
        if let Some(time) = time {
            self.engine
                .host()
                .preview_waveform(device, slot, Some(time));
            self.dialogs.plugin_assets.waveform_preview =
                Some(WaveformPreview::Asset { device, slot });
        } else {
            self.start_waveform_note(track, key);
        }
    }

    fn start_waveform_note(&mut self, track: TrackId, key: u8) {
        self.dialogs.plugin_assets.waveform_preview = Some(WaveformPreview::Sample { track, key });
        self.engine.send(Command::LiveNote {
            track,
            key,
            velocity: AUDITION_VELOCITY,
        });
    }

    pub(crate) fn stop_waveform_preview(&mut self) {
        match self.dialogs.plugin_assets.waveform_preview.take() {
            Some(WaveformPreview::Sample { track, key }) => {
                self.engine.send(Command::LiveNote {
                    track,
                    key,
                    velocity: 0,
                });
            }
            Some(WaveformPreview::Asset { device, slot }) => {
                self.engine.host().preview_waveform(device, slot, None);
            }
            None => {}
        }
    }

    /// Releases the audition notes that are due; asks for a frame when one is still held.
    pub(crate) fn release_auditions(&mut self, ctx: &Context) {
        if ctx.input(|input| !input.focused || !input.pointer.primary_down()) {
            self.stop_waveform_preview();
        }
        let now = Instant::now();
        let held = &mut self.dialogs.plugin_assets.auditions;
        let due: Vec<(TrackId, u8)> = held
            .iter()
            .filter(|(at, ..)| *at <= now)
            .map(|(_, t, k)| (*t, *k))
            .collect();
        held.retain(|(at, ..)| *at > now);
        let next = held.iter().map(|(at, ..)| *at).min();
        for (track, key) in due {
            self.engine.send(Command::LiveNote {
                track,
                key,
                velocity: 0,
            });
        }
        if let Some(at) = next {
            ctx.request_repaint_after(at.saturating_duration_since(now));
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

    #[test]
    fn a_busy_asset_edit_fails_once_after_its_deadline_and_is_not_retried() {
        let start = Instant::now();
        let mut state = State::default();
        let waiting = |deadline| WaitingAssetEdit {
            updates: BTreeMap::new(),
            deadline,
        };
        state
            .waiting_asset_edits
            .insert(DeviceId(1), waiting(start + ASSET_EDIT_WAIT));
        state
            .waiting_asset_edits
            .insert(DeviceId(2), waiting(start + ASSET_EDIT_WAIT * 2));

        let (expired, retry) = state.take_waiting_asset_edits(start + ASSET_EDIT_WAIT / 2);
        assert!(expired.is_empty());
        assert_eq!(retry.len(), 2);
        // The caller parks a still busy edit again with its original deadline.
        state.waiting_asset_edits.extend(retry);

        let (expired, retry) = state.take_waiting_asset_edits(start + ASSET_EDIT_WAIT);
        assert_eq!(expired, [DeviceId(1)]);
        assert_eq!(retry.len(), 1);
        state.waiting_asset_edits.extend(retry);

        let (expired, retry) = state.take_waiting_asset_edits(start + ASSET_EDIT_WAIT * 3);
        assert_eq!(expired, [DeviceId(2)]);
        assert!(retry.is_empty());
        let (expired, retry) = state.take_waiting_asset_edits(start + ASSET_EDIT_WAIT * 4);
        assert!(expired.is_empty() && retry.is_empty());
    }
    use komponist_model::{DeviceRole, Document, PluginFormat, PluginRef, Project};
    use komponist_plugins::{Catalog, DeviceStatus, PluginManagerEvent, ProcessorSetup};
    use komponist_processor::Processor;
    use komponist_session::device_host::{
        AssetHost, DevicePresentationHost, DeviceStateHost, OfflineRenderHost, ParameterHost,
        PluginCatalogHost, PluginWindowHost, PresetHost, ProcessorHost,
    };
    use komponist_session::engine_sync::EngineSync;

    struct PreparingHost {
        catalog: Catalog,
        assets: Option<komponist_plugin_api::device_assets::Snapshot>,
    }

    impl PreparingHost {
        fn new(assets: Option<komponist_plugin_api::device_assets::Snapshot>) -> Self {
            Self {
                catalog: Catalog::default(),
                assets,
            }
        }
    }

    fn asset_project() -> (Project, DeviceId) {
        let (project, track) = edits::add_track(
            &Project::new("Asset actions"),
            edits::NewTrack {
                instrument: Some(PluginRef {
                    format: PluginFormat::Clap,
                    id: "de.klang-studio.kp-test-assets".into(),
                    name: "Asset fixture".into(),
                    vendor: "Test".into(),
                    version: "1.0".into(),
                    module: "kp-test-assets.clap".into(),
                }),
                name: None,
                hue: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = project.track(track).unwrap().channel.devices[0].id;
        (project, device)
    }

    impl ProcessorHost for PreparingHost {
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
    }

    impl ParameterHost for PreparingHost {}

    impl DeviceStateHost for PreparingHost {}

    impl PluginCatalogHost for PreparingHost {
        fn role(&self, _plugin: &PluginRef) -> Option<DeviceRole> {
            Some(DeviceRole::Instrument)
        }

        fn catalog(&self) -> &Catalog {
            &self.catalog
        }
    }

    impl PresetHost for PreparingHost {}

    impl AssetHost for PreparingHost {
        fn asset_slots(
            &self,
            _device: DeviceId,
        ) -> Option<&komponist_plugin_api::device_assets::Snapshot> {
            self.assets.as_ref()
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

    impl OfflineRenderHost for PreparingHost {}

    impl PluginWindowHost for PreparingHost {}

    impl DevicePresentationHost for PreparingHost {}

    #[test]
    fn a_project_link_is_repaired_with_the_picker_before_the_inventory_arrives() {
        use crate::dialogs::Overlay;

        let (project, device) = asset_project();
        let dir = tempfile::tempdir().unwrap();
        let slot = 3;
        let missing = AssetLink::new(&dir.path().join("missing.wav"), None);
        let found = dir.path().join("found.wav");
        std::fs::write(&found, b"fixture").unwrap();
        let project =
            edits::set_device_asset_links(&project, device, [(slot, missing.clone())].into())
                .unwrap();
        let mut app = App::new(
            Document::new(project),
            EngineSync::null(Box::new(PreparingHost::new(None))),
            Default::default(),
            None,
        );

        app.pick_plugin_asset(device, slot);
        let Some(Overlay::File(browser)) = app.dialogs.overlay.as_ref() else {
            panic!("expected the file picker");
        };
        let purpose = browser.purpose;
        assert_eq!(
            purpose,
            Purpose::PluginAsset {
                device,
                slot,
                kind: AssetKind::File
            }
        );
        app.file_chosen(purpose, found.clone());
        let request = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
        app.finish_plugin_asset_edit(device, request, &Ok(None));

        let current = app.project().find_device(device).unwrap().2;
        assert_eq!(
            current.asset_links.get(&slot),
            Some(&AssetLink::new(&found, None))
        );
        assert!(app.doc.undo());
        let current = app.project().find_device(device).unwrap().2;
        assert_eq!(current.asset_links.get(&slot), Some(&missing));
    }

    #[test]
    fn preview_requires_a_declared_audio_slot() {
        use komponist_plugin_api::device_assets::{AssetStatus, Slot, Snapshot};

        let (project, device) = asset_project();
        let link = AssetLink::new(Path::new("/samples/example.wav"), None);
        let project = edits::set_device_asset_links(
            &project,
            device,
            [(0, link.clone()), (1, link.clone()), (2, link)].into(),
        )
        .unwrap();
        let asset = |id, kind| Slot {
            id,
            role: "sample".into(),
            label: "Sample".into(),
            kind,
            status: AssetStatus::Ready,
        };
        let assets = Snapshot {
            slots: vec![asset(1, AssetKind::File), asset(2, AssetKind::AudioSample)],
        };
        let mut app = App::new(
            Document::new(project),
            EngineSync::null(Box::new(PreparingHost::new(Some(assets)))),
            Default::default(),
            None,
        );

        for slot in [0, 1] {
            app.preview_waveform(device, slot, 60, Some(0.01));
            assert!(app.dialogs.plugin_assets.waveform_preview.is_none());
        }
        app.preview_waveform(device, 2, 60, Some(0.01));
        assert!(matches!(
            app.dialogs.plugin_assets.waveform_preview,
            Some(WaveformPreview::Asset { device: preview_device, slot: 2 }) if preview_device == device
        ));
        app.stop_waveform_preview();
        assert!(app.dialogs.plugin_assets.waveform_preview.is_none());
    }

    #[test]
    fn pending_asset_edits_merge_slots_and_survive_empty_redo() {
        use crate::actions::EditAction;

        let (project, device) = asset_project();
        let mut app = App::new(
            Document::new(project),
            EngineSync::null(Box::new(PreparingHost::new(None))),
            Default::default(),
            None,
        );
        let first = AssetLink::new(Path::new("/samples/first.wav"), None);
        let second = AssetLink::new(Path::new("/samples/second.wav"), None);
        let replaced = AssetLink::new(Path::new("/samples/replaced.wav"), None);
        let cancelled = AssetLink::new(Path::new("/samples/cancelled.wav"), None);

        app.update_plugin_assets(device, [(0, Some(first))]);
        let stale = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
        app.update_plugin_assets(device, [(1, Some(second.clone())), (2, Some(cancelled))]);
        app.update_plugin_assets(device, [(0, Some(replaced.clone())), (2, None)]);
        let request = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
        assert_ne!(stale, request);

        app.dispatch(crate::Action::Edit(EditAction::Redo));
        assert_eq!(
            app.dialogs.plugin_assets.pending_asset_edits[&device].request,
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
        let (mut project, device) = asset_project();
        let track = Arc::make_mut(&mut project.tracks[0]);
        let entry = Arc::make_mut(&mut track.channel.devices[0]);
        entry.state = Some(Arc::from([1, 2]));
        let mut app = App::new(
            Document::new(project),
            EngineSync::null(Box::new(PreparingHost::new(None))),
            Default::default(),
            None,
        );
        let result = Ok(Some(Arc::<[u8]>::from([3, 4]).into()));
        let first = AssetLink::new(Path::new("/samples/first.wav"), None);
        let second = AssetLink::new(Path::new("/samples/second.wav"), None);

        app.update_plugin_assets(device, [(0, Some(first.clone()))]);
        let cleared_request = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
        app.update_plugin_assets(device, [(1, None)]);
        assert_eq!(
            app.dialogs.plugin_assets.pending_asset_edits[&device].request,
            cleared_request,
        );
        app.update_plugin_assets(device, [(0, None)]);
        assert!(
            !app.dialogs
                .plugin_assets
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
        let first_request = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
        app.update_plugin_assets(device, [(0, Some(second.clone()))]);
        let second_request = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
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
            app.dialogs.plugin_assets.pending_asset_edits[&device].request,
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
        let valid_request = app.dialogs.plugin_assets.pending_asset_edits[&device].request;
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
