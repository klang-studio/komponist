//! The mixer (WP-43): the dock's "Mixer" tab, normal height or maximised with Tab (S04;
//! `Screens.dc.html` `mixer()`, `mxDb`, `mxScale`, the `isMixer` markup; `ks-view.js`
//! `mixView`).
//!
//! One strip per track, 80 wide, left to right in track order on `surface.0` with 1 px gaps,
//! and the master strip (96) at the right edge. A strip ([`strip`]) shows the colour bar, name,
//! instrument, the insert slots with "+", the reserved sends, the pan knob, M S R, the fader with
//! the meter and the level and peak values. The strip of the selected track is
//! `canvas.strip-selected` with its name row in `accent.surface`; selection is the same as the
//! arrangement's. Strips that do not fit scroll sideways with the wheel; only visible strips are
//! drawn and get AccessKit nodes ("Kanal Lead").
//!
//! | Input | What happens (S04 notes) |
//! |---|---|
//! | click on a strip | selects the track (Strg toggles, Shift extends), synchronous with the arrangement |
//! | fader / pan | drag relative, Shift fine, Alt-click default, double-click text entry; a drag is one undo step, fast-path commands while it lasts |
//! | M S R | as in the track headers: press, drag across strips, Alt-click S / R exclusive |
//! | insert slot | click opens the plugin window (an internal device: the Geräte tab, scrolled to it), drag reorders (2 px accent line), Alt-click bypass |
//! | "+" | opens the browser on Effekte |
//! | browser plugin dropped on a strip | effect: inserted at the line; instrument: replaces the track's instrument |
//! | peak value / meter (LED) | click resets peak and clip, Alt-click all strips |
//! | level value | double-click text entry, Alt-click 0 dB, click clears the clip indicator |
//! | sends | reserved, empty (busses come later) |

pub mod metrics;
pub mod model;
mod strip;

use std::sync::Arc;

use egui::{Rangef, Rect, Response, Ui, pos2, vec2};
use komponist_model::TrackId;
use komponist_model::edits::ExclusiveFlag;
use komponist_tokens::color;
use komponist_tokens::egui_support::color32;
use komponist_ui::px;
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind};

use self::metrics::{MASTER_WIDTH, STRIP_GAP, STRIP_WIDTH, Tier};
use self::strip::toggle_prop;
pub use self::strip::{Rows, SlotDrag};
use crate::actions::{Action, EditAction};
use crate::views::late::LateTracker;
use crate::views::{View, ViewCtx};

/// Sends the change of a continuous control (fader, knob, number field) as an undo step:
/// `BeginGesture { label }` when its drag starts, `action` on every change, `EndGesture` when the
/// drag stops. A change without a drag (Alt-click, text entry, keys) is one undo step of its own.
/// Fader, pan, mute and solo inside a gesture reach the engine on the fast path (the dispatcher).
pub fn send_gesture(
    ctx: &mut ViewCtx<'_>,
    response: &Response,
    label: &str,
    action: impl FnOnce() -> Action,
) {
    if response.drag_started() {
        ctx.send(Action::Edit(EditAction::BeginGesture {
            label: label.to_owned(),
        }));
    }
    if response.changed() {
        ctx.send(action());
    }
    if response.drag_stopped() {
        ctx.send(Action::Edit(EditAction::EndGesture));
    }
}

/// The mixer view. Its view state is [`crate::ui_state::MixerView`] (scroll).
#[derive(Debug, Default)]
pub struct Mixer {
    /// A press on M S R opened a gesture (ended on release).
    toggle_gesture: bool,
    /// The "zu spät" state of the inserts.
    late: LateTracker,
    /// The selection of the last frame: a track selected elsewhere is scrolled into view.
    last_selection: std::collections::BTreeSet<TrackId>,
}

impl Mixer {
    /// The view.
    pub fn new() -> Mixer {
        Mixer::default()
    }

    /// Applies a toggle event of `track`'s strip; `spans` are the x ranges of all track strips.
    fn toggle(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        strips: &[model::Strip],
        spans: &[(TrackId, Rangef)],
        track: TrackId,
        kind: ToggleKind,
        event: ToggleEvent,
    ) {
        match event {
            ToggleEvent::Set {
                exclusive: true, ..
            } => {
                let flag = if kind == ToggleKind::Solo {
                    ExclusiveFlag::Solo
                } else {
                    ExclusiveFlag::Arm
                };
                ctx.send(Action::Edit(EditAction::SetExclusive { track, flag }));
            }
            ToggleEvent::Set { value, .. } => {
                if !self.toggle_gesture {
                    ctx.send(Action::Edit(EditAction::BeginGesture {
                        label: model::names::UNDO_TOGGLES.to_owned(),
                    }));
                    self.toggle_gesture = true;
                }
                ctx.send(Action::Edit(EditAction::SetTrackProp {
                    tracks: vec![track],
                    prop: toggle_prop(kind, value),
                }));
            }
            ToggleEvent::Sweep { value, from, to } => {
                // "Ziehen über Spuren": every strip the pointer crossed.
                let on = |s: &model::Strip| match kind {
                    ToggleKind::Mute => s.mute,
                    ToggleKind::Solo => s.solo,
                    ToggleKind::Arm => s.arm,
                    ToggleKind::Monitor => false,
                };
                let tracks: Vec<TrackId> = spans
                    .iter()
                    .filter(|(_, span)| strip::crosses(*span, from, to))
                    .filter(|(t, _)| {
                        strips
                            .iter()
                            .find(|s| s.track() == Some(*t))
                            .is_some_and(|s| on(s) != value)
                    })
                    .map(|(t, _)| *t)
                    .collect();
                if !tracks.is_empty() {
                    ctx.send(Action::Edit(EditAction::SetTrackProp {
                        tracks,
                        prop: toggle_prop(kind, value),
                    }));
                }
            }
        }
    }
}

impl View for Mixer {
    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        let project = Arc::clone(ctx.project);
        let mut strips = model::strips(&project, &ctx.ui.selection.tracks, ctx.engine.host());
        let now = ui.input(|i| i.time);
        for s in &mut strips {
            for insert in &mut s.inserts {
                let running = insert.state == model::SlotState::Ok;
                insert.late = self
                    .late
                    .state(ctx.engine.host(), insert.device, running, now);
            }
        }
        let live: Vec<_> = strips
            .iter()
            .flat_map(|s| s.inserts.iter().map(|i| i.device))
            .collect();
        self.late.retain(&live);
        let body = ui.max_rect();
        px::fill(ui.painter(), body, 0.0, color32(color::SURFACE_0));
        let tier = Tier::for_height(body.height());
        let Some((master, tracks)) = strips.split_last() else {
            return;
        };
        let master_rect =
            Rect::from_min_max(pos2(body.right() - MASTER_WIDTH, body.top()), body.max);
        let area = Rect::from_min_max(
            body.min,
            pos2(master_rect.left() - STRIP_GAP, body.bottom()),
        );
        let pitch = STRIP_WIDTH + STRIP_GAP;
        let content = tracks.len() as f32 * pitch - STRIP_GAP;
        let max_scroll = (content - area.width()).max(0.0);
        let mut scroll = ctx.ui.mixer.scroll_x.clamp(0.0, max_scroll);
        if self.last_selection != ctx.ui.selection.tracks {
            // A track selected in the arrangement, inspector or by keys comes into view; the
            // strip the user clicked here is visible already.
            let first = tracks.iter().position(|s| {
                s.track()
                    .is_some_and(|t| ctx.ui.selection.tracks.contains(&t))
            });
            if let Some(i) = first {
                scroll = model::scroll_to_strip(i, pitch, STRIP_WIDTH, area.width(), scroll)
                    .clamp(0.0, max_scroll);
            }
            self.last_selection.clone_from(&ctx.ui.selection.tracks);
        }
        ctx.ui.mixer.scroll_x = scroll;

        let spans: Vec<(TrackId, Rangef)> = tracks
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let x = area.left() + i as f32 * pitch - scroll;
                Some((s.track()?, Rangef::new(x, x + STRIP_WIDTH)))
            })
            .collect();
        let mut events = Vec::new();
        for (s, (track, span)) in tracks.iter().zip(&spans) {
            if span.max < area.left() || span.min > area.right() {
                continue;
            }
            let rect = Rect::from_x_y_ranges(*span, body.y_range());
            let rect = Rect::from_min_size(rect.min, vec2(STRIP_WIDTH, body.height()));
            for (kind, event) in strip::show(ui, ctx, s, rect, area, tier) {
                events.push((*track, kind, event));
            }
        }
        strip::show(ui, ctx, master, master_rect, body, tier);
        for (track, kind, event) in events {
            self.toggle(ctx, tracks, &spans, track, kind, event);
        }

        // The wheel scrolls the strips sideways.
        if max_scroll > 0.0 && ui.rect_contains_pointer(area) {
            let delta = ui.input(|i| i.smooth_scroll_delta);
            let d = delta.x + delta.y;
            if d != 0.0 {
                ctx.ui.mixer.scroll_x = (scroll - d).clamp(0.0, max_scroll);
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
        }
        if self.toggle_gesture && !ui.input(|i| i.pointer.primary_down()) {
            ctx.send(Action::Edit(EditAction::EndGesture));
            self.toggle_gesture = false;
        }
    }
}
