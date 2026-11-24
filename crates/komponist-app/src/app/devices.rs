//! The device chain's actions (WP-82): copy, duplicate, paste, delete, collapse, pages, the
//! neighbour selection of ← →, and the scroll requests of the mixer jump and the chain head.

use komponist_model::edits::{self, NewDevice};
use komponist_model::{ChannelRef, Device, DeviceId, DeviceRole, Project, TrackId};

use super::App;
use crate::actions::{PluginAction, SelectMode, SelectionChange};
use crate::ui_state::{CopiedDevice, DEVICES_CLIPBOARD_MARKER, DockTab, ParamTouch, Reveal};

/// A device as the spec of a copy: plugin, state and sidechain (kept where its source can still
/// feed the copy, #7), not bypassed.
fn spec(device: &Device) -> NewDevice {
    NewDevice {
        role: device.role,
        plugin: device.plugin.clone(),
        state: device.state.clone(),
        sidechain: device.sidechain,
    }
}

fn capture(project: &Project, device: &Device) -> CopiedDevice {
    CopiedDevice {
        device: device.clone(),
        lanes: edits::automation::device_lanes(project, device.id),
    }
}

fn insert_copy(
    project: &Project,
    channel: ChannelRef,
    index: usize,
    copy: CopiedDevice,
) -> Result<(Project, DeviceId), edits::EditError> {
    let (project, id) = edits::insert_device(project, channel, index, spec(&copy.device))?;
    let project = if !copy.device.parameter_bases.is_empty()
        && let Some(state) = copy.device.saved_state()
    {
        edits::set_saved_plugin_state(&project, id, &state)?
    } else {
        project
    };
    let project = if copy.device.asset_links.is_empty() {
        project
    } else {
        edits::set_device_asset_links(&project, id, copy.device.asset_links)?
    };
    Ok((
        edits::automation::paste_device_lanes(&project, id, &copy.lanes)?,
        id,
    ))
}

impl App {
    pub(super) fn remove_device(&mut self, device: DeviceId) {
        if self.apply("undo-remove-device", |p| edits::remove_device(p, device)) {
            self.ui.selection.devices.remove(&device);
        }
    }

    /// The selected track's chain, if exactly the first selected track exists.
    fn selected_chain(&self) -> Option<(TrackId, Vec<DeviceId>)> {
        let p = self.project();
        let track = self.ui.selection.tracks.iter().find_map(|t| p.track(*t))?;
        Some((
            track.id,
            track.channel.devices.iter().map(|d| d.id).collect(),
        ))
    }

    /// Inserts devices and their lanes from `index` on, as one undo step; selects them.
    fn insert_copies(
        &mut self,
        label: &str,
        channel: ChannelRef,
        index: usize,
        copies: Vec<CopiedDevice>,
    ) {
        if copies.is_empty() {
            return;
        }
        let ids = self.apply_with(label, |p| {
            let mut p = p.clone();
            let mut ids = Vec::with_capacity(copies.len());
            for (i, copy) in copies.into_iter().enumerate() {
                let (next, id) = insert_copy(&p, channel, index + i, copy)?;
                p = next;
                ids.push(id);
            }
            Ok((p, ids))
        });
        if let Some(ids) = ids {
            self.ui.selection.devices = ids.into_iter().collect();
        }
    }

    /// Alt-drag: a copy of `device` at `index` of `to` (`usize::MAX`: the end of its part of the
    /// chain). The chain keeps its order: a MIDI effect lands in the MIDI part, an effect behind the
    /// instrument; an instrument goes right behind the MIDI effects of a track that has none (a
    /// track with one refuses: one instrument per track).
    pub(super) fn copy_device(&mut self, device: DeviceId, to: ChannelRef, index: usize) {
        let Some((_, _, d)) = self.project().find_device(device) else {
            return;
        };
        let copies = vec![capture(self.project(), d)];
        let index = match (d.role, to) {
            (DeviceRole::Instrument, ChannelRef::Track(track)) => self
                .project()
                .track(track)
                .map_or(index, |t| t.channel.midi_effect_count()),
            _ => index,
        };
        self.insert_copies("undo-copy-device", to, index, copies);
    }

    /// Ctrl D: every selected effect copied behind itself.
    pub(super) fn duplicate_devices(&mut self) {
        let Some((track, chain)) = self.selected_chain() else {
            return;
        };
        let p = self.project().clone();
        let chosen: Vec<(usize, CopiedDevice)> = chain
            .iter()
            .enumerate()
            .filter(|(_, id)| self.ui.selection.devices.contains(id))
            .filter_map(|(i, id)| {
                let (_, _, d) = p.find_device(*id)?;
                (d.role != DeviceRole::Instrument).then(|| (i, capture(&p, d)))
            })
            .collect();
        if chosen.is_empty() {
            return;
        }
        let ids = self.apply_with("undo-duplicate-devices", |p| {
            let mut p = p.clone();
            let mut ids = Vec::new();
            // Back to front, so the earlier indices stay valid.
            for (i, copy) in chosen.into_iter().rev() {
                let (next, id) = insert_copy(&p, ChannelRef::Track(track), i + 1, copy)?;
                p = next;
                ids.push(id);
            }
            Ok((p, ids))
        });
        if let Some(ids) = ids {
            self.ui.selection.devices = ids.into_iter().collect();
        }
    }

    /// Ctrl C: remember the selected effects of the chain.
    pub(super) fn copy_devices(&mut self) {
        let Some((_, chain)) = self.selected_chain() else {
            return;
        };
        let p = self.project().clone();
        let source_dir = self.project_dir();
        let copied: Vec<CopiedDevice> = chain
            .iter()
            .filter(|id| self.ui.selection.devices.contains(id))
            .filter_map(|id| p.find_device(*id).map(|(_, _, d)| d))
            .filter(|d| d.role != DeviceRole::Instrument)
            .map(|d| {
                let mut copy = capture(&p, d);
                for link in copy.device.asset_links.values_mut() {
                    if let Some(source) = link.runtime_path(source_dir.as_deref()) {
                        *link = link.moved_to(&source, None);
                    } else {
                        link.rel.clear();
                    }
                }
                copy
            })
            .collect();
        if !copied.is_empty() {
            // egui-winit sends `Event::Paste` for Strg V only while the system clipboard holds
            // text: put a short marker there; the devices stay in the app's own clipboard.
            self.shell.pending_clipboard_text = Some(DEVICES_CLIPBOARD_MARKER.to_owned());
            self.ui.devices.clipboard = copied;
        }
    }

    /// Ctrl V: the copied devices behind the selected device, else at the chain's end.
    pub(super) fn paste_devices(&mut self) {
        let Some((track, chain)) = self.selected_chain() else {
            return;
        };
        let mut copies = self.ui.devices.clipboard.clone();
        let destination = self.project_dir();
        for copy in &mut copies {
            for link in copy.device.asset_links.values_mut() {
                if link.abs.is_absolute() {
                    *link = link.moved_to(&link.abs, destination.as_deref());
                }
            }
        }
        let index = chain
            .iter()
            .rposition(|id| self.ui.selection.devices.contains(id))
            .map_or(chain.len(), |i| i + 1);
        self.insert_copies(
            "undo-paste-devices",
            ChannelRef::Track(track),
            index,
            copies,
        );
    }

    /// Entf: remove the selected devices of the chain.
    pub(super) fn delete_devices(&mut self) {
        let Some((_, chain)) = self.selected_chain() else {
            return;
        };
        let doomed: Vec<DeviceId> = chain
            .into_iter()
            .filter(|id| self.ui.selection.devices.contains(id))
            .collect();
        if doomed.is_empty() {
            return;
        }
        let ok = self.apply("undo-remove-devices", |p| {
            let mut p = p.clone();
            for id in &doomed {
                p = edits::remove_device(&p, *id)?;
            }
            Ok(p)
        });
        if ok {
            for id in doomed {
                self.ui.selection.devices.remove(&id);
            }
        }
    }

    /// The device ids of the chain that holds `device`.
    fn chain_of(&self, device: DeviceId) -> Vec<DeviceId> {
        let p = self.project();
        match p.find_device(device) {
            Some((ChannelRef::Track(t), _, _)) => p
                .track(t)
                .map(|t| t.channel.devices.iter().map(|d| d.id).collect())
                .unwrap_or_default(),
            Some((ChannelRef::Master, _, _)) => p.master.devices.iter().map(|d| d.id).collect(),
            None => Vec::new(),
        }
    }

    /// `ToggleDeviceCollapsed`: collapse or expand a device; with `all` every device of its chain.
    pub(super) fn toggle_device_collapsed(&mut self, device: DeviceId, all: bool) {
        let collapse = !self.ui.devices.collapsed.contains(&device);
        let ids = if all {
            self.chain_of(device)
        } else {
            vec![device]
        };
        for id in ids {
            if collapse {
                self.ui.devices.collapsed.insert(id);
            } else {
                self.ui.devices.collapsed.remove(&id);
            }
        }
    }

    /// `SelectAdjacentDevice`: select the neighbour of the selected device.
    pub(super) fn select_adjacent_device(&mut self, dir: i32) {
        let Some((_, chain)) = self.selected_chain() else {
            return;
        };
        let current = chain
            .iter()
            .position(|id| self.ui.selection.devices.contains(id));
        let next = match current {
            Some(i) => i
                .saturating_add_signed(dir as isize)
                .min(chain.len().saturating_sub(1)),
            None if dir < 0 => chain.len().saturating_sub(1),
            None => 0,
        };
        if let Some(device) = chain.get(next).copied() {
            self.select(SelectionChange::Devices {
                devices: vec![device],
                mode: SelectMode::Replace,
            });
            self.ui.devices.reveal = Some((device, Reveal::Visible));
        }
    }

    /// `RevealDevice`: open the Geräte tab on the device's track and scroll it into the middle.
    pub(super) fn reveal_device(&mut self, device: DeviceId) {
        match self.project().find_device(device) {
            Some((ChannelRef::Track(track), _, _)) => {
                self.select(SelectionChange::Tracks {
                    tracks: vec![track],
                    mode: SelectMode::Replace,
                });
                self.select(SelectionChange::Devices {
                    devices: vec![device],
                    mode: SelectMode::Replace,
                });
                self.set_dock_tab(DockTab::Devices);
                self.ui.focus = self.dock_area();
                self.ui.devices.reveal = Some((device, Reveal::Center));
            }
            // The master has no tab in the dock: its plugin window.
            Some((ChannelRef::Master, _, _)) => {
                self.plugin_action(PluginAction::OpenWindow(device));
            }
            None => {}
        }
    }

    /// `TouchParam`: the inspector and the status line follow the touched parameter.
    pub(super) fn touch_param(
        &mut self,
        device: DeviceId,
        param: komponist_model::params::ParamId,
        dragging: bool,
    ) {
        let touch = ParamTouch {
            device,
            param: param.clone(),
            dragging,
        };
        let recent = &mut self.ui.devices.automation_recent;
        recent.retain(|old| old.device != device || old.param != param);
        recent.insert(0, touch.clone());
        recent.truncate(crate::views::automation::RECENT_TARGETS);
        self.ui.devices.touched = Some(touch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::plugin_assets::AssetLink;
    use komponist_model::{Document, PluginFormat, PluginRef};

    #[test]
    fn pasted_asset_keeps_the_copied_file_after_save_as_and_open() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        let destination = scratch.path().join("destination");
        for folder in [&source, &destination] {
            std::fs::create_dir(folder).unwrap();
            std::fs::write(
                folder.join("file.dat"),
                folder.as_os_str().as_encoded_bytes(),
            )
            .unwrap();
        }
        let file = source.join("file.dat");
        let track_spec = || edits::NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        };
        let (project, track) = edits::add_track(&Project::new("Source"), track_spec()).unwrap();
        let channel = ChannelRef::Track(track);
        let (project, device) = edits::insert_device(
            &project,
            channel,
            0,
            NewDevice {
                role: DeviceRole::Effect,
                plugin: PluginRef {
                    format: PluginFormat::Clap,
                    id: "com.example.assets".into(),
                    name: "Asset fixture".into(),
                    vendor: String::new(),
                    version: "1".into(),
                    module: Box::default(),
                },
                sidechain: None,
                state: None,
            },
        )
        .unwrap();
        let mut link = AssetLink::new(&file, Some(&source));
        link.abs = scratch.path().join("previous/file.dat");
        let project = edits::set_device_asset_links(&project, device, [(0, link)].into()).unwrap();
        let mut app = App::headless(Project::new("Empty"));
        app.open_document(
            Document::new(project),
            Some(source.join("source.komponist")),
        );
        app.ui.selection.tracks = [track].into_iter().collect();
        app.ui.selection.devices = [device].into_iter().collect();
        app.copy_devices();

        let saved = destination.join("save-as.komponist");
        assert!(app.save_to(&saved));
        app.paste_devices();
        let chain = &app.project().track(track).unwrap().channel.devices;
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain[1].asset_links[&0].resolve(Some(&destination)),
            Some(file.clone())
        );

        let (other, other_track) = edits::add_track(&Project::new("Other"), track_spec()).unwrap();
        app.open_document(
            Document::new(other),
            Some(destination.join("other.komponist")),
        );
        app.ui.selection.tracks = [other_track].into_iter().collect();
        app.paste_devices();
        let chain = &app.project().track(other_track).unwrap().channel.devices;
        assert_eq!(chain.len(), 1);
        assert_eq!(
            chain[0].asset_links[&0].resolve(Some(&destination)),
            Some(file)
        );
    }
}
