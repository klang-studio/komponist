//! The sampler's samples in the app (WP-87): loading, dropping, relinking, saving, collecting and
//! the pad selection that follows the step sequencer and the input.
//!
//! Linking, relinking and emptying a pad are undo steps (`crate::param_undo`).
//!
//! - **Link, don't copy** (owner, 2026-11-14, like Ableton): a sample dropped on a pad, chosen with
//!   "Sample laden …" or found with "Suchen …" becomes a link in the device state
//!   (`komponist_sampler::links`: absolute path and path relative to the project file);
//!   the device host loads it in the background.
//! - **Saving** rewrites the links that resolve for the file being written ([`rebase_project_links`]),
//!   so "Speichern unter" into another folder keeps relative paths right. The rewrite is outside the
//!   history and reaches every undo and redo step, each from its own state: undo after a save
//!   still restores that step's parameters and samples, with paths for the new folder (#86).
//! - **"Sammeln und speichern"** ([`ProjectAction::CollectAndSave`]) copies every sample that lies
//!   outside `<project name> Samples/` into it, next to the project file, and saves. Identical
//!   files are copied once. It is the only action that creates a folder. The files of audio clips
//!   (`komponist_model::AudioFile`, epic #8) are linked the same way and collected into the same
//!   folder by the same collector; saving rebases their links like the Sampler's.
//! - **Missing samples** are listed in the S06 dialog ([`crate::dialogs::missing_plugins`]) and
//!   found again with "Suchen …"; the other missing samples that lie in the chosen folder follow.
//! - **Pad selection**: the pad the device frame has selected (`{sel}`), the pad of the step
//!   sequencer's selected row and, with "Pad folgt Eingang", the pad the last note hit are one
//!   selection ([`App::sync_pads`], once a frame).
//!
//! Thread: UI thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Context;
use komponist_builtin::Internal;
use komponist_engine::Command;
use komponist_general_sampler as general_sampler;
use komponist_model::edits;
use komponist_model::{AudioFile, ChannelRef, ClipContent, DeviceId, Project, TrackId};
use komponist_plugins::bundled_sampler;
use komponist_sampler as sampler;
use komponist_sampler::{Collector, PadField, SampleLink, links};

use crate::actions::PluginAction;
use crate::app::App;
use crate::dialogs::file::Purpose;
use crate::dialogs::missing_plugins::missing_samples;
use crate::step_rows;
use crate::ui_state::{StatusKind, StepCursor};

/// The demo project with the synthesized test kit on the pads of every sampler (`--demo`): the
/// kit's files and showcase audio clips are synthesized into `dir` (created), the pads and clips
/// link them. The demo is a throwaway, so
/// `dir` is the system's temporary folder in the app.
pub fn demo_with_kit(
    mut project: komponist_model::Project,
    dir: &Path,
) -> std::io::Result<komponist_model::Project> {
    std::fs::create_dir_all(dir)?;
    let kit = sampler::testkit::write_kit(dir)?;
    let mut state = Internal::Sampler.encode_state(&Internal::Sampler.defaults());
    for (pad, path) in kit.iter().enumerate() {
        state = links::write(Some(&state), pad, Some(&SampleLink::new(path, None)));
    }
    let state = Some(Arc::<[u8]>::from(state));
    for track in &mut project.tracks {
        for device in &mut Arc::make_mut(track).channel.devices {
            if Internal::from_plugin(&device.plugin) == Some(Internal::Sampler) {
                Arc::make_mut(device).state = state.clone();
            }
        }
    }
    crate::showcase::audio::write_files(&mut project, dir)?;
    crate::showcase::audio::write_sampler_file(&mut project, dir)?;
    Ok(project)
}

/// How long a click on a pad holds its note (a gate pad needs a note off).
const AUDITION: Duration = Duration::from_millis(250);

/// Velocity of a pad click.
const AUDITION_VELOCITY: u8 = 100;

/// Transient state of the samples: the pad a file browser is open for, held audition notes, the
/// pad selection of the last frame.
#[derive(Default)]
pub(crate) struct State {
    /// "Sample laden …" / "Suchen …" waits for a file for this pad.
    pub(crate) target: Option<(DeviceId, usize)>,
    /// A "Sammeln und speichern" without a project file waits for the save.
    pub(crate) collect_after_save: bool,
    /// Held audition notes: when to release, track, key.
    auditions: Vec<(Instant, TrackId, u8)>,
    /// Waveform preview held by the primary pointer button.
    waveform_preview: Option<WaveformPreview>,
    /// Per sampler: the pad of the step row, the selected pad and the hit counter of the last
    /// frame.
    pads: std::collections::HashMap<DeviceId, PadSync>,
    /// Parameters with edits awaiting the document's next accepted snapshot. Values live in the host.
    pending_params: std::collections::HashMap<DeviceId, std::collections::BTreeSet<u32>>,
}

impl State {
    pub(crate) fn forget_device_state(&mut self) {
        self.pads.clear();
        self.pending_params.clear();
    }
}

enum WaveformPreview {
    Sample { track: TrackId, key: u8 },
    Slice(DeviceId),
}

/// A sampler device of the project: id, track (`None`: master), state.
type SamplerDevice = (DeviceId, Option<TrackId>, Internal, Option<Arc<[u8]>>);

fn sample_device_kind(device: &komponist_model::Device) -> Option<Internal> {
    Internal::from_plugin(&device.plugin)
        .or_else(|| bundled_sampler::kind(&device.plugin))
        .filter(|kind| matches!(kind, Internal::Sampler | Internal::GeneralSampler))
}

fn valid_slot(kind: Internal, pad: usize) -> bool {
    if kind == Internal::GeneralSampler {
        pad == 0
    } else {
        pad < sampler::PADS
    }
}

#[derive(Clone, Copy)]
struct PadSync {
    cursor: Option<usize>,
    sel: usize,
    hit: u32,
}

impl App {
    /// The folder of the project file.
    pub(crate) fn project_dir(&self) -> Option<PathBuf> {
        self.project_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    /// The sampler devices of the project: id, track (`None`: master), state.
    fn samplers(&self) -> Vec<SamplerDevice> {
        let p = self.project();
        let chains = p
            .tracks
            .iter()
            .map(|t| (Some(t.id), &t.channel.devices))
            .chain(std::iter::once((None, &p.master.devices)));
        chains
            .flat_map(|(track, devices)| {
                devices.iter().filter_map(move |d| {
                    Some((d.id, track, sample_device_kind(d)?, d.state.clone()))
                })
            })
            .collect()
    }

    fn sampler_state(&self, device: DeviceId) -> Option<Option<Arc<[u8]>>> {
        let (_, _, found) = self.project().find_device(device)?;
        sample_device_kind(found)?;
        Some(found.state.clone())
    }

    pub(crate) fn remember_sampler_param(&mut self, device: DeviceId, wire: u32) {
        if self
            .project()
            .find_device(device)
            .is_some_and(|(_, _, d)| bundled_sampler::kind(&d.plugin).is_some())
        {
            self.dialogs
                .samples
                .pending_params
                .entry(device)
                .or_default()
                .insert(wire);
        }
    }

    pub(crate) fn forget_sampler_params(&mut self, device: DeviceId) {
        self.dialogs.samples.pending_params.remove(&device);
    }

    /// A snapshot can retire the edit journal once it is in the document and the host
    /// certifies that it still represents the current parameter bases.
    pub(crate) fn reconcile_sampler_params(&mut self, device: DeviceId, state: &Arc<[u8]>) {
        if self.dialogs.samples.pending_params.contains_key(&device)
            && self.engine.host().is_current_param_snapshot(device, state)
        {
            self.forget_sampler_params(device);
        }
    }

    pub(crate) fn reset_sampler_hit(&mut self, device: DeviceId) {
        if let Some(sync) = self.dialogs.samples.pads.get_mut(&device) {
            sync.hit = 0;
        }
    }

    /// Commit knob edits independently so undoing an asset edit restores the previous link
    /// with the current settings, including controls still waiting for a plugin snapshot.
    pub(crate) fn flush_sampler_params(&mut self, device: DeviceId) {
        let Some(wires) = self.dialogs.samples.pending_params.get(&device) else {
            return;
        };
        let Some((_, _, d)) = self.project().find_device(device) else {
            return;
        };
        let Some(kind) = bundled_sampler::kind(&d.plugin) else {
            return;
        };
        let host = self.engine.host();
        let Some(ids) = host
            .native_layout(device)
            .and_then(|layout| layout.author_ids())
        else {
            return;
        };
        let params = kind.params().iter().filter_map(|info| {
            let id = *ids.get(info.id.as_str()).filter(|id| wires.contains(id))?;
            // The host orders plugin-side and application-side edits on one parameter base.
            Some((info, host.param_base(device, id)?))
        });
        if let Some(state) =
            bundled_sampler::edit_params(&d.plugin.version, d.state.as_deref(), params)
        {
            self.store_sampler_knob_state(device, &state.into());
            self.forget_sampler_params(device);
        }
    }

    /// The actions of the sampler's samples; `false` if `action` is not one of them.
    pub(crate) fn sampler_action(&mut self, action: &PluginAction) -> bool {
        match action {
            PluginAction::LoadSample { device, pad, path } => self.load_sample(*device, *pad, path),
            PluginAction::ClearSample { device, pad } => self.clear_sample(*device, *pad),
            PluginAction::PickSample { device, pad } => {
                self.dialogs.samples.target = Some((*device, *pad));
                self.open_file_browser(Purpose::Sample);
            }
            PluginAction::AuditionPad { device, pad } => self.audition(*device, *pad),
            PluginAction::PreviewSampler { device, slice_time } => {
                self.preview_sampler(*device, *slice_time);
            }
            PluginAction::SetSamplerSlices { device, settings } => {
                self.set_sampler_slices(*device, settings);
            }
            _ => return false,
        }
        true
    }

    /// Links `path` to the pad: the pad's start and end return to the whole sample, the link goes
    /// into the device state, the sample loads. One undo step ("Sample laden").
    fn load_sample(&mut self, device: DeviceId, pad: usize, path: &Path) {
        if self.engine.host().asset_slots(device).is_some() {
            self.set_plugin_asset(device, pad as u32, Some(path));
            return;
        }
        self.flush_sampler_params(device);
        self.in_one_step("undo-sample-load", |app| app.link_sample(device, pad, path));
    }

    fn link_sample(&mut self, device: DeviceId, pad: usize, path: &Path) {
        let Some((_, _, found)) = self.project().find_device(device) else {
            return;
        };
        if let Some(kind) = bundled_sampler::kind(&found.plugin) {
            let dir = self.project_dir();
            let state = found.state.as_deref();
            let relinking = state.is_some_and(|state| {
                bundled_sampler::missing_links(state, dir.as_deref())
                    .iter()
                    .any(|(slot, _)| *slot == pad)
            });
            let link = SampleLink::new(path, dir.as_deref());
            let edited = bundled_sampler::edit_link(
                kind,
                &found.plugin.version,
                state,
                pad,
                Some(&link),
                relinking,
            );
            if self.apply_bundled_sample_edit(device, edited, "undo-sample-load") {
                self.report_sample_loaded(kind, pad, &link);
            }
            return;
        }
        let Some(kind) = sample_device_kind(found).filter(|kind| valid_slot(*kind, pad)) else {
            return;
        };
        let state = found.state.clone();
        let dir = self.project_dir();
        let link = SampleLink::new(path, dir.as_deref());
        let values = kind
            .decode_state(state.as_deref())
            .unwrap_or_else(|_| kind.defaults());
        let relinking = kind == Internal::GeneralSampler
            && links::missing(state.as_deref(), dir.as_deref())
                .iter()
                .any(|(slot, _)| *slot == pad);
        if !relinking {
            for (wire, param) in kind.params().iter().enumerate() {
                let region_parameter = if kind == Internal::GeneralSampler {
                    matches!(param.id.as_str(), "start" | "end")
                } else {
                    [PadField::Start.wire(pad), PadField::End.wire(pad)].contains(&wire)
                };
                if region_parameter && values[wire] != param.default {
                    self.plugin_action(PluginAction::SetParam {
                        device,
                        id: wire as u32,
                        value: param.default,
                    });
                }
            }
        }
        let state = self.sampler_state(device).flatten();
        let reset_slices = kind == Internal::GeneralSampler && !relinking;
        let mut new = links::write(state.as_deref(), pad, Some(&link));
        if reset_slices {
            new =
                general_sampler::write_settings(Some(&new), &general_sampler::Settings::default());
        }
        self.store_param_state(device, &new.into(), "undo-sample-load");
        if let Some(handle) = self.engine.host().sampler(device) {
            if reset_slices {
                handle.set_general_settings(&general_sampler::Settings::default());
            }
            handle.load_link(pad, link.clone(), dir);
        }
        self.report_sample_loaded(kind, pad, &link);
    }

    fn apply_bundled_sample_edit(
        &mut self,
        device: DeviceId,
        state: Option<Vec<u8>>,
        label: &str,
    ) -> bool {
        let Some(state) = state else {
            return false;
        };
        let state: Arc<[u8]> = state.into();
        if !self.engine.host_mut().set_state(device, &state) {
            let name = self.device_name(device);
            self.set_status(self.lang.status_state_load_failed(&name), StatusKind::Error);
            return false;
        }
        self.store_param_state(device, &state, label);
        self.forget_sampler_params(device);
        true
    }

    fn report_sample_loaded(&mut self, kind: Internal, pad: usize, link: &SampleLink) {
        let name = if link.name.is_empty() {
            self.lang.status_sample_unnamed()
        } else {
            &link.name
        };
        let status = if kind == Internal::GeneralSampler {
            self.lang.status_sampler_loaded(name)
        } else {
            self.lang.status_sample_on_pad(name, pad + 1)
        };
        self.set_status(status, StatusKind::Info);
    }

    fn clear_sample(&mut self, device: DeviceId, pad: usize) {
        if self.engine.host().asset_slots(device).is_some() {
            self.set_plugin_asset(device, pad as u32, None);
            return;
        }
        self.flush_sampler_params(device);
        let Some((_, _, found)) = self.project().find_device(device) else {
            return;
        };
        let Some(kind) = sample_device_kind(found).filter(|kind| valid_slot(*kind, pad)) else {
            return;
        };
        if bundled_sampler::kind(&found.plugin).is_some() {
            let Some(state) = found.state.as_deref() else {
                return;
            };
            let edited = bundled_sampler::edit_link(
                kind,
                &found.plugin.version,
                Some(state),
                pad,
                None,
                false,
            );
            self.apply_bundled_sample_edit(device, edited, "undo-sample-clear");
            return;
        }
        let mut new = links::write(found.state.as_deref(), pad, None);
        if kind == Internal::GeneralSampler {
            new =
                general_sampler::write_settings(Some(&new), &general_sampler::Settings::default());
        }
        let new: Arc<[u8]> = new.into();
        self.store_param_state(device, &new, "undo-sample-clear");
        if let Some(handle) = self.engine.host().sampler(device) {
            if kind == Internal::GeneralSampler {
                handle.set_general_settings(&general_sampler::Settings::default());
            }
            handle.clear(pad);
        }
    }

    fn set_sampler_slices(&mut self, device: DeviceId, settings: &general_sampler::Settings) {
        self.flush_sampler_params(device);
        let Some((_, _, found)) = self.project().find_device(device) else {
            return;
        };
        if sample_device_kind(found) != Some(Internal::GeneralSampler) {
            return;
        }
        let state = found.state.as_deref();
        if bundled_sampler::kind(&found.plugin).is_some() {
            let edited = state.and_then(|state| bundled_sampler::edit_slices(state, settings));
            self.apply_bundled_sample_edit(device, edited, "undo-sample-slices");
            return;
        }
        let new = general_sampler::write_settings(state, settings);
        let updated = general_sampler::read_settings(Some(&new));
        if general_sampler::read_settings(state) == updated {
            return;
        }
        self.store_param_state(device, &new.into(), "undo-sample-slices");
        if let Some(handle) = self.engine.host().sampler(device) {
            handle.set_general_settings(&updated);
        }
    }

    /// A click on a pad: a live note on the device's track, released shortly after.
    fn audition(&mut self, device: DeviceId, pad: usize) {
        let Some((ChannelRef::Track(track), _, found)) = self.project().find_device(device) else {
            return;
        };
        if sample_device_kind(found) != Some(Internal::Sampler) || pad >= sampler::PADS {
            return;
        }
        let key = sampler::note_of_pad(pad);
        self.engine.send(Command::LiveNote {
            track,
            key,
            velocity: AUDITION_VELOCITY,
        });
        self.dialogs
            .samples
            .auditions
            .push((Instant::now() + AUDITION, track, key));
    }

    fn preview_sampler(&mut self, device: DeviceId, slice_time: Option<f64>) {
        let Some((ChannelRef::Track(track), _, found)) = self.project().find_device(device) else {
            return;
        };
        let kind = Internal::GeneralSampler;
        if sample_device_kind(found) != Some(kind) {
            return;
        }
        if let Some(time) = slice_time {
            let bundled = bundled_sampler::kind(&found.plugin).is_some();
            self.stop_waveform_preview();
            if bundled && self.engine.host().sampler_ui(device).is_some() {
                self.engine.host().preview_sampler_slice(device, Some(time));
                self.dialogs.samples.waveform_preview = Some(WaveformPreview::Slice(device));
            } else if let Some(handle) = self.engine.host().sampler(device) {
                handle.preview_slice(time);
                self.dialogs.samples.waveform_preview = Some(WaveformPreview::Slice(device));
            }
            return;
        }
        let key = if bundled_sampler::kind(&found.plugin).is_some() {
            let host = self.engine.host();
            host.native_layout(device)
                .and_then(|layout| layout.author_ids()?.get("root"))
                .and_then(|id| host.param_value(device, *id))
                .map_or(60, |value| value.value.round().clamp(0.0, 127.0) as u8)
        } else {
            let values = kind
                .decode_state(found.state.as_deref())
                .unwrap_or_else(|_| kind.defaults());
            kind.params()
                .iter()
                .zip(values)
                .find(|(param, _)| param.id.as_str() == "root")
                .map_or(60, |(_, value)| value.round() as u8)
        };
        self.stop_waveform_preview();
        self.dialogs.samples.waveform_preview = Some(WaveformPreview::Sample { track, key });
        self.engine.send(Command::LiveNote {
            track,
            key,
            velocity: AUDITION_VELOCITY,
        });
    }

    pub(crate) fn stop_waveform_preview(&mut self) {
        match self.dialogs.samples.waveform_preview.take() {
            Some(WaveformPreview::Sample { track, key }) => {
                self.engine.send(Command::LiveNote {
                    track,
                    key,
                    velocity: 0,
                });
            }
            Some(WaveformPreview::Slice(device)) => {
                if let Some(handle) = self.engine.host().sampler(device) {
                    handle.release_slice_preview();
                } else {
                    self.engine.host().preview_sampler_slice(device, None);
                }
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
        let held = &mut self.dialogs.samples.auditions;
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

    /// The file browser chose `path` for the pad it was opened for ("Sample laden …", "Suchen
    /// …"). A missing sample that is found pulls the other missing samples with it: those whose
    /// file name is in the chosen folder.
    pub(crate) fn sample_chosen(&mut self, path: PathBuf) {
        let Some((device, pad)) = self.dialogs.samples.target.take() else {
            return;
        };
        let dir = self.project_dir();
        let others = missing_samples(self.project(), dir.as_deref());
        let was_missing = others
            .iter()
            .any(|sample| (sample.device, sample.pad) == (device, pad));
        let mut links = vec![(device, pad, path.clone())];
        if let Some(folder) = path.parent().filter(|_| was_missing) {
            links.extend(others.into_iter().filter_map(|other| {
                if (other.device, other.pad) == (device, pad) || other.file.is_empty() {
                    return None;
                }
                let candidate = folder.join(other.file);
                candidate
                    .is_file()
                    .then_some((other.device, other.pad, candidate))
            }));
        }
        for &(device, _, _) in &links {
            self.flush_sampler_params(device);
        }
        // The chosen sample and the ones it finds with it are one undo step.
        self.in_one_step("undo-sample-load", |app| {
            for (device, pad, path) in links {
                app.link_sample(device, pad, &path);
            }
        });
    }

    /// "Sammeln und speichern": collects into the samples folder of the project file, then saves.
    pub(crate) fn collect_and_save(&mut self) {
        // A recovered copy whose file is gone has no file to collect next to (as Ctrl S).
        let file = self
            .project_path
            .clone()
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
        let folder = sampler::links::samples_folder_name(&file);
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
        if let Some(warning) = save_warning {
            text = format!("{warning} · {text}");
            kind = StatusKind::Error;
        }
        self.set_status(text, kind);
    }

    // --------------------------------------------------------------------------------------------
    // The pad selection

    /// Once a frame: the pad selected in the device frame (`{sel}`), the pad of the step
    /// sequencer's selected row and the pad the last note hit follow each other.
    ///
    /// A change of the step row selects its pad; a hit selects its pad while "Pad folgt Eingang"
    /// is on; a pad selected in the device frame (a click, or one of the two above) selects its
    /// row in the open step clip of the track.
    pub(crate) fn sync_pads(&mut self, ctx: &Context) {
        let open = self.ui.editor.clip;
        let cursor = self.ui.selection.step;
        for (device, track, kind, state) in self.samplers() {
            if kind != Internal::Sampler {
                continue;
            }
            let host = self.engine.host();
            let hit = host.sampler(device).map_or_else(
                || {
                    host.sampler_ui(device)
                        .map(|data| (data.last_hit.0, usize::from(data.last_hit.1)))
                },
                |handle| Some(handle.last_hit()),
            );
            let Some((hit_counter, hit_pad)) = hit else {
                continue;
            };
            let sel = crate::views::devices::selected_pad(ctx, device);
            // The pattern of the open step clip, if it is this sampler's track's.
            let pattern = open
                .zip(track)
                .and_then(|(clip, track)| {
                    let (t, c) = self.project().find_clip(clip)?;
                    (t.id == track).then(|| match &c.content {
                        ClipContent::Step(p) => Some((clip, p.clone())),
                        ClipContent::Midi(_) | ClipContent::Audio(_) => None,
                    })
                })
                .flatten();
            let cursor_pad = pattern.as_ref().and_then(|(clip, p)| {
                cursor
                    .filter(|c| c.clip == *clip)
                    .and_then(|c| step_rows::pad_of_row(p, c.row))
            });
            let last = self
                .dialogs
                .samples
                .pads
                .get(&device)
                .copied()
                .unwrap_or(PadSync {
                    cursor: cursor_pad,
                    sel,
                    hit: hit_counter,
                });
            let follow = host
                .native_layout(device)
                .and_then(|layout| layout.author_ids()?.get("follow"))
                .and_then(|id| host.param_value(device, *id))
                .map(|value| value.value >= 0.5)
                .unwrap_or_else(|| {
                    Internal::Sampler
                        .decode_state(state.as_deref())
                        .ok()
                        .and_then(|v| v.get(sampler::FOLLOW).copied())
                        .is_some_and(|v| v >= 0.5)
                });
            let mut now = sel;
            if cursor_pad != last.cursor
                && let Some(pad) = cursor_pad
            {
                now = pad;
            } else if follow && hit_counter != 0 && hit_counter != last.hit {
                now = hit_pad;
            }
            if now != sel {
                crate::views::devices::select_pad(ctx, device, now);
                ctx.request_repaint();
            }
            let mut cursor_pad = cursor_pad;
            if now != last.sel
                && cursor_pad != Some(now)
                && let Some((clip, p)) = &pattern
                && let Some(row) = step_rows::row_of_pad(p, now)
            {
                self.ui.selection.step = Some(StepCursor {
                    clip: *clip,
                    row,
                    step: None,
                });
                cursor_pad = Some(now);
            }
            self.dialogs.samples.pads.insert(
                device,
                PadSync {
                    cursor: cursor_pad,
                    sel: now,
                    hit: hit_counter,
                },
            );
        }
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
pub(crate) fn collect_sample_sources<'a>(
    project: &Project,
    collector: &mut Collector,
    project_dir: Option<&Path>,
    plugin_snapshot: impl Fn(DeviceId) -> Option<&'a [u8]>,
) -> std::io::Result<()> {
    for device in project
        .tracks
        .iter()
        .flat_map(|track| track.channel.devices.iter())
        .chain(project.master.devices.iter())
    {
        for link in device.asset_links.values() {
            if let Some(found) = link.resolve(project_dir) {
                collector.collect_asset_file(&found)?;
            }
        }
        if bundled_sampler::kind(&device.plugin).is_some() {
            let state = plugin_snapshot(device.id).or(device.state.as_deref());
            bundled_sampler::collect_links(collector, state, project_dir)?;
        } else if sample_device_kind(device).is_some() {
            collector.collect(device.state.as_deref(), project_dir)?;
        }
    }
    for file in audio_files(project) {
        if let Some(found) = file.resolve(project_dir) {
            collector.collect_file(&found)?;
        }
    }
    Ok(())
}

/// Rewrites a project or history entry for the destination folder while it still resolves
/// through the current project directory. Recording copies already have their final paths.
pub(crate) fn rebase_project_links(
    project: &Project,
    old_dir: Option<&Path>,
    new_dir: &Path,
    destination: impl Fn(PathBuf) -> PathBuf,
) -> Option<Project> {
    let after_samples = rewrite_links(
        project,
        |device| {
            if bundled_sampler::kind(&device.plugin).is_some() {
                bundled_sampler::rebase_links(
                    device.state.as_deref()?,
                    old_dir,
                    new_dir,
                    &destination,
                )
            } else {
                sample_device_kind(device)?;
                links::rebase_with(device.state.as_deref(), old_dir, new_dir, &destination)
            }
        },
        |file| {
            let path = destination(file.resolve(old_dir)?);
            Some(file.moved_to(&path, new_dir))
        },
    );
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
                    |source| link.moved_to(&destination(source), Some(new_dir)),
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

/// `project` with the Sampler links rewritten by `samplers` and the audio clip files by `audio`
/// (`None`: unchanged); `None` if nothing changed.
fn rewrite_links(
    project: &Project,
    samplers: impl FnMut(&komponist_model::Device) -> Option<Vec<u8>>,
    audio: impl FnMut(&AudioFile) -> Option<AudioFile>,
) -> Option<Project> {
    let after_samplers = rewrite_samplers(project, samplers);
    let base = after_samplers.as_ref().unwrap_or(project);
    match edits::rewrite_audio_files(base, audio) {
        Some(next) => Some(next),
        None => after_samplers,
    }
}

/// `project` with `rewrite` applied to the state of every sampler (tracks and master); `None` if
/// no state changed. `rewrite` returns the new state, `None` for "as it is".
fn rewrite_samplers(
    project: &Project,
    mut rewrite: impl FnMut(&komponist_model::Device) -> Option<Vec<u8>>,
) -> Option<Project> {
    let changes: Vec<(DeviceId, Arc<[u8]>)> = project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .chain(project.master.devices.iter())
        .filter_map(|d| rewrite(d).map(|s| (d.id, Arc::from(s))))
        .collect();
    if changes.is_empty() {
        return None;
    }
    let mut next = project.clone();
    for (device, state) in changes {
        next = edits::set_device_state(&next, device, Some(state)).ok()?;
    }
    Some(next)
}
