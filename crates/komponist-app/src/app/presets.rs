//! Browser preset selection. The new device enters the document with its initial state.

use super::*;

use crate::dialogs::file::Purpose;
use komponist_ipc::clap_presets::{ClapPresetSource, DevicePreset};
use komponist_plugins::PresetPreparation;
use serde::{Deserialize, Serialize};
use std::io::Read as _;
use std::sync::mpsc::{self, Receiver, TryRecvError};

const MAX_PRESET_STATE: usize = 16 * 1024 * 1024;
const MAX_PRESET_FILE: usize = 4 * MAX_PRESET_STATE + 64 * 1024;

#[derive(Serialize, Deserialize)]
struct UserPreset {
    version: u32,
    plugin_id: String,
    state: Vec<u8>,
}

#[derive(Default)]
pub(super) struct UserPresets {
    capture: Option<(DeviceId, String, PathBuf)>,
    read: Option<(DeviceId, Receiver<Result<UserPreset, String>>)>,
    write: Option<Receiver<Result<(), String>>>,
}

impl UserPresets {
    pub(super) fn is_busy(&self) -> bool {
        self.capture.is_some() || self.read.is_some() || self.write.is_some()
    }
}

fn file_job<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Receiver<Result<T, String>> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(work());
    });
    receive
}

fn file_result<T>(job: &Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match job.try_recv() {
        Ok(result) => Some(result),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err("preset file worker disconnected".into())),
    }
}

fn read_user_preset(path: &Path) -> Result<UserPreset, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take((MAX_PRESET_FILE + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_PRESET_FILE {
        return Err("preset file is too large".into());
    }
    let preset: UserPreset = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if preset.version != 1 || preset.state.len() > MAX_PRESET_STATE {
        return Err("unsupported user preset file".into());
    }
    Ok(preset)
}

pub(super) struct PendingPreset {
    preparation: Preparation,
    plugin: PluginRef,
    source: ClapPresetSource,
    role: DeviceRole,
    track: Option<TrackId>,
}

enum Preparation {
    Running(PresetPreparation),
    Ready(Result<Vec<u8>, String>),
}

impl Preparation {
    fn poll(&mut self) {
        if let Self::Running(job) = self
            && let Some(result) = job.try_finish()
        {
            *self = Self::Ready(result);
        }
    }
}

impl App {
    pub(super) fn add_from_preset(
        &mut self,
        plugin: PluginRef,
        source: ClapPresetSource,
        track: Option<TrackId>,
    ) {
        let Some(role) = self.engine.host().role(&plugin) else {
            return;
        };
        if role != DeviceRole::Instrument && track.is_none() {
            self.set_status(
                match role {
                    DeviceRole::MidiEffect => self.lang.status_midi_fx_need_track(),
                    _ => self.lang.status_fx_need_track(),
                },
                StatusKind::Error,
            );
            return;
        }
        let Some(job) = self.engine.host().prepare_clap_preset(&plugin, &source) else {
            self.set_status(self.lang.devices_preset_load_failed(), StatusKind::Error);
            return;
        };
        let preparation = Preparation::Running(job);
        self.preset_preparations.push(PendingPreset {
            preparation,
            plugin,
            source,
            role,
            track,
        });
        self.poll_preset_preparations();
    }

    pub(super) fn poll_preset_preparations(&mut self) {
        let mut blocked_tracks = std::collections::BTreeSet::new();
        for mut request in std::mem::take(&mut self.preset_preparations) {
            request.preparation.poll();
            let destination = request
                .track
                .filter(|_| request.role != DeviceRole::Instrument);
            if destination.is_some_and(|track| blocked_tracks.contains(&track))
                || matches!(request.preparation, Preparation::Running(_))
            {
                blocked_tracks.extend(destination);
                self.preset_preparations.push(request);
                continue;
            }
            match request.preparation {
                Preparation::Ready(Ok(state)) => {
                    let preset = DevicePreset::loaded(request.source);
                    self.insert_prepared_preset(
                        request.plugin,
                        request.role,
                        request.track,
                        state,
                        preset,
                    );
                }
                Preparation::Ready(Err(error)) => {
                    eprintln!(
                        "komponist: preparing a preset from {}: {error}",
                        request.plugin.name
                    );
                    self.set_status(self.lang.devices_preset_load_failed(), StatusKind::Error);
                }
                Preparation::Running(_) => unreachable!("running preparations were retained"),
            }
        }
    }

    fn insert_prepared_preset(
        &mut self,
        plugin: PluginRef,
        role: DeviceRole,
        track: Option<TrackId>,
        state: Vec<u8>,
        preset: DevicePreset,
    ) {
        if role == DeviceRole::Instrument {
            let spec = NewTrack {
                name: None,
                hue: None,
                instrument: Some(plugin),
                index: None,
                arm_exclusive: true,
            };
            let names = default_names(self.lang);
            if let Some(track) = self.apply_with("undo-load-preset", |p| {
                let (project, track) = edits::add_track_in(p, spec, &names)?;
                let device = project
                    .track(track)
                    .and_then(|track| track.channel.instrument())
                    .expect("new track has its instrument")
                    .1
                    .id;
                let project = edits::set_device_state(&project, device, Some(state.into()))?;
                let project = edits::set_device_preset(&project, device, Some(preset))?;
                Ok((project, track))
            }) {
                self.select_tracks(vec![track], SelectMode::Replace);
            }
        } else if let Some(track) = track.filter(|track| self.project().track(*track).is_some()) {
            let new = NewDevice {
                role,
                plugin,
                state: Some(state.into()),
                sidechain: None,
                preset: Some(preset),
            };
            if self
                .apply_with("undo-load-preset", |p| {
                    edits::insert_device(p, ChannelRef::Track(track), usize::MAX, new)
                })
                .is_some()
            {
                self.ui_action(UiAction::SetDockTab(DockTab::Devices));
            }
        }
    }
}

impl App {
    pub(super) fn open_user_preset_dialog(&mut self, device: DeviceId, save: bool) {
        if self.engine.host().supports_user_presets(device) && !self.user_presets.is_busy() {
            self.open_file_browser(if save {
                Purpose::SavePreset(device)
            } else {
                Purpose::OpenPreset(device)
            });
        }
    }

    pub(crate) fn save_user_preset_file(&mut self, device: DeviceId, path: PathBuf) {
        let Some((_, _, target)) = self.project().find_device(device) else {
            return;
        };
        let plugin_id = target.plugin.id.clone();
        if self.engine.host_mut().capture_clap_preset(device) {
            self.user_presets.capture = Some((device, plugin_id, path));
        } else {
            self.user_preset_save_failed("device is not ready to capture a preset");
        }
    }

    pub(crate) fn load_user_preset_file(&mut self, device: DeviceId, path: PathBuf) {
        self.user_presets.read = Some((device, file_job(move || read_user_preset(&path))));
    }

    pub(super) fn user_preset_captured(
        &mut self,
        device: DeviceId,
        result: &Result<Vec<u8>, String>,
    ) {
        if self
            .user_presets
            .capture
            .as_ref()
            .is_none_or(|(id, _, _)| *id != device)
        {
            return;
        }
        let (_, plugin_id, path) = self.user_presets.capture.take().expect("matched capture");
        match result {
            Ok(state) if state.len() <= MAX_PRESET_STATE => {
                let preset = UserPreset {
                    version: 1,
                    plugin_id,
                    state: state.clone(),
                };
                self.user_presets.write = Some(file_job(move || {
                    let encoded =
                        serde_json::to_string(&preset).map_err(|error| error.to_string())?;
                    match crate::project_io::write_atomic(&path, &encoded, false) {
                        Ok(None) => Ok(()),
                        Ok(Some(error)) => Err(format!(
                            "preset was written but could not be flushed: {error}"
                        )),
                        Err(error) => Err(error.to_string()),
                    }
                }));
            }
            Ok(_) => self.user_preset_save_failed("plugin preset state is too large"),
            Err(error) => self.user_preset_save_failed(error),
        }
    }

    fn user_preset_save_failed(&mut self, error: &str) {
        eprintln!("komponist: saving a preset: {error}");
        self.set_status(self.lang.devices_preset_save_failed(), StatusKind::Error);
    }

    pub(super) fn poll_user_presets(&mut self) {
        if self
            .user_presets
            .capture
            .as_ref()
            .is_some_and(|(device, _, _)| {
                self.project().find_device(*device).is_none()
                    || !self.engine.host().supports_user_presets(*device)
            })
        {
            self.user_presets.capture = None;
            self.user_preset_save_failed("plugin became unavailable");
        }
        if let Some((device, reader)) = &self.user_presets.read
            && let Some(result) = file_result(reader)
        {
            let device = *device;
            self.user_presets.read = None;
            let result = result.and_then(|preset| {
                let matches = self
                    .project()
                    .find_device(device)
                    .is_some_and(|(_, _, target)| target.plugin.id == preset.plugin_id);
                if matches
                    && self
                        .engine
                        .host_mut()
                        .load_user_preset(device, preset.state)
                {
                    Ok(())
                } else {
                    Err("preset does not match a ready plugin".into())
                }
            });
            if let Err(error) = result {
                eprintln!("komponist: loading a preset: {error}");
                self.set_status(self.lang.devices_preset_load_failed(), StatusKind::Error);
            }
        }
        if let Some(writer) = &self.user_presets.write
            && let Some(result) = file_result(writer)
        {
            self.user_presets.write = None;
            match result {
                Ok(()) => self.set_status(self.lang.devices_preset_saved(), StatusKind::Success),
                Err(error) => self.user_preset_save_failed(&error),
            }
        }
    }
}
