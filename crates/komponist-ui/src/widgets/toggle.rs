//! Track toggles M S R I (`tgl`, `toggleRows` of the design system): 18 × 16, `radius.s`, letter
//! in Mono 10 semibold.
//!
//! | | off | hover | on |
//! |---|---|---|---|
//! | M Mute | surface.4, text.2 | surface.5, text.1 | state.mute (text.2), surface.0 |
//! | S Solo | | | state.solo, surface.0 |
//! | R Record-Arm | | | state.record, text.1 |
//! | I Monitoring | | | state.play, surface.0 |
//!
//! R and I show a symbol instead of the letter (owner decision 2026-11-14, like Reaper): R a
//! filled record circle, I the Lucide `volume-2` speaker; M and S keep their letters. In a row
//! the toggles R and I stand [`metrics::TOGGLE_GROUP_GAP`] apart from M and S. Monitoring is
//! green (the design system has it accent blue).
//!
//! "Stumm durch Solo" (M implied by a solo elsewhere): surface.2 with a 1 px text.3 contour,
//! letter text.2. It is no click target of its own; M stays clickable. Disabled: surface.2 and
//! text.4 when off; an "on" toggle keeps its colours, so the value stays readable.
//!
//! Interaction ([`ToggleEvent`]): the toggle acts on **pointer press**, so a press can turn
//! into a drag across the other tracks ("Ziehen über Spuren überträgt den Zustand"). The widget
//! reports the press and the sweep; the caller (the arrangement) applies the value to the tracks
//! the pointer crosses. Alt-press on Solo or Record-Arm asks for "only this track". The caller
//! owns the track state; the widget only shows `on` and `implied`.
//!
//! Track toggles take no keyboard focus: M · S · R act on the selected tracks when the
//! arrangement is focused (app keymap). AccessKit can click them.

use egui::{Color32, Id, PointerButton, Pos2, Response, Role, Sense, Ui, Vec2, WidgetInfo, pos2};
use komponist_tokens::Icon;
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Srgba, color, radius, size, space};

use super::metrics;
use super::tooltip::Tip;
use crate::state::{self, State};
use crate::{focus, px};

/// Which flag a track toggle switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToggleKind {
    /// M: mute.
    Mute,
    /// S: solo.
    Solo,
    /// R: record arm.
    Arm,
    /// I: input monitoring.
    Monitor,
}

impl ToggleKind {
    /// M S R I in the order of the track header.
    pub const ALL: [Self; 4] = [Self::Mute, Self::Solo, Self::Arm, Self::Monitor];

    /// The letter on the toggle.
    pub const fn letter(self) -> &'static str {
        match self {
            Self::Mute => "M",
            Self::Solo => "S",
            Self::Arm => "R",
            Self::Monitor => "I",
        }
    }

    /// `true` for the toggles drawn with a symbol instead of a letter (R and I).
    pub const fn has_symbol(self) -> bool {
        matches!(self, Self::Arm | Self::Monitor)
    }

    /// The x offset of toggle `index` of [`Self::ALL`] in a row with `gap` between toggles and
    /// the extra group gap before R: `index × (width + gap)`, plus the group gap from R on.
    pub fn row_offset(index: usize, width: f32, gap: f32) -> f32 {
        let extra = if index >= 2 {
            metrics::TOGGLE_GROUP_GAP
        } else {
            0.0
        };
        index as f32 * (width + gap) + extra
    }

    /// The German name ("Mute", "Solo", "Record-Arm", "Monitoring"), as in the design system.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mute => "Mute",
            Self::Solo => "Solo",
            Self::Arm => "Record-Arm",
            Self::Monitor => "Monitoring",
        }
    }

    /// Surface and letter colour when on.
    pub const fn on_colors(self) -> (Srgba, Srgba) {
        match self {
            Self::Mute => (color::STATE_MUTE, color::SURFACE_0),
            Self::Solo => (color::STATE_SOLO, color::SURFACE_0),
            Self::Arm => (color::STATE_RECORD, color::TEXT_1),
            Self::Monitor => (color::STATE_PLAY, color::SURFACE_0),
        }
    }

    /// `true` if Alt-click makes it exclusive ("Solo bzw. Arm exklusiv: nur diese Spur").
    pub const fn alt_exclusive(self) -> bool {
        matches!(self, Self::Solo | Self::Arm)
    }

    /// The tooltip. `implied` adds "stumm durch Solo" to Mute.
    pub fn tip(self, implied: bool) -> Tip<'static> {
        match self {
            Self::Mute if implied => Tip::new("Mute").shortcut("M").detail("stumm durch Solo"),
            Self::Mute => Tip::new("Mute").shortcut("M"),
            Self::Solo => Tip::new("Solo")
                .shortcut("S")
                .detail("Alt-Klick: nur diese Spur solo"),
            Self::Arm => Tip::new("Record-Arm")
                .shortcut("R")
                .detail("Alt-Klick exklusiv"),
            Self::Monitor => Tip::new("Monitoring"),
        }
    }
}

/// Colours of a track toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToggleLook {
    /// Surface.
    pub fill: Color32,
    /// Letter.
    pub text: Color32,
    /// The 1 px inner contour of "stumm durch Solo", if shown.
    pub contour: Option<Color32>,
    /// Draw the focus ring.
    pub focus_ring: bool,
}

/// The [`ToggleLook`] of a `kind` toggle that is `on` (or `implied`: muted by a solo elsewhere)
/// in `state`.
pub fn look(kind: ToggleKind, on: bool, implied: bool, state: State) -> ToggleLook {
    let focus_ring = state.focused && !state.disabled;
    // Disabled: "Werte bleiben lesbar", so an "on" toggle keeps its colours (without hover).
    if state.disabled && !on {
        return ToggleLook {
            fill: color32(color::SURFACE_2),
            text: color32(color::TEXT_4),
            contour: implied.then(|| color32(color::TEXT_4)),
            focus_ring,
        };
    }
    let (fill, text, contour) = if on {
        let (f, t) = kind.on_colors();
        (color32(f), color32(t), None)
    } else if implied {
        (
            color32(color::SURFACE_2),
            state::text_for(color::TEXT_2, state),
            Some(color32(color::TEXT_3)),
        )
    } else {
        (
            state::surface_for(4, state),
            state::text_for(color::TEXT_2, state),
            None,
        )
    };
    ToggleLook {
        fill,
        text,
        contour,
        focus_ring,
    }
}

/// Paints the symbol of an R or I toggle centred in `rect` in `ink`: the record circle or the
/// speaker. Returns `false` (and paints nothing) for M and S.
pub fn paint_symbol(
    painter: &egui::Painter,
    rect: egui::Rect,
    kind: ToggleKind,
    ink: Color32,
) -> bool {
    let ppp = painter.pixels_per_point();
    match kind {
        ToggleKind::Arm => {
            let d = metrics::TOGGLE_RECORD_DOT;
            let c = pos2(
                px::snap(rect.center().x - d / 2.0, ppp) + d / 2.0,
                px::snap(rect.center().y - d / 2.0, ppp) + d / 2.0,
            );
            crate::icons::paint_record(
                painter,
                egui::Rect::from_center_size(c, Vec2::splat(d)),
                ink,
            );
            true
        }
        ToggleKind::Monitor => {
            let s = metrics::TOGGLE_MONITOR_ICON;
            let r = egui::Rect::from_min_size(
                pos2(
                    px::snap(rect.center().x - s / 2.0, ppp),
                    px::snap(rect.center().y - s / 2.0, ppp),
                ),
                Vec2::splat(s),
            );
            crate::timeline::paint_icon(painter, r, Icon::Volume2, ink);
            true
        }
        ToggleKind::Mute | ToggleKind::Solo => false,
    }
}

/// What a track toggle asks the caller to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToggleEvent {
    /// Set the flag of this track to `value` (pointer press, AccessKit click). With
    /// `exclusive` (Alt-press on Solo or Record-Arm), set it on this track only and clear it on
    /// all others; `value` is then `true`.
    Set {
        /// The new value for this track.
        value: bool,
        /// Only this track.
        exclusive: bool,
    },
    /// The press on this toggle is being dragged: the pointer moved from `from` to `to` in this
    /// pass. Set the flag to `value` (the one the press set) on every track whose toggle row the
    /// segment crosses, including tracks skipped by a fast move.
    Sweep {
        /// The value to apply.
        value: bool,
        /// Pointer position at the previous pass of the gesture (the press position at first).
        from: Pos2,
        /// Pointer position now.
        to: Pos2,
    },
}

/// A track toggle with its event.
#[derive(Debug)]
pub struct ToggleResponse {
    /// The widget's response.
    pub response: Response,
    /// What the user did in this pass, if anything.
    pub event: Option<ToggleEvent>,
}

/// One drag gesture: the value it spreads and the last pointer position.
#[derive(Clone, Copy, Debug)]
struct Gesture {
    value: Option<bool>,
    last: Pos2,
}

/// A track toggle (M, S, R or I).
#[must_use = "call `.show(ui)`"]
#[derive(Clone, Copy, Debug)]
pub struct TrackToggle<'a> {
    kind: ToggleKind,
    on: bool,
    implied: bool,
    track: Option<&'a str>,
    size: Vec2,
}

impl<'a> TrackToggle<'a> {
    /// A `kind` toggle showing `on`.
    pub fn new(kind: ToggleKind, on: bool) -> Self {
        Self {
            kind,
            on,
            implied: false,
            track: None,
            size: Vec2::from(size::TOGGLE),
        }
    }

    /// Shows Mute as implied by a solo elsewhere (only while off).
    pub fn implied(mut self, implied: bool) -> Self {
        self.implied = implied;
        self
    }

    /// The track's name, for the accessible name ("Mute · Lead").
    pub fn track(mut self, track: &'a str) -> Self {
        self.track = Some(track);
        self
    }

    /// Another size than `size.toggle` (18 × 16), e.g. in a mixer strip.
    pub fn size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    /// The accessible name: the kind's name, with the track's after " · ".
    pub fn accessible_name(&self) -> String {
        match self.track {
            Some(track) => format!("{} · {track}", self.kind.name()),
            None => self.kind.name().to_owned(),
        }
    }

    /// Adds the toggle; returns its response and what the user did.
    pub fn show(self, ui: &mut Ui) -> ToggleResponse {
        let still = super::is_still(ui);
        let (rect, response) =
            ui.allocate_exact_size(self.size, super::sense(ui, Sense::CLICK | Sense::DRAG));
        let st = State::of(ui, &response);
        let enabled = ui.is_enabled() && !st.disabled;
        let name = self.accessible_name();
        let on = self.on;
        response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, on, &name));

        let event = if still || !enabled {
            None
        } else {
            self.interact(ui, &response)
        };

        if ui.is_rect_visible(rect) {
            let l = look(self.kind, self.on, self.implied, st);
            let painter = ui.painter();
            px::fill(painter, rect, radius::S, l.fill);
            if let Some(contour) = l.contour {
                let ppp = ui.pixels_per_point();
                painter.rect_stroke(
                    px::snap_rect(rect, ppp),
                    crate::theme::corner_radius(radius::S),
                    egui::Stroke::new(px::line_width(ppp), contour),
                    egui::StrokeKind::Inside,
                );
            }
            if !paint_symbol(painter, rect, self.kind, l.text) {
                let galley = super::galley(
                    ui,
                    self.kind.letter(),
                    super::toggle_letter_font(),
                    typography::VALUE_S.line_height,
                );
                let pos = super::text_pos_centered(rect, &galley, ui.pixels_per_point());
                painter.galley(pos, galley, l.text);
            }
            if l.focus_ring {
                focus::paint_ring(painter, rect, radius::S);
            }
        }
        if !still {
            self.kind.tip(self.implied && !self.on).attach(&response);
        }
        ToggleResponse { response, event }
    }

    fn interact(&self, ui: &Ui, response: &Response) -> Option<ToggleEvent> {
        let gesture_id: Id = response.id.with("komponist_toggle_gesture");
        let (pressed, alt, pos) = ui.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Primary),
                i.modifiers.alt,
                i.pointer.interact_pos(),
            )
        });
        let exclusive = alt && self.kind.alt_exclusive();
        let set = ToggleEvent::Set {
            value: exclusive || !self.on,
            exclusive,
        };
        if pressed && response.is_pointer_button_down_on() {
            let g = Gesture {
                value: (!exclusive).then_some(exclusive || !self.on),
                last: pos.unwrap_or_else(|| response.rect.center()),
            };
            ui.data_mut(|d| d.insert_temp(gesture_id, g));
            return Some(set);
        }
        let gesture: Option<Gesture> = ui.data(|d| d.get_temp(gesture_id));
        if response.clicked() && gesture.is_none() {
            // A click without a press seen before: Enter / Space, AccessKit, or press and
            // release within one pass.
            return Some(if response.clicked_by(PointerButton::Primary) {
                set
            } else {
                ToggleEvent::Set {
                    value: !self.on,
                    exclusive: false,
                }
            });
        }
        if !response.is_pointer_button_down_on() && !response.dragged() {
            if gesture.is_some() {
                ui.data_mut(|d| d.remove::<Gesture>(gesture_id));
            }
            return None;
        }
        let (g, to) = (gesture?, pos?);
        let value = g.value?;
        if to == g.last {
            return None;
        }
        ui.data_mut(|d| d.insert_temp(gesture_id, Gesture { last: to, ..g }));
        Some(ToggleEvent::Sweep {
            value,
            from: g.last,
            to,
        })
    }
}

/// The "Toggle" card of the gallery: the M S R I table (aus, hover, an, sonderfall), the
/// switches, then a live row.
pub fn gallery(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    egui::Grid::new("komponist_gallery_toggles")
        .min_row_height(size::TOGGLE[1])
        .spacing([space::SPACE_4 + space::SPACE_1, space::SPACE_3])
        .show(ui, |ui| {
            ui.label("");
            for head in ["aus", "hover", "an", "sonderfall"] {
                super::caption_text(ui, head);
            }
            ui.end_row();
            for kind in ToggleKind::ALL {
                ui.label(
                    crate::fonts::rich_text(kind.name(), &typography::UI)
                        .color(color32(color::TEXT_2)),
                );
                for (on, st) in [
                    (false, State::REST),
                    (false, State::HOVERED),
                    (true, State::REST),
                ] {
                    state::force(ui, st, |ui| TrackToggle::new(kind, on).show(ui));
                }
                if kind == ToggleKind::Mute {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::SPACE_1_5;
                        state::force(ui, State::REST, |ui| {
                            TrackToggle::new(kind, false).implied(true).show(ui)
                        });
                        super::caption_text(ui, "stumm durch Solo");
                    });
                } else {
                    super::caption_text(ui, "—");
                }
                ui.end_row();
            }
        });
    super::switch::gallery_row(ui);
    live_row(ui);
}

fn live_row(ui: &mut Ui) {
    let id = Id::unique("komponist_gallery_toggle_live");
    let (mut flags, mut last): ([bool; 4], String) = ui
        .data(|d| d.get_temp(id))
        .unwrap_or(([false; 4], "—".to_owned()));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_0_5;
        for (i, kind) in ToggleKind::ALL.into_iter().enumerate() {
            if i == 2 {
                ui.add_space(metrics::TOGGLE_GROUP_GAP);
            }
            let r = TrackToggle::new(kind, flags[i]).track("Lead").show(ui);
            if let Some(ToggleEvent::Set { value, exclusive }) = r.event {
                flags[i] = value;
                let what = if exclusive {
                    "exklusiv"
                } else if value {
                    "an"
                } else {
                    "aus"
                };
                last = format!("{} {what}", kind.letter());
            }
        }
        ui.add_space(space::SPACE_3);
        super::caption_text(ui, &format!("live · Lead · {last}"));
    });
    ui.data_mut(|d| d.insert_temp(id, (flags, last)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ToggleKind::*;

    #[test]
    fn colours_follow_tgl() {
        assert_eq!(
            look(Mute, false, false, State::REST).fill,
            color32(color::SURFACE_4)
        );
        assert_eq!(
            look(Mute, false, false, State::REST).text,
            color32(color::TEXT_2)
        );
        assert_eq!(
            look(Mute, false, false, State::HOVERED).fill,
            color32(color::SURFACE_5)
        );
        assert_eq!(
            look(Mute, false, false, State::HOVERED).text,
            color32(color::TEXT_1)
        );
        assert_eq!(
            look(Mute, true, false, State::REST).fill,
            color32(color::STATE_MUTE)
        );
        assert_eq!(
            look(Solo, true, false, State::REST).fill,
            color32(color::STATE_SOLO)
        );
        assert_eq!(
            look(Solo, true, false, State::REST).text,
            color32(color::SURFACE_0)
        );
        assert_eq!(
            look(Arm, true, false, State::REST).text,
            color32(color::TEXT_1)
        );
        assert_eq!(
            look(Monitor, true, false, State::REST).fill,
            color32(color::STATE_PLAY)
        );
        assert_eq!(
            look(Monitor, true, false, State::REST).text,
            color32(color::SURFACE_0)
        );
        assert!(Arm.has_symbol() && Monitor.has_symbol());
        assert!(!Mute.has_symbol() && !Solo.has_symbol());
        // R and I stand one group gap further right than a plain row.
        assert_eq!(ToggleKind::row_offset(1, 18.0, 2.0), 20.0);
        assert_eq!(ToggleKind::row_offset(2, 18.0, 2.0), 44.0);
        let implied = look(Mute, false, true, State::REST);
        assert_eq!(implied.fill, color32(color::SURFACE_2));
        assert_eq!(implied.contour, Some(color32(color::TEXT_3)));
        assert_eq!(implied.text, color32(color::TEXT_2));
        // An explicit mute wins over the implied one.
        assert_eq!(look(Mute, true, true, State::REST).contour, None);
        // Disabled keeps the value readable.
        assert_eq!(
            look(Solo, true, false, State::DISABLED).fill,
            color32(color::STATE_SOLO)
        );
        assert_eq!(
            look(Solo, false, false, State::DISABLED).fill,
            color32(color::SURFACE_2)
        );
    }

    #[test]
    fn names() {
        assert_eq!(TrackToggle::new(Solo, false).accessible_name(), "Solo");
        assert_eq!(
            TrackToggle::new(Arm, false).track("Lead").accessible_name(),
            "Record-Arm · Lead"
        );
        assert!(Solo.alt_exclusive() && Arm.alt_exclusive());
        assert!(!Mute.alt_exclusive() && !Monitor.alt_exclusive());
        let letters: String = ToggleKind::ALL.iter().map(|k| k.letter()).collect();
        assert_eq!(letters, "MSRI");
    }

    #[test]
    fn letters_in_mono_semibold() {
        // `tgl`: Mono 10, `fontWeight: 600`.
        assert_eq!(
            crate::widgets::toggle_letter_font(),
            egui::FontId::new(
                typography::VALUE_S.size,
                egui::FontFamily::Name("IBMPlexMono-SemiBold".into())
            )
        );
    }
}
