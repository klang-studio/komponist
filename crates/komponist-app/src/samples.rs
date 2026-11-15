//! The sampler's samples in the app (WP-87): loading, dropping, relinking, saving, collecting and
//! the pad selection that follows the step sequencer and the input.
//!
//! Linking, relinking and emptying a pad are undo steps (`crate::param_undo`).
//!
//! - **Link, don't copy** (owner, 2026-11-14, like Ableton): a sample dropped on a pad, chosen with
//!   "Sample laden …" or found with "Suchen …" becomes a link in the device state
//!   (`komponist_builtin::sampler::links`: absolute path and path relative to the project file);
//!   the device host loads it in the background.
//! - **Saving** rewrites the links that resolve for the file being written ([`App::rebase_samples`]),
//!   so "Speichern unter" into another folder keeps relative paths right.
//! - **"Sammeln und speichern"** ([`ProjectAction::CollectAndSave`]) copies every sample that lies
//!   outside `<project name> Samples/` into it, next to the project file, and saves. Identical
//!   files are copied once. It is the only action that creates a folder.
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
use komponist_builtin::sampler::{self, Collector, PadField, SampleLink, links};
use komponist_engine::Command;
use komponist_model::{ChannelRef, ClipContent, DeviceId, TrackId};

use crate::actions::PluginAction;
use crate::app::App;
use crate::dialogs::file::Purpose;
use crate::dialogs::missing_plugins::missing_samples;
use crate::step_rows;
use crate::ui_state::{StatusKind, StepCursor};

/// The demo project with the synthesized test kit on the pads of every sampler (`--demo`): the
/// kit's files are written into `dir` (created), the pads link them. The demo is a throwaway, so
/// `dir` is the system's temporary folder in the app.
pub fn demo_with_kit(
    mut project: komponist_model::Project,
    dir: &Path,
) -> std::io::Result<komponist_model::Project> {
    std::fs::create_dir_all(dir)?;
    let kit = sampler::testkit::write_kit(dir)?;
    let mut state: Option<Vec<u8>> = None;
    for (pad, path) in kit.iter().enumerate() {
        state = Some(links::write(
            state.as_deref(),
            pad,
            Some(&SampleLink::new(path, None)),
        ));
    }
    let state: Option<Arc<[u8]>> = state.map(Arc::from);
    for track in &mut project.tracks {
        for device in &mut Arc::make_mut(track).channel.devices {
            if Internal::from_plugin(&device.plugin) == Some(Internal::Sampler) {
                Arc::make_mut(device).state = state.clone();
            }
        }
    }
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
    /// Per sampler: the pad of the step row, the selected pad and the hit counter of the last
    /// frame.
    pads: std::collections::HashMap<DeviceId, PadSync>,
}

/// A sampler device of the project: id, track (`None`: master), state.
type SamplerDevice = (DeviceId, Option<TrackId>, Option<Arc<[u8]>>);

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
                devices
                    .iter()
                    .filter(|d| Internal::from_plugin(&d.plugin) == Some(Internal::Sampler))
                    .map(move |d| (d.id, track, d.state.clone()))
            })
            .collect()
    }

    fn sampler_state(&self, device: DeviceId) -> Option<Option<Arc<[u8]>>> {
        self.samplers()
            .into_iter()
            .find(|(id, ..)| *id == device)
            .map(|(.., state)| state)
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
            _ => return false,
        }
        true
    }

    /// Links `path` to the pad: the pad's start and end return to the whole sample, the link goes
    /// into the device state, the sample loads. One undo step ("Sample laden").
    fn load_sample(&mut self, device: DeviceId, pad: usize, path: &Path) {
        if pad >= sampler::PADS {
            return;
        }
        self.in_one_step("undo-sample-load", |app| app.link_sample(device, pad, path));
    }

    fn link_sample(&mut self, device: DeviceId, pad: usize, path: &Path) {
        let Some(state) = self.sampler_state(device) else {
            return;
        };
        let dir = self.project_dir();
        let link = SampleLink::new(path, dir.as_deref());
        let values = Internal::Sampler
            .decode_state(state.as_deref())
            .unwrap_or_else(|_| Internal::Sampler.defaults());
        for field in [PadField::Start, PadField::End] {
            let wire = field.wire(pad);
            let default = Internal::Sampler.params()[wire].default;
            if values
                .get(wire)
                .is_some_and(|v| (v - default).abs() > f64::EPSILON)
            {
                self.plugin_action(PluginAction::SetParam {
                    device,
                    id: wire as u32,
                    value: default,
                });
            }
        }
        let state = self.sampler_state(device).flatten();
        let new: Arc<[u8]> = links::write(state.as_deref(), pad, Some(&link)).into();
        self.store_param_state(device, &new, "undo-sample-load");
        if let Some(handle) = self.engine.host().sampler(device) {
            handle.load_link(pad, link.clone(), dir);
        }
        let name = if link.name.is_empty() {
            self.lang.status_sample_unnamed()
        } else {
            &link.name
        };
        self.set_status(
            self.lang.status_sample_on_pad(name, pad + 1),
            StatusKind::Info,
        );
    }

    fn clear_sample(&mut self, device: DeviceId, pad: usize) {
        let Some(state) = self.sampler_state(device) else {
            return;
        };
        let new: Arc<[u8]> = links::write(state.as_deref(), pad, None).into();
        self.store_param_state(device, &new, "undo-sample-clear");
        if let Some(handle) = self.engine.host().sampler(device) {
            handle.clear(pad);
        }
    }

    /// A click on a pad: a live note on the device's track, released shortly after.
    fn audition(&mut self, device: DeviceId, pad: usize) {
        let track = match self.project().find_device(device) {
            Some((ChannelRef::Track(t), ..)) => t,
            _ => return,
        };
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

    /// Releases the audition notes that are due; asks for a frame when one is still held.
    pub(crate) fn release_auditions(&mut self, ctx: &Context) {
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
        let was_missing = self
            .sampler_state(device)
            .flatten()
            .map(|s| links::missing(Some(&s), dir.as_deref()))
            .is_some_and(|m| m.iter().any(|(p, _)| *p == pad));
        let others = if was_missing {
            missing_samples(self.project(), dir.as_deref())
        } else {
            Vec::new()
        };
        // The chosen sample and the ones it finds with it are one undo step.
        self.in_one_step("undo-sample-load", |app| {
            app.load_sample(device, pad, &path);
            let Some(folder) = path.parent() else { return };
            for other in others {
                if (other.device, other.pad) == (device, pad) || other.file.is_empty() {
                    continue;
                }
                let candidate = folder.join(&other.file);
                if candidate.is_file() {
                    app.load_sample(other.device, other.pad, &candidate);
                }
            }
        });
    }

    /// Before saving to `file`: every link that resolves gets its absolute path refreshed and its
    /// relative path computed for `file`'s folder.
    pub(crate) fn rebase_samples(&mut self, file: &Path) {
        let Some(new_dir) = file.parent() else { return };
        let new_dir = if new_dir.as_os_str().is_empty() {
            sampler::links::absolute(Path::new(""))
        } else {
            sampler::links::absolute(new_dir)
        };
        let old_dir = self.project_dir();
        for (device, _, state) in self.samplers() {
            if let Some(new) = links::rebase(state.as_deref(), old_dir.as_deref(), &new_dir) {
                self.store_state_untracked(device, &Arc::from(new));
            }
        }
    }

    /// "Sammeln und speichern": collects into the samples folder of the project file, then saves.
    pub(crate) fn collect_and_save(&mut self) {
        let Some(file) = self.project_path.clone() else {
            // The folder of the samples depends on the file: ask for it, then collect.
            self.dialogs.samples.collect_after_save = true;
            self.open_file_browser(Purpose::Save);
            return;
        };
        match self.collect_samples(&file) {
            Ok(collector) => {
                if self.save_to(&file) {
                    let copied = collector.log.iter().filter(|c| c.copied).count();
                    let folder = sampler::links::samples_folder_name(&file);
                    let text = match copied {
                        0 => self.lang.status_collected_none(&folder),
                        n => self.lang.status_collected(n, &folder),
                    };
                    self.set_status(text, StatusKind::Success);
                }
            }
            Err(e) => {
                eprintln!("komponist: collecting samples failed: {e}");
                self.set_status(self.lang.status_collect_failed(e), StatusKind::Error);
            }
        }
    }

    /// Copies the samples of every sampler into the samples folder of `file` and points the links
    /// there; returns the collector (its log says what was copied).
    fn collect_samples(&mut self, file: &Path) -> std::io::Result<Collector> {
        let mut collector = Collector::new(file);
        let old_dir = self.project_dir();
        for (device, _, state) in self.samplers() {
            if let Some(new) = collector.collect(state.as_deref(), old_dir.as_deref())? {
                self.store_state_untracked(device, &Arc::from(new));
            }
        }
        Ok(collector)
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
        for (device, track, state) in self.samplers() {
            let Some(handle) = self.engine.host().sampler(device) else {
                continue;
            };
            let (hit_counter, hit_pad) = handle.last_hit();
            let sel = crate::views::devices::selected_pad(ctx, device);
            // The pattern of the open step clip, if it is this sampler's track's.
            let pattern = open
                .zip(track)
                .and_then(|(clip, track)| {
                    let (t, c) = self.project().find_clip(clip)?;
                    (t.id == track).then(|| match &c.content {
                        ClipContent::Step(p) => Some((clip, p.clone())),
                        ClipContent::Midi(_) => None,
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
            let follow = Internal::Sampler
                .decode_state(state.as_deref())
                .ok()
                .and_then(|v| v.get(sampler::FOLLOW).copied())
                .is_some_and(|v| v >= 0.5);
            let mut now = sel;
            if cursor_pad != last.cursor
                && let Some(pad) = cursor_pad
            {
                now = pad;
            } else if follow && hit_counter != last.hit {
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
