//! The device chain's actions (WP-82): copy, duplicate, paste, delete, collapse, pages, the
//! neighbour selection of ← →, and the scroll requests of the mixer jump and the chain head.

use komponist_model::edits::{self, NewDevice};
use komponist_model::{ChannelRef, Device, DeviceId, DeviceRole, TrackId};

use super::App;
use crate::actions::{PluginAction, SelectMode, SelectionChange, UiAction};
use crate::ui_state::{DockTab, ParamTouch, Reveal};

/// A device as the spec of a copy: plugin and state, not bypassed.
fn spec(device: &Device) -> NewDevice {
    NewDevice {
        role: device.role,
        plugin: device.plugin.clone(),
        state: device.state.clone(),
    }
}

impl App {
    /// The selected track's chain, if exactly the first selected track exists.
    fn selected_chain(&self) -> Option<(TrackId, Vec<DeviceId>)> {
        let p = self.project();
        let track = self.ui.selection.tracks.iter().find_map(|t| p.track(*t))?;
        Some((
            track.id,
            track.channel.devices.iter().map(|d| d.id).collect(),
        ))
    }

    /// Inserts copies of `specs` from `index` on, as one undo step; selects them.
    fn insert_copies(
        &mut self,
        label: &str,
        channel: ChannelRef,
        index: usize,
        specs: Vec<NewDevice>,
    ) {
        if specs.is_empty() {
            return;
        }
        let ids = self.apply_with(label, |p| {
            let mut p = p.clone();
            let mut ids = Vec::with_capacity(specs.len());
            for (i, spec) in specs.into_iter().enumerate() {
                let (next, id) = edits::insert_device(&p, channel, index + i, spec)?;
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
        let specs = vec![spec(d)];
        let index = match (d.role, to) {
            (DeviceRole::Instrument, ChannelRef::Track(track)) => self
                .project()
                .track(track)
                .map_or(index, |t| t.channel.midi_effect_count()),
            _ => index,
        };
        self.insert_copies("undo-copy-device", to, index, specs);
    }

    /// Ctrl D: every selected effect copied behind itself.
    pub(super) fn duplicate_devices(&mut self) {
        let Some((track, chain)) = self.selected_chain() else {
            return;
        };
        let p = self.project().clone();
        let chosen: Vec<(usize, NewDevice)> = chain
            .iter()
            .enumerate()
            .filter(|(_, id)| self.ui.selection.devices.contains(id))
            .filter_map(|(i, id)| {
                let (_, _, d) = p.find_device(*id)?;
                (d.role != DeviceRole::Instrument).then(|| (i, spec(d)))
            })
            .collect();
        if chosen.is_empty() {
            return;
        }
        let ids = self.apply_with("undo-duplicate-devices", |p| {
            let mut p = p.clone();
            let mut ids = Vec::new();
            // Back to front, so the earlier indices stay valid.
            for (i, spec) in chosen.into_iter().rev() {
                let (next, id) = edits::insert_device(&p, ChannelRef::Track(track), i + 1, spec)?;
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
        let copied: Vec<Device> = chain
            .iter()
            .filter(|id| self.ui.selection.devices.contains(id))
            .filter_map(|id| p.find_device(*id).map(|(_, _, d)| (**d).clone()))
            .filter(|d| d.role != DeviceRole::Instrument)
            .collect();
        if !copied.is_empty() {
            // egui-winit sends `Event::Paste` for Strg V only while the system clipboard holds
            // text: put a short marker there; the devices stay in the app's own clipboard.
            self.shell.pending_clipboard_text = Some(match copied.len() {
                1 => "komponist: 1 Gerät".to_owned(),
                n => format!("komponist: {n} Geräte"),
            });
            self.ui.devices.clipboard = copied;
        }
    }

    /// Ctrl V: the copied devices behind the selected device, else at the chain's end.
    pub(super) fn paste_devices(&mut self) {
        let Some((track, chain)) = self.selected_chain() else {
            return;
        };
        let specs: Vec<NewDevice> = self.ui.devices.clipboard.iter().map(spec).collect();
        let index = chain
            .iter()
            .rposition(|id| self.ui.selection.devices.contains(id))
            .map_or(chain.len(), |i| i + 1);
        self.insert_copies("undo-paste-devices", ChannelRef::Track(track), index, specs);
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

    /// The UI actions of the device chain; `false` if `action` is not one of them.
    pub(super) fn device_ui_action(&mut self, action: &UiAction) -> bool {
        match action {
            UiAction::ToggleDeviceCollapsed { device, all } => {
                let collapse = !self.ui.devices.collapsed.contains(device);
                let ids = if *all {
                    self.chain_of(*device)
                } else {
                    vec![*device]
                };
                for id in ids {
                    if collapse {
                        self.ui.devices.collapsed.insert(id);
                    } else {
                        self.ui.devices.collapsed.remove(&id);
                    }
                }
            }
            UiAction::SetDevicePage { device, page } => {
                self.ui.devices.pages.insert(*device, *page);
            }
            UiAction::CopyDevices => self.copy_devices(),
            UiAction::SelectAdjacentDevice(dir) => {
                let Some((_, chain)) = self.selected_chain() else {
                    return true;
                };
                let current = chain
                    .iter()
                    .position(|id| self.ui.selection.devices.contains(id));
                let next = match current {
                    Some(i) => i
                        .saturating_add_signed(*dir as isize)
                        .min(chain.len().saturating_sub(1)),
                    None if *dir < 0 => chain.len().saturating_sub(1),
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
            UiAction::RevealDevice(device) => {
                let device = *device;
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
            UiAction::ScrollToDevice(device) => {
                self.ui.devices.reveal = Some((*device, Reveal::Start));
            }
            UiAction::TouchParam {
                device,
                param,
                dragging,
            } => {
                self.ui.devices.touched = Some(ParamTouch {
                    device: *device,
                    param: param.clone(),
                    dragging: *dragging,
                });
            }
            _ => return false,
        }
        true
    }
}
