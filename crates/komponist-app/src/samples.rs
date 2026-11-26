//! The pad selection of devices that declare a pad bank, and the project folder helper.
//!
//! Linked files of every plugin go through the managed asset slots (`crate::plugin_assets`); the
//! app never reads or writes a device's own link format. Saving, collecting and rebasing linked
//! files happen in `crate::project_assets`; missing files are listed in the S06 dialog
//! ([`crate::dialogs::missing_plugins`]).
//!
//! - **Pad selection**: the pad the device frame has selected (`{sel}`), the pad of the step
//!   sequencer's selected row and, with "Pad folgt Eingang", the pad the last note hit are one
//!   selection ([`App::sync_pads`], once a frame).
//!
//! Thread: UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::Context;
use komponist_model::{ClipContent, DeviceId, DeviceRole, TrackId};

use crate::app::App;
use crate::ui_state::StepCursor;

/// Transient state of the pad selection.
#[derive(Default)]
pub(crate) struct State {
    /// A "Sammeln und speichern" without a project file waits for the save.
    pub(crate) collect_after_save: bool,
    /// Per pad bank: the pad of the step row, the selected pad and the hit counter of the last
    /// frame.
    pads: HashMap<DeviceId, PadSync>,
}

impl State {
    pub(crate) fn forget_device_state(&mut self) {
        self.pads.clear();
    }
}

#[derive(Clone, Copy)]
struct PadSync {
    cursor: Option<usize>,
    sel: usize,
    hit: Option<u32>,
}

struct PadInput {
    device: DeviceId,
    track: Option<TrackId>,
    notes: Vec<u8>,
    follow: bool,
    hit: Option<(u32, usize)>,
}

impl App {
    /// The folder of the project file.
    pub(crate) fn project_dir(&self) -> Option<PathBuf> {
        self.project_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    /// Forgets the last hit of `device` so the next one counts as new.
    pub(crate) fn reset_pad_hit(&mut self, device: DeviceId) {
        if let Some(sync) = self.dialogs.samples.pads.get_mut(&device) {
            sync.hit = None;
        }
    }

    /// Once a frame: the pad selected in the device frame (`{sel}`), the pad of the step
    /// sequencer's selected row and the pad the last note hit follow each other.
    ///
    /// A change of the step row selects its pad; a hit selects its pad while "Pad folgt Eingang"
    /// is on; a pad selected in the device frame (a click, or one of the two above) selects its
    /// row in the open step clip of the track.
    pub(crate) fn sync_pads(&mut self, ctx: &Context) {
        let open = self.ui.editor.clip;
        let cursor = self.ui.selection.step;
        for PadInput {
            device,
            track,
            notes,
            follow,
            hit,
        } in self.pad_inputs()
        {
            let hit_counter = hit.map(|(counter, _)| counter);
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
                let row = p.rows.get(cursor.filter(|c| c.clip == *clip)?.row)?;
                notes.iter().position(|&note| note == row.note)
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
            let mut now = sel;
            if cursor_pad != last.cursor
                && let Some(pad) = cursor_pad
            {
                now = pad;
            } else if follow
                && let Some((counter, pad)) = hit
                && counter != 0
                && last.hit.is_some_and(|previous| previous != counter)
            {
                now = pad;
            }
            if now != sel {
                crate::views::devices::select_pad(ctx, device, now);
                ctx.request_repaint();
            }
            let mut cursor_pad = cursor_pad;
            if now != last.sel
                && cursor_pad != Some(now)
                && let Some((clip, p)) = &pattern
                && let Some(&note) = notes.get(now)
                && let Some(row) = p.rows.iter().position(|row| row.note == note)
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

    fn pad_inputs(&self) -> Vec<PadInput> {
        let host = self.engine.host();
        let project = self.project();
        let instruments = project.tracks.iter().flat_map(|track| {
            track.channel.devices.iter().filter_map(|device| {
                (device.role == DeviceRole::Instrument).then_some((Some(track.id), device))
            })
        });
        instruments
            .filter_map(|(track, device)| {
                let bank = host.native_layout(device.id)?.pad_bank.as_ref()?;
                let follow = bank
                    .follow_param
                    .and_then(|id| host.param_value(device.id, id))
                    .is_some_and(|value| value.value >= 0.5);
                let hit = if follow {
                    bank.activity_tap
                        .as_deref()
                        .and_then(|key| host.signal_tap(device.id, key))
                        .and_then(|tap| tap.pad_activity(bank.pads.len()))
                        .map(|(_, frame)| (frame.hit, frame.last_pad))
                } else {
                    None
                };
                Some(PadInput {
                    device: device.id,
                    track,
                    follow,
                    hit,
                    notes: bank.pads.iter().map(|pad| pad.note).collect(),
                })
            })
            .collect()
    }
}
