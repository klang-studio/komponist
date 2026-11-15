//! The mixer (WP-43): the dock's "Mixer" tab, normal height or maximised with Tab (S04;
//! `Screens.dc.html` `mixer()`, `mxDb`, `mxScale`, the `isMixer` markup; `ks-view.js`
//! `mixView`).
//!
//! One strip per track, 80 wide, left to right in track order on `surface.0` with 1 px gaps,
//! then (WP-103) the bus strips right before the master under a "Busse" label, and the master strip
//! (96) at the right edge. A strip ([`strip`]) shows the colour bar, name,
//! instrument, the insert slots with "+", the reserved sends, the pan knob, M S R, the fader with
//! the meter and the level and peak values. The strip of the selected track is
//! `canvas.strip-selected` with its name row in `accent.surface`; selection is the same as the
//! arrangement's. Strips that do not fit scroll sideways with the wheel; only visible strips are
//! drawn and get AccessKit nodes ("Kanal Lead").
//!
//! | Input | What happens (S04 notes) |
//! |---|---|
//! | click on a strip | selects the track (Strg toggles, Shift extends), synchronous with the arrangement |
//! | fader / pan | drag relative, Shift fine, double-click or Alt-click default, Enter or right-click „Wert eingeben“ text entry, wheel one step (Shift fine); a drag or a wheel burst is one undo step, fast-path commands while it lasts |
//! | M S R | as in the track headers: press, drag across strips, Alt-click S / R exclusive |
//! | insert slot | click opens the plugin window (an internal device: the Geräte tab, scrolled to it), drag reorders (2 px accent line), Alt-click bypass |
//! | "+" | opens the add menu below the slot ([`crate::views::devices::add_menu`]); on the master it lists audio effects only |
//! | browser plugin dropped on a strip | effect: inserted at the line; instrument: replaces the track's instrument |
//! | peak value / meter (LED) | click resets peak and clip, Alt-click all strips |
//! | level value | double-click text entry, Alt-click 0 dB, click clears the clip indicator |
//! | sends (WP-103, always; a project without bus offers „Neuer Effekt-Bus“ at the „+“) | one compact row per send ([`crate::views::routing::widgets::send_row`]): drag sets the level (one undo step, fast path while it lasts), click on the name changes the bus, right click pre / post / off / remove; the dashed "+" adds a send; a bus strip shows its inputs instead |
//! | output button (WP-103) | above the level: the dropdown of the master, groups and buses; targets that would close a loop are grey with the reason |
//! | routing chip (WP-103) | normal dock only: right end of the name row, opens the routing popover; Strg+click selects the track |

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
use komponist_ui::widgets::number_field::{WHEEL_GESTURE_IDLE, wheel_turned};
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind};

use self::metrics::{self as m, MASTER_WIDTH, STRIP_GAP, STRIP_WIDTH, Tier};
pub(crate) use self::strip::dashed_rect;
use self::strip::toggle_prop;
pub use self::strip::{Rows, SlotDrag};
use crate::actions::{Action, EditAction};
use crate::views::late::LateTracker;
use crate::views::{View, ViewCtx};

/// Sends the change of a continuous control (fader, knob, number field) as an undo step:
/// `BeginGesture { label }` when its drag starts, `action` on every change, `EndGesture` when the
/// drag stops. A change without a drag (double-click, text entry, keys) is one undo step of its own;
/// a burst of wheel notches is one undo step (one gesture).
/// Fader, pan, mute and solo inside a gesture reach the engine on the fast path (the dispatcher).
pub fn send_gesture(
    ctx: &mut ViewCtx<'_>,
    response: &Response,
    label: &str,
    action: impl FnOnce() -> Action,
) {
    let mut out = Vec::new();
    gesture_actions(
        response,
        response.changed(),
        || {
            Action::Edit(EditAction::BeginGesture {
                label: label.to_owned(),
            })
        },
        &mut out,
        |out| out.push(action()),
    );
    for action in out {
        ctx.send(action);
    }
}

/// The gesture bracket of [`send_gesture`] for views that collect their actions in a `Vec` (the
/// device cards): `begin` is pushed when the drag starts or the first wheel notch comes, `change`
/// runs when `changed` (and pushes the edits), `Edit(EndGesture)` follows when the drag stops or the
/// wheel burst ends. A change without drag or wheel is not bracketed: one undo step of its own.
pub fn gesture_actions(
    response: &Response,
    changed: bool,
    begin: impl Fn() -> Action,
    out: &mut Vec<Action>,
    change: impl FnOnce(&mut Vec<Action>),
) {
    let input = Touch {
        drag_started: response.drag_started(),
        dragged: response.dragged(),
        drag_stopped: response.drag_stopped(),
        wheel: !response.dragged() && wheel_turned(response),
        hovered: response.hovered(),
    };
    gesture_touch(
        &response.ctx,
        response.id,
        input,
        changed,
        begin,
        out,
        change,
    );
}

/// What the pointer did to one control in a frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Touch {
    /// A drag started.
    pub drag_started: bool,
    /// A drag is going on.
    pub dragged: bool,
    /// A drag ended.
    pub drag_stopped: bool,
    /// A wheel notch turned the control.
    pub wheel: bool,
    /// The pointer is over the control.
    pub hovered: bool,
}

/// [`gesture_actions`] for a control whose several parts have no response of their own to ask
/// (the device graph's handles): `id` keeps the wheel burst's state.
pub fn gesture_touch(
    ctx: &egui::Context,
    id: egui::Id,
    touch: Touch,
    changed: bool,
    begin: impl Fn() -> Action,
    out: &mut Vec<Action>,
    change: impl FnOnce(&mut Vec<Action>),
) {
    if touch.drag_started {
        out.push(begin());
    }
    // A burst of wheel notches is one gesture: it opens with the first notch and closes after
    // [`WHEEL_GESTURE_IDLE`] without one, or when the pointer leaves the control.
    let wheel = id.with("komponist_wheel_gesture");
    let turned = !touch.dragged && touch.wheel;
    let now = ctx.input(|i| i.time);
    let mut open: Option<f64> = ctx.data(|d| d.get_temp(wheel));
    if turned {
        if open.is_none() {
            out.push(begin());
        }
        open = Some(now);
    }
    if changed {
        change(out);
    }
    if touch.drag_stopped {
        out.push(Action::Edit(EditAction::EndGesture));
    }
    if let Some(last) = open {
        if !turned && (now - last >= WHEEL_GESTURE_IDLE || !touch.hovered) {
            out.push(Action::Edit(EditAction::EndGesture));
            open = None;
        } else if !turned {
            ctx.request_repaint_after_secs((WHEEL_GESTURE_IDLE - (now - last)).max(0.0) as f32);
        }
    }
    ctx.data_mut(|d| match open {
        Some(t) => {
            d.insert_temp(wheel, t);
        }
        None => {
            d.remove::<f64>(wheel);
        }
    });
}

/// A strip's name being edited (double-click on the name, "Umbenennen", F2).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Rename {
    track: TrackId,
    text: String,
    /// Focus and select all in the next frame.
    fresh: bool,
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
    /// The name being edited in place.
    rename: Option<Rename>,
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
                        label: "undo-toggle-tracks".to_owned(),
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
        let mut strips = model::strips(
            ctx.lang,
            &project,
            &ctx.ui.selection.tracks,
            ctx.engine.host(),
        );
        if self
            .rename
            .as_ref()
            .is_some_and(|r| project.track(r.track).is_none())
        {
            self.rename = None;
        }
        // F2 with the mixer in view: the selected track's name goes into the edit mode.
        if let Some(id) = ctx.ui.mixer.rename_request.take()
            && let Some(track) = project.track(id)
        {
            self.rename = Some(Rename {
                track: id,
                text: track.name.clone(),
                fresh: true,
            });
        }
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
        let Some((master, rest)) = strips.split_last() else {
            return;
        };
        // Tracks scroll; the buses stand right before the master and stay where they are (owner
        // decision 3 A). With buses a label band "Busse" lies over the buses and the strips below
        // it start lower; without them nothing changes.
        let bus_from = rest
            .iter()
            .position(model::Strip::is_bus)
            .unwrap_or(rest.len());
        let (tracks, buses) = rest.split_at(bus_from);
        let send_rows = model::send_rows(&strips);
        let band = if buses.is_empty() {
            0.0
        } else {
            m::BUS_LABEL_BAND
        };
        let strips_top = body.top() + band;
        let tier = if send_rows > 0 {
            Tier::for_routed_height(body.height() - band, send_rows)
        } else {
            Tier::for_height(body.height())
        };
        let master_rect =
            Rect::from_min_max(pos2(body.right() - MASTER_WIDTH, strips_top), body.max);
        let pitch = STRIP_WIDTH + STRIP_GAP;
        let buses_width = buses.len() as f32 * pitch;
        let buses_left = master_rect.left() - STRIP_GAP - buses_width + STRIP_GAP;
        let area_right = if buses.is_empty() {
            master_rect.left() - STRIP_GAP
        } else {
            buses_left - m::BUS_GAP
        };
        let area = Rect::from_min_max(
            pos2(body.left(), strips_top),
            pos2(area_right, body.bottom()),
        );
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

        let mut spans: Vec<(TrackId, Rangef)> = tracks
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let x = area.left() + i as f32 * pitch - scroll;
                Some((s.track()?, Rangef::new(x, x + STRIP_WIDTH)))
            })
            .collect();
        spans.extend(buses.iter().enumerate().filter_map(|(i, s)| {
            let x = buses_left + i as f32 * pitch;
            Some((s.track()?, Rangef::new(x, x + STRIP_WIDTH)))
        }));
        let mut events = Vec::new();
        let bus_clip = Rect::from_min_max(pos2(buses_left, strips_top), body.max);
        for (s, (track, span)) in rest.iter().zip(&spans) {
            let clip = if s.is_bus() { bus_clip } else { area };
            if span.max < clip.left() || span.min > clip.right() {
                continue;
            }
            let rect = Rect::from_x_y_ranges(*span, strips_top..=body.bottom());
            let rect = Rect::from_min_size(rect.min, vec2(STRIP_WIDTH, body.bottom() - strips_top));
            for (kind, event) in strip::show(ui, ctx, s, rect, clip, tier, &mut self.rename) {
                events.push((*track, kind, event));
            }
        }
        // The master has neither sends nor an output button.
        let master_tier = Tier {
            sends: false,
            send_rows: 0,
            output: false,
            ..tier
        };
        strip::show(
            ui,
            ctx,
            master,
            master_rect,
            body,
            master_tier,
            &mut self.rename,
        );
        if !buses.is_empty() {
            strip::bus_label(
                ui,
                ctx.lang,
                Rect::from_min_max(
                    pos2(buses_left, body.top()),
                    pos2(master_rect.left(), strips_top),
                ),
            );
        }
        for (track, kind, event) in events {
            self.toggle(ctx, rest, &spans, track, kind, event);
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
