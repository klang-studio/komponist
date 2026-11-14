//! The discrete controls of a half cell: `segment` (`W_seg`), `toggle_button` (`W_tbtn`) and
//! `dropdown` (`W_drop`). A label sits above the control (12 px), the control is 18 px high, so
//! label and control fill the 30 px half cell.

use egui::{
    Color32, EventFilter, Key, Painter, Rect, Response, Role, Sense, Ui, Widget, WidgetInfo, pos2,
    vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, widget};

use super::{LABEL, half_cell, paint_label};
use crate::state::State;
use crate::widgets::dropdown::{Dropdown, Item};
use crate::widgets::metrics::{self as wm, device as metrics};
use crate::widgets::{accesskit, is_still, sense};
use crate::{focus, icons, px};

/// The four wave shapes of an oscillator, drawn as 14 × 12 icons (`wave_icon`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wave {
    /// `M1 6 Q4 -1 7 6 T13 6`.
    Sine,
    /// `M1 10 L4 2 L10 10 L13 4`.
    Triangle,
    /// `M1 10 L7 2 V10 L13 2`.
    Saw,
    /// `M1 10 V2 H7 V10 H13 V2`.
    Square,
}

impl Wave {
    /// The polyline in the 14 × 12 icon box (the SVG paths of `wave_icon`; the sine's quadratic
    /// curves are sampled).
    pub fn points(self) -> Vec<(f32, f32)> {
        match self {
            Wave::Sine => {
                // Q4 -1 7 6 then T13 6 (reflected control point 10 13).
                let quad = |p0: (f32, f32), c: (f32, f32), p1: (f32, f32)| {
                    (0..=8).map(move |i| {
                        let t = i as f32 / 8.0;
                        let u = 1.0 - t;
                        (
                            u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0,
                            u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1,
                        )
                    })
                };
                let mut v: Vec<_> = quad((1.0, 6.0), (4.0, -1.0), (7.0, 6.0)).collect();
                v.extend(quad((7.0, 6.0), (10.0, 13.0), (13.0, 6.0)).skip(1));
                v
            }
            Wave::Triangle => vec![(1.0, 10.0), (4.0, 2.0), (10.0, 10.0), (13.0, 4.0)],
            Wave::Saw => vec![(1.0, 10.0), (7.0, 2.0), (7.0, 10.0), (13.0, 2.0)],
            Wave::Square => vec![
                (1.0, 10.0),
                (1.0, 2.0),
                (7.0, 2.0),
                (7.0, 10.0),
                (13.0, 10.0),
                (13.0, 2.0),
            ],
        }
    }
}

/// What a segment shows besides its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// An oscillator wave.
    Wave(Wave),
    /// An icon of the icon set (14 × 12 box).
    Icon(Icon),
}

/// One option of a [`Segment`]: its German name (the segment's text, or its accessible name when
/// it shows a glyph).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentOption<'a> {
    /// Text and accessible name.
    pub name: &'a str,
    /// Drawn instead of the text.
    pub glyph: Option<Glyph>,
}

impl<'a> SegmentOption<'a> {
    /// A text segment.
    pub const fn text(name: &'a str) -> Self {
        Self { name, glyph: None }
    }

    /// An icon segment named `name`.
    pub const fn glyph(name: &'a str, glyph: Glyph) -> Self {
        Self {
            name,
            glyph: Some(glyph),
        }
    }
}

/// Colours of one segment (`seg_on` `surface.5` / `text.1`, `seg_off` transparent / `text.2`;
/// hover one surface up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentLook {
    /// Ground, `None` = none.
    pub fill: Option<Color32>,
    /// Text and glyph.
    pub text: Color32,
    /// Keyboard focus ring.
    pub focus_ring: bool,
}

/// The [`SegmentLook`] of a segment, `on` or not, in `state`.
pub fn segment_look(on: bool, state: State) -> SegmentLook {
    let focus_ring = state.focused && !state.disabled;
    let (fill, text) = if state.disabled {
        (None, color::TEXT_4)
    } else if on {
        (Some(color::SURFACE_5), color::TEXT_1)
    } else if state.hovered || state.pressed {
        (Some(color::SURFACE_3), color::TEXT_1)
    } else {
        (None, color::TEXT_2)
    };
    SegmentLook {
        fill: fill.map(color32),
        text: color32(text),
        focus_ring,
    }
}

/// The track of a segment control: `surface.1`, `radius.m`, 1 px padding, 1 px between the
/// segments (`W_seg`: `padding:1px; gap:1px`; segments 16 high).
pub fn segment_rects(track: Rect, n: usize) -> Vec<Rect> {
    let pad = widget::segment::PADDING;
    let inner = track.shrink(pad);
    let gap = pad;
    let n = n.max(1);
    let w = (inner.width() - gap * (n as f32 - 1.0)) / n as f32;
    (0..n)
        .map(|i| {
            Rect::from_min_size(
                pos2(inner.left() + i as f32 * (w + gap), inner.top()),
                vec2(w, widget::segment::SEG_H),
            )
        })
        .collect()
}

fn paint_glyph(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32, ui: &Ui) {
    let [w, h] = widget::segment::ICON;
    let icon = Rect::from_center_size(rect.center(), vec2(w, h));
    let icon = px::snap_rect(icon, painter.pixels_per_point());
    match glyph {
        Glyph::Wave(wave) => {
            let pts: Vec<_> = wave
                .points()
                .into_iter()
                .map(|(x, y)| pos2(icon.left() + x, icon.top() + y))
                .collect();
            painter.add(egui::Shape::line(
                pts,
                egui::epaint::PathStroke::new(metrics::GLYPH_STROKE, color),
            ));
        }
        Glyph::Icon(i) => icons::paint_in(ui, icon, i, color),
    }
}

/// A segmented control in a half cell: `ui.add(Segment::new("Typ", &options, &mut sel).cols(2))`.
/// Click or wheel selects, ← → on a focused segment steps.
pub struct Segment<'a> {
    label: &'a str,
    options: &'a [SegmentOption<'a>],
    selected: &'a mut usize,
    cols: u8,
}

impl<'a> Segment<'a> {
    /// A control named `label` (shown above, and the group's accessible name; empty for none:
    /// the page switch) over `options`.
    pub fn new(label: &'a str, options: &'a [SegmentOption<'a>], selected: &'a mut usize) -> Self {
        Self {
            label,
            options,
            selected,
            cols: 1,
        }
    }

    /// Width in cells.
    #[must_use]
    pub fn cols(mut self, cols: u8) -> Self {
        self.cols = cols;
        self
    }
}

impl Widget for Segment<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let n = self.options.len();
        let (cell, mut outer) = ui.allocate_exact_size(half_cell(self.cols), Sense::hover());
        let has_label = !self.label.is_empty();
        let track = Rect::from_min_size(
            pos2(
                cell.left(),
                cell.top()
                    + if has_label {
                        widget::number::LABEL_H
                    } else {
                        0.0
                    },
            ),
            vec2(cell.width(), widget::segment::TRACK_H),
        );
        let painter = ui.painter().clone();
        paint_label(&painter, cell, self.label, !ui.is_enabled());
        px::fill(
            &painter,
            track,
            metrics::FIELD_RADIUS,
            color32(color::SURFACE_1),
        );
        let rects = segment_rects(track, n);
        let group = outer.id;
        let live = !is_still(ui) && ui.is_enabled();
        let mut choose = None;
        let mut hovered_any = false;
        for (i, (rect, option)) in rects.iter().zip(self.options).enumerate() {
            let id = group.with(("segment", i));
            let r = ui.interact(*rect, id, sense(ui, Sense::click()));
            hovered_any |= r.hovered();
            let on = i == *self.selected;
            let st = State::of(ui, &r);
            let name = if has_label {
                format!("{}: {}", self.label, option.name)
            } else {
                option.name.to_owned()
            };
            r.widget_info(|| WidgetInfo::selected(Role::RadioButton, !st.disabled, on, &name));
            accesskit(&r, |node| node.set_selected(on));
            if live {
                if r.clicked() {
                    r.request_focus();
                    choose = Some(i);
                }
                if r.has_focus() {
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(
                            id,
                            EventFilter {
                                horizontal_arrows: true,
                                ..Default::default()
                            },
                        );
                    });
                    let (left, right) = ui.input_mut(|inp| {
                        (
                            inp.consume_key(egui::Modifiers::NONE, Key::ArrowLeft),
                            inp.consume_key(egui::Modifiers::NONE, Key::ArrowRight),
                        )
                    });
                    if left && i > 0 {
                        choose = Some(i - 1);
                        ui.memory_mut(|m| m.request_focus(group.with(("segment", i - 1))));
                    }
                    if right && i + 1 < n {
                        choose = Some(i + 1);
                        ui.memory_mut(|m| m.request_focus(group.with(("segment", i + 1))));
                    }
                }
            }
            let look = segment_look(on, st);
            if let Some(fill) = look.fill {
                px::fill(&painter, *rect, metrics::SEGMENT_RADIUS, fill);
            }
            match option.glyph {
                Some(g) => paint_glyph(&painter, *rect, g, look.text, ui),
                None => {
                    let ppp = ui.pixels_per_point();
                    let g = super::elided_galley(&painter, option.name, &LABEL, None, rect.width());
                    painter.galley(
                        crate::widgets::text_pos_centered(*rect, &g, ppp),
                        g,
                        look.text,
                    );
                }
            }
            if look.focus_ring {
                focus::paint_ring(&painter, *rect, metrics::SEGMENT_RADIUS);
            }
        }
        if live && hovered_any {
            let steps = crate::widgets::wheel_steps(ui, group.with("wheel"));
            if steps != 0 {
                let next = (*self.selected as i64 - i64::from(steps.signum()))
                    .clamp(0, n as i64 - 1) as usize;
                choose = Some(next);
            }
        }
        // The control as a whole: a radio group named after the label.
        outer = ui.interact(track, group.with("group"), Sense::hover());
        outer.widget_info(|| WidgetInfo::labeled(Role::RadioGroup, true, self.label));
        if let Some(c) = choose
            && c != *self.selected
        {
            *self.selected = c;
            outer.mark_changed();
        }
        outer
    }
}

/// The look of a toggle button, `on` or not, in `state`: on is a `text.1` ground with `surface.0`
/// text (deliberately not the accent); off `surface.3` / `text.2`, hover one surface up.
pub fn toggle_look(on: bool, state: State) -> (Color32, Color32, bool) {
    let focus_ring = state.focused && !state.disabled;
    let (fill, text) = if state.disabled {
        (color::SURFACE_2, color::TEXT_4)
    } else if on {
        (color::TEXT_1, color::SURFACE_0)
    } else if state.hovered || state.pressed {
        (color::SURFACE_4, color::TEXT_2)
    } else {
        (color::SURFACE_3, color::TEXT_2)
    };
    (color32(fill), color32(text), focus_ring)
}

/// A toggle button in a half cell: `ui.add(ToggleButton::new("Loop", &mut on))`. `above` sets a
/// label above it; without, the 12 px stay free so the button sits at the bottom of the cell.
pub struct ToggleButton<'a> {
    label: &'a str,
    on: &'a mut bool,
    sub: &'a str,
    cols: u8,
}

impl<'a> ToggleButton<'a> {
    /// A button with the text and accessible name `label`.
    pub fn new(label: &'a str, on: &'a mut bool) -> Self {
        Self {
            label,
            on,
            sub: "",
            cols: 1,
        }
    }

    /// The label above the button.
    #[must_use]
    pub fn above(mut self, label: &'a str) -> Self {
        self.sub = label;
        self
    }

    /// Width in cells.
    #[must_use]
    pub fn cols(mut self, cols: u8) -> Self {
        self.cols = cols;
        self
    }
}

impl Widget for ToggleButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (cell, outer) = ui.allocate_exact_size(half_cell(self.cols), Sense::hover());
        let rect = Rect::from_min_size(
            pos2(cell.left(), cell.top() + widget::number::LABEL_H),
            vec2(cell.width(), widget::toggle_button::H),
        );
        let mut response = ui.interact(rect, outer.id.with("button"), sense(ui, Sense::click()));
        if !is_still(ui) && response.enabled() && response.clicked() {
            response.request_focus();
            *self.on = !*self.on;
            response.mark_changed();
        }
        let on = *self.on;
        let st = State::of(ui, &response);
        response.widget_info(|| WidgetInfo::selected(Role::Button, !st.disabled, on, self.label));
        accesskit(&response, |n| n.set_selected(on));
        let painter = ui.painter();
        paint_label(painter, cell, self.sub, st.disabled);
        let (fill, text, ring) = toggle_look(on, st);
        px::fill(painter, rect, metrics::FIELD_RADIUS, fill);
        let ppp = ui.pixels_per_point();
        let g = super::elided_galley(
            painter,
            self.label,
            &LABEL,
            Some(wm::SEMIBOLD),
            rect.width() - 2.0 * metrics::BUTTON_PADDING_X,
        );
        painter.galley(crate::widgets::text_pos_centered(rect, &g, ppp), g, text);
        if ring {
            focus::paint_ring(painter, rect, metrics::FIELD_RADIUS);
        }
        response
    }
}

/// A dropdown in a half cell: label above, the field 18 high, the list the design system's menu
/// ([`Dropdown`] in its device form). `ui.add(DropdownCell::new("Kit", &items, &mut sel).cols(2))`.
pub struct DropdownCell<'a> {
    label: &'a str,
    items: &'a [Item<'a>],
    selected: &'a mut usize,
    cols: u8,
    open_still: Option<Option<usize>>,
}

impl<'a> DropdownCell<'a> {
    /// A dropdown named `label` over `items` (default two cells wide, as the handoff).
    pub fn new(label: &'a str, items: &'a [Item<'a>], selected: &'a mut usize) -> Self {
        Self {
            label,
            items,
            selected,
            cols: 2,
            open_still: None,
        }
    }

    /// Width in cells.
    #[must_use]
    pub fn cols(mut self, cols: u8) -> Self {
        self.cols = cols;
        self
    }

    /// The list open below the field in the layout (gallery and snapshots).
    #[must_use]
    pub fn open_still(mut self, highlight: Option<usize>) -> Self {
        self.open_still = Some(highlight);
        self
    }
}

impl Widget for DropdownCell<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let width = super::cell_width(self.cols);
        let name = self.label;
        let (items, selected, open_still) = (self.items, self.selected, self.open_still);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let (label, _) =
                ui.allocate_exact_size(vec2(width, widget::number::LABEL_H), Sense::hover());
            paint_label(ui.painter(), label, name, !ui.is_enabled());
            let mut dd = Dropdown::new(name, selected, items).device().width(width);
            if let Some(h) = open_still {
                dd = dd.open_still(h);
            }
            ui.add(dd)
        })
        .inner
    }
}
