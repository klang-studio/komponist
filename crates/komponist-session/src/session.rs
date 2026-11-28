//! Ownership of the open document and its live engine.

use std::path::{Path, PathBuf};

use komponist_engine::Command;
use komponist_model::{DeviceId, Document, Tick};

use crate::device_host::DeviceHost;
use crate::engine_sync::{DraftOverlay, EngineSync};

/// The open document, its file location, and the engine running its program.
pub struct Session {
    document: Document,
    project_path: Option<PathBuf>,
    engine: EngineSync,
}

impl Session {
    /// Starts with an unsaved document on the supplied engine.
    pub fn new(document: Document, engine: EngineSync) -> Self {
        Self {
            document,
            project_path: None,
            engine,
        }
    }

    /// The document and its edit history.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Edits the open document and its history.
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    /// The running engine and its device host.
    pub fn engine(&self) -> &EngineSync {
        &self.engine
    }

    /// Controls the engine and its device host.
    pub fn engine_mut(&mut self) -> &mut EngineSync {
        &mut self.engine
    }

    /// The file the current document was opened from or last saved to.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// The base for project-relative media and plugin assets.
    pub fn project_dir(&self) -> Option<&Path> {
        self.project_path().and_then(Path::parent)
    }

    /// Replaces the device host and gives it the open project's base before the next sync.
    pub fn set_device_host(&mut self, host: Box<dyn DeviceHost>) {
        self.engine.set_host(host);
        if let Some(dir) = self.project_dir().map(Path::to_path_buf) {
            self.engine.host_mut().set_project_dir(Some(dir));
        }
    }

    /// Publishes the current document revision and its playback draft.
    pub fn sync(&mut self, draft: DraftOverlay<'_>) {
        self.engine.sync_with_draft(&self.document, draft);
    }

    /// Publishes the document without a playback draft, for offline rendering.
    pub fn sync_committed(&mut self) {
        self.engine.sync(&self.document);
    }

    /// Reloads the device from the current document.
    pub fn reload_device(&mut self, device: DeviceId) {
        self.engine.reload_device(self.document.project(), device);
    }

    /// Refreshes plugin automation bases before capturing plugin state for a save.
    pub fn refresh_snapshot_automation(&mut self) {
        self.engine
            .refresh_snapshot_automation(self.document.project());
    }

    /// Stops the old project, clears its runtime devices and events, and installs a new document.
    /// Callers finish or discard their take and close plugin windows before calling this.
    pub fn replace_project(&mut self, document: Document, path: Option<PathBuf>) {
        self.engine.send(Command::CancelAutomationWrite);
        self.engine.send(Command::Stop);
        self.engine.send(Command::Locate(Tick::ZERO));
        self.engine.reset();
        self.document = document;
        self.set_project_path(path);
    }

    /// Installs the rebased document after its file was published and updates the live links.
    /// The caller prepares the saved revision and history before handing over the document.
    pub fn accept_saved_project(&mut self, document: Document, path: PathBuf) {
        let previous = std::mem::replace(&mut self.document, document);
        self.set_project_path(Some(path));
        let project = self.document.project();
        let host = self.engine.host_mut();
        for device in project
            .tracks
            .iter()
            .flat_map(|track| &track.channel.devices)
            .chain(&project.master.devices)
        {
            if let Some((_, _, old)) = previous.project().find_device(device.id)
                && old.asset_links != device.asset_links
            {
                host.sync_asset_links(device.id, &old.asset_links, &device.asset_links);
            }
        }
    }

    fn set_project_path(&mut self, path: Option<PathBuf>) {
        self.project_path = path;
        let dir = self.project_dir().map(Path::to_path_buf);
        self.engine.host_mut().set_project_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::edits::{self, ProjectProp};
    use komponist_model::fixtures::demo_project;
    use komponist_plugins::PluginManagerConfig;

    use crate::device_host::PluginHost;

    #[test]
    fn save_as_preserves_the_program_and_project_replacement_rebuilds_it() {
        let temp = tempfile::tempdir().unwrap();
        let host = PluginHost::without_bundled_clap(PluginManagerConfig::defaults(temp.path()));
        let mut session = Session::new(
            Document::new(demo_project()),
            EngineSync::null(Box::new(host)),
        );
        let device = session.document().project().tracks[0].channel.devices[0].id;
        session.sync(DraftOverlay::default());
        let slot = session
            .engine()
            .program()
            .unwrap()
            .slots
            .slot(device)
            .unwrap();
        let sent = session.engine().programs_sent();

        let path = temp.path().join("saved/Song.komponist");
        let mut saved = session.document().clone();
        saved
            .apply("Rename", |project| {
                edits::set_project_prop(project, ProjectProp::Name("Saved song".into()))
            })
            .unwrap();
        saved.mark_saved();
        session.accept_saved_project(saved, path.clone());
        assert_eq!(session.project_path(), Some(path.as_path()));
        assert_eq!(
            session.engine().host().project_dir().as_deref(),
            path.parent()
        );
        assert!(!session.document().is_dirty());
        assert_eq!(session.document().undo_label(), Some("Rename"));
        assert_eq!(
            session.engine().program().unwrap().slots.slot(device),
            Some(slot)
        );
        assert_eq!(session.engine().programs_sent(), sent);

        // Opening the same model with the same IDs and a fresh revision still retires the old program.
        let next = Document::new(demo_project());
        assert_eq!(next.project().tracks[0].channel.devices[0].id, device);
        let next_path = temp.path().join("other/Song.komponist");
        session.replace_project(next, Some(next_path.clone()));
        assert_eq!(session.project_path(), Some(next_path.as_path()));
        assert_eq!(
            session.engine().host().project_dir().as_deref(),
            next_path.parent()
        );
        assert!(session.engine().program().is_none());
        session.sync(DraftOverlay::default());
        assert!(
            session
                .engine()
                .program()
                .unwrap()
                .slots
                .slot(device)
                .is_some()
        );
        assert_eq!(session.engine().programs_sent(), sent + 1);
        session.engine_mut().shutdown();
    }
}
