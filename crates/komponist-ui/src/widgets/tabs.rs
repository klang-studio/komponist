//! Tabs and the segmented control ("Tabs und Umschalter": `Tab h 28 · Umschalter h 24, Segment
//! h 20`).
//!
//! **Tab** (`size.tab` 28 high, padding 0 14, `type.ui`), on a `surface.0` strip that the caller
//! paints; the active tab is part of the panel below:
//!
//! | | surface | text |
//! |---|---|---|
//! | active | surface.1 | text.1, medium |
//! | hover | surface.2 | text.2 |
//! | pressed | surface.3 | text.2 |
//! | inactive | – | text.3 |
//! | disabled | – | text.4 |
//!
//! **Segmented control** (`Umschalter`): a `surface.2` box, 24 high, `radius.m`, padding and gap
//! 2; segments 20 high, padding 0 10, `radius.s`, `type.small` size. Active: surface.4, text.1,
//! medium; hover: surface.3, text.1; inactive: text.2. The focus ring goes around the box.
//!
//! Keys: only the active tab or segment takes focus (Tab key); ← → then choose the neighbour
//! (skipping disabled ones) and keep the focus on it. Ctrl Tab (next tab in the focused area) is
//! an app shortcut; [`step`] computes the target.

use egui::{
    Color32, EventFilter, Frame, Id, InnerResponse, Key, Margin, Modifiers, Response, Role, Sense,
    Ui, UiBuilder, Widget, WidgetInfo, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, radius, size, space};

use super::metrics;
use crate::state::{self, State};
use crate::theme::corner_radius;
use crate::{focus, fonts, px};

/// One tab or segment.
pub type Item<'a> = super::dropdown::Item<'a>;

/// Colours and weight of a tab or segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabLook {
    /// Surface, `None` = none.
    pub fill: Option<Color32>,
    /// Text.
    pub text: Color32,
    /// Medium weight (active).
    pub medium: bool,
    /// Draw the focus ring.
    pub focus_ring: bool,
}

/// The [`TabLook`] of a tab, `active` or not, in `state`.
pub fn tab_look(active: bool, state: State) -> TabLook {
    let focus_ring = state.focused && !state.disabled;
    let (fill, text) = if state.disabled {
        (None, color::TEXT_4)
    } else if active {
        (Some(color::SURFACE_1), color::TEXT_1)
    } else if state.pressed {
        (Some(color::SURFACE_3), color::TEXT_2)
    } else if state.hovered {
        (Some(color::SURFACE_2), color::TEXT_2)
    } else {
        (None, color::TEXT_3)
    };
    TabLook {
        fill: fill.map(color32),
        text: color32(text),
        medium: active && !state.disabled,
        focus_ring,
    }
}

/// The [`TabLook`] of a segment, `active` or not, in `state` (the ring is the box's).
pub fn segment_look(active: bool, state: State) -> TabLook {
    let focus_ring = state.focused && !state.disabled;
    let (fill, text) = if state.disabled {
        (None, color::TEXT_4)
    } else if active || state.pressed {
        (Some(color::SURFACE_4), color::TEXT_1)
    } else if state.hovered {
        (Some(color::SURFACE_3), color::TEXT_1)
    } else {
        (None, color::TEXT_2)
    };
    TabLook {
        fill: fill.map(color32),
        text: color32(text),
        medium: active && !state.disabled,
        focus_ring,
    }
}

/// The index `dir` (+1 / −1) steps from `current` among the enabled `items`, wrapping around;
/// `current` if there is no other.
pub fn step(items: &[Item<'_>], current: usize, dir: i32) -> usize {
    let n = items.len();
    (1..n)
        .map(|k| {
            let k = k as i64 * i64::from(dir.signum());
            (current as i64 + k).rem_euclid(n as i64) as usize
        })
        .find(|&i| items[i].enabled)
        .unwrap_or(current)
}

/// Which of the two shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Tab,
    Segment,
}

impl Shape {
    fn height(self) -> f32 {
        match self {
            Self::Tab => size::TAB,
            Self::Segment => size::CONTROL_COMPACT,
        }
    }

    fn padding(self) -> f32 {
        match self {
            Self::Tab => metrics::TAB_PADDING_X,
            Self::Segment => metrics::BUTTON_PADDING_X,
        }
    }

    fn style(self) -> typography::TypeStyle {
        match self {
            Self::Tab => typography::UI,
            Self::Segment => typography::SMALL,
        }
    }

    fn radius(self) -> f32 {
        match self {
            Self::Tab => 0.0,
            Self::Segment => radius::S,
        }
    }

    fn role(self) -> Role {
        match self {
            Self::Tab => Role::Tab,
            Self::Segment => Role::RadioButton,
        }
    }

    fn look(self, active: bool, state: State) -> TabLook {
        match self {
            Self::Tab => tab_look(active, state),
            Self::Segment => segment_look(active, state),
        }
    }
}

fn item(
    ui: &mut Ui,
    id: Id,
    shape: Shape,
    label: &str,
    active: bool,
    enabled: bool,
) -> (Response, State) {
    let style = shape.style();
    let regular = super::galley(ui, label, fonts::font_id(&style), style.line_height);
    let medium = super::galley(
        ui,
        label,
        super::font_at(&style, metrics::MEDIUM),
        style.line_height,
    );
    // As wide as the medium text, so the width does not change with the state.
    let width = medium.size().x.ceil() + 2.0 * shape.padding();
    // Only the active entry is a Tab stop; arrows move between entries.
    let sense = if active && enabled {
        Sense::click()
    } else {
        Sense::CLICK
    };
    let (_, rect) = ui.allocate_space(vec2(width, shape.height()));
    let response = ui.interact(rect, id, super::sense(ui, sense));
    let mut st = State::of(ui, &response);
    if !enabled {
        st.disabled = true;
    }
    let is_enabled = enabled && ui.is_enabled() && !st.disabled;
    response.widget_info(|| WidgetInfo::selected(shape.role(), is_enabled, active, label));
    super::accesskit(&response, |n| n.set_selected(active));
    if ui.is_rect_visible(rect) {
        let l = shape.look(active, st);
        let painter = ui.painter();
        if let Some(fill) = l.fill {
            px::fill(painter, rect, shape.radius(), fill);
        }
        let galley = if l.medium { medium } else { regular };
        let pos = super::text_pos_centered(rect, &galley, ui.pixels_per_point());
        painter.galley(pos, galley, l.text);
        if shape == Shape::Tab && l.focus_ring {
            focus::paint_ring(painter, rect, 0.0);
        }
    }
    (response, st)
}

/// One tab (`Role::Tab`): for custom tab bars and the gallery. [`TabBar`] uses it.
pub fn tab(ui: &mut Ui, label: &str, active: bool, enabled: bool) -> Response {
    let id = ui.scope_id().with(("komponist_tab", label));
    item(ui, id, Shape::Tab, label, active, enabled).0
}

/// One segment (`Role::RadioButton`) inside [`segmented_frame`]. [`Segmented`] uses it.
pub fn segment(ui: &mut Ui, label: &str, active: bool, enabled: bool) -> (Response, State) {
    let id = ui.scope_id().with(("komponist_segment", label));
    item(ui, id, Shape::Segment, label, active, enabled)
}

/// The box of a segmented control: `surface.2`, padding and gap 2, `radius.m`, the focus ring
/// around it when `focus_ring`; `add` adds the segments. AccessKit: a radio group named `name`.
pub fn segmented_frame<R>(
    ui: &mut Ui,
    name: &str,
    focus_ring: impl FnOnce(&R) -> bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let builder = UiBuilder::new()
        .accessibility_role(Role::RadioGroup)
        .accessibility_label(name);
    let r = ui.scope_builder(builder, |ui| {
        Frame::new()
            .fill(color32(color::SURFACE_2))
            .corner_radius(corner_radius(radius::M))
            .inner_margin(Margin::same(space::SPACE_0_5 as i8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::SPACE_0_5;
                    add(ui)
                })
                .inner
            })
    });
    let frame = r.inner;
    if focus_ring(&frame.inner) {
        focus::paint_ring(ui.painter(), frame.response.rect, radius::M);
    }
    InnerResponse::new(frame.inner, frame.response)
}

/// Handles ← → on the focused entry `focused`; returns the new index, if any.
fn arrows(ui: &mut Ui, items: &[Item<'_>], focused: &Response, current: usize) -> Option<usize> {
    if !focused.has_focus() {
        return None;
    }
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(
            focused.id,
            EventFilter {
                horizontal_arrows: true,
                ..Default::default()
            },
        );
    });
    let (left, right) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowLeft),
            i.consume_key(Modifiers::NONE, Key::ArrowRight),
        )
    });
    let dir = i32::from(right) - i32::from(left);
    (dir != 0).then(|| step(items, current, dir))
}

/// The ids of the entries of a bar, so focus can follow the active one.
fn entry_id(bar: Id, i: usize) -> Id {
    bar.with(("komponist_tab", i))
}

fn show_entries(
    ui: &mut Ui,
    shape: Shape,
    bar: Id,
    items: &[Item<'_>],
    selected: usize,
) -> (Option<usize>, bool) {
    let mut chosen = None;
    let mut focus_to = None;
    let mut ring = false;
    for (i, it) in items.iter().enumerate() {
        let active = i == selected;
        let (r, st) = item(ui, entry_id(bar, i), shape, it.label, active, it.enabled);
        ring |= st.focused;
        if r.clicked() && it.enabled && !active {
            chosen = Some(i);
        }
        if active && let Some(next) = arrows(ui, items, &r, selected) {
            chosen = Some(next);
            focus_to = Some(next);
        }
    }
    // Focus follows the active entry, the only Tab stop. Requested after all entries exist:
    // egui drops the focus of a widget that is added without focus sense in the same pass.
    if let Some(next) = focus_to {
        ui.memory_mut(|m| m.request_focus(entry_id(bar, next)));
    }
    (chosen, ring)
}

/// A row of tabs choosing one of `items` by index; `name` is the tab list's accessible name.
/// The response (of the whole row) is `changed()` when the active tab changed.
#[must_use = "add it with `ui.add(…)`"]
pub struct TabBar<'a> {
    name: &'a str,
    selected: &'a mut usize,
    items: &'a [Item<'a>],
}

impl<'a> TabBar<'a> {
    /// Tabs over `items`, showing and setting `selected`.
    pub fn new(name: &'a str, selected: &'a mut usize, items: &'a [Item<'a>]) -> Self {
        Self {
            name,
            selected,
            items,
        }
    }
}

impl Widget for TabBar<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let bar = ui.scope_id().with(("komponist_tab_bar", self.name));
        let builder = UiBuilder::new()
            .id_salt(bar)
            .accessibility_role(Role::TabList)
            .accessibility_label(self.name);
        let r = ui.scope_builder(builder, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                show_entries(ui, Shape::Tab, bar, self.items, *self.selected)
            })
            .inner
        });
        let mut response = r.response;
        if let (Some(c), _) = r.inner {
            *self.selected = c;
            response.mark_changed();
        }
        response
    }
}

/// A segmented control (`Umschalter`) choosing one of `items` by index; `name` is its
/// accessible name. The response is `changed()` when the value changed.
#[must_use = "add it with `ui.add(…)`"]
pub struct Segmented<'a> {
    name: &'a str,
    selected: &'a mut usize,
    items: &'a [Item<'a>],
}

impl<'a> Segmented<'a> {
    /// Segments over `items`, showing and setting `selected`.
    pub fn new(name: &'a str, selected: &'a mut usize, items: &'a [Item<'a>]) -> Self {
        Self {
            name,
            selected,
            items,
        }
    }
}

impl Widget for Segmented<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let bar = ui.scope_id().with(("komponist_segmented", self.name));
        let (items, selected) = (self.items, self.selected);
        let r = segmented_frame(
            ui,
            self.name,
            |(_, ring): &(Option<usize>, bool)| *ring,
            |ui| show_entries(ui, Shape::Segment, bar, items, *selected),
        );
        let mut response = r.response;
        if let (Some(c), _) = r.inner {
            *selected = c;
            response.mark_changed();
        }
        response
    }
}

/// The "Tabs und Umschalter" card of the gallery.
pub fn gallery(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    // The tab strip (surface.0) over a panel (surface.1, 1 px surface.0 edge, 44 high), in a
    // box with radius.m, as in the design.
    let width = ui.available_width();
    let (rect, _) =
        ui.allocate_exact_size(vec2(width, size::TAB + size::TRANSPORT), Sense::hover());
    let edge = px::line_width(ui.pixels_per_point());
    px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_0));
    let panel = egui::Rect::from_min_max(
        egui::pos2(rect.left() + edge, rect.top() + size::TAB),
        egui::pos2(rect.right() - edge, rect.bottom() - edge),
    );
    px::fill(ui.painter(), panel, 0.0, color32(color::SURFACE_1));
    let strip = egui::Rect::from_min_size(rect.min, vec2(width, size::TAB));
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(strip)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row.spacing_mut().item_spacing.x = 0.0;
    let samples = [
        ("Piano Roll", true, true, State::REST),
        ("Step Sequencer", false, true, State::HOVERED),
        ("Geräte", false, true, State::REST),
        ("Automation", false, false, State::REST),
    ];
    for (label, active, enabled, st) in samples {
        state::force(&mut row, st, |ui| tab(ui, label, active, enabled));
    }

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4 + space::SPACE_1;
        for t in [
            "aktiv: fläche-1, text-1",
            "hover: fläche-2, text-2",
            "inaktiv: text-3",
            "deaktiviert: text-4",
        ] {
            super::caption_text(ui, t);
        }
    });

    super::sample_row(ui, space::SPACE_6, |ui| {
        super::sample(ui, "Umschalter", |ui| {
            segmented_frame(
                ui,
                "Ansicht",
                |_| false,
                |ui| {
                    state::force(ui, State::REST, |ui| {
                        segment(ui, "Arrangement", true, true);
                        segment(ui, "Mixer", false, true);
                    });
                },
            );
        });
        super::sample(ui, "fokussiert · Radierer hover", |ui| {
            segmented_frame(
                ui,
                "Werkzeug",
                |_| true,
                |ui| {
                    state::force(ui, State::REST, |ui| {
                        segment(ui, "Zeiger", false, true);
                        segment(ui, "Stift", true, true);
                    });
                    state::force(ui, State::HOVERED, |ui| {
                        segment(ui, "Radierer", false, true)
                    });
                },
            );
        });
    });

    let id = Id::unique("komponist_gallery_tabs_live");
    let (mut tab_i, mut tool): (usize, usize) = ui.data(|d| d.get_temp(id)).unwrap_or((0, 1));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        let tabs = [
            Item::new("Piano Roll"),
            Item::new("Step Sequencer"),
            Item::new("Geräte"),
            Item::new("Automation").disabled(),
        ];
        ui.add(TabBar::new("Editor", &mut tab_i, &tabs));
        let tools = [
            Item::new("Zeiger"),
            Item::new("Stift"),
            Item::new("Radierer"),
        ];
        ui.add(Segmented::new("Werkzeug", &mut tool, &tools));
        super::caption_text(ui, "live");
    });
    ui.data_mut(|d| d.insert_temp(id, (tab_i, tool)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_colours() {
        let a = tab_look(true, State::REST);
        assert_eq!(
            (a.fill, a.text, a.medium),
            (
                Some(color32(color::SURFACE_1)),
                color32(color::TEXT_1),
                true
            )
        );
        let h = tab_look(false, State::HOVERED);
        assert_eq!(
            (h.fill, h.text),
            (Some(color32(color::SURFACE_2)), color32(color::TEXT_2))
        );
        let i = tab_look(false, State::REST);
        assert_eq!((i.fill, i.text), (None, color32(color::TEXT_3)));
        let d = tab_look(false, State::DISABLED);
        assert_eq!(d.text, color32(color::TEXT_4));
    }

    #[test]
    fn segment_colours() {
        let a = segment_look(true, State::REST);
        assert_eq!(
            (a.fill, a.text, a.medium),
            (
                Some(color32(color::SURFACE_4)),
                color32(color::TEXT_1),
                true
            )
        );
        let h = segment_look(false, State::HOVERED);
        assert_eq!(
            (h.fill, h.text),
            (Some(color32(color::SURFACE_3)), color32(color::TEXT_1))
        );
        let i = segment_look(false, State::REST);
        assert_eq!((i.fill, i.text), (None, color32(color::TEXT_2)));
    }

    #[test]
    fn stepping_wraps_and_skips() {
        let items = [Item::new("a"), Item::new("b").disabled(), Item::new("c")];
        assert_eq!(step(&items, 0, 1), 2);
        assert_eq!(step(&items, 2, 1), 0);
        assert_eq!(step(&items, 0, -1), 2);
        assert_eq!(step(&[Item::new("a")], 0, 1), 0);
    }
}
