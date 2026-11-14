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
//! [`Segmented::equal_width`] stretches the box over the available width and gives every segment
//! the same width (the S01 browser: `grid-template-columns: repeat(4, 1fr)`).
//!
//! Keys: only the active tab or segment takes focus (Tab key); ← → then choose the neighbour
//! (skipping disabled ones) and keep the focus on it. Ctrl Tab (next tab in the focused area) is
//! an app shortcut; [`step`] computes the target.
//!
//! **Dock tabs** (WP-30, `tab()` / `editorTabs()` in `Screens.dc.html`): a tab of a [`TabBar`]
//! can carry [`TabExtra`]s, a colour mark before the label (8 × 8, `radius.s`, the track colour
//! of the open clip) and secondary text after it in `type.ui` `text.3` ("■ Lead A  Piano
//! Roll"), 8 px apart. With [`TabBar::accent_line`] the active tab shows the focused-panel
//! accent line (1 px `accent` at its top).

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

/// Dock tab metrics from `Screens.dc.html` `tab()`. They belong into [`super::metrics`], which
/// WP-30 may not change (report: move them there).
pub mod dock_metrics {
    use komponist_tokens::space;

    /// The colour mark: `dot` `width:8, height:8, borderRadius:2` (the radius is `radius.s`).
    pub const MARK: f32 = 8.0;
    /// Gap between mark, label and secondary text: `gap:8px`.
    pub const GAP: f32 = space::SPACE_2;
}

/// Extras of a tab in a [`TabBar`]: the dock's colour mark and secondary text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TabExtra<'a> {
    /// Colour mark before the label (the track colour of the open clip), `None` = none.
    pub mark: Option<Color32>,
    /// Secondary text after the label ("Piano Roll"), empty = none.
    pub secondary: &'a str,
}

impl<'a> TabExtra<'a> {
    /// A mark of `color` and the secondary text `secondary` (may be empty).
    pub const fn new(mark: Option<Color32>, secondary: &'a str) -> Self {
        Self { mark, secondary }
    }

    fn is_empty(&self) -> bool {
        self.mark.is_none() && self.secondary.is_empty()
    }

    /// The accessible name of a tab labelled `label` with these extras: "Lead A · Piano Roll"
    /// (the tab text of `Layout.dc.html`).
    pub fn accessible_name(&self, label: &str) -> String {
        if self.secondary.is_empty() {
            label.to_owned()
        } else {
            format!("{label} · {}", self.secondary)
        }
    }
}

/// Decoration of a tab: extras and the accent line of the active tab.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Deco<'a> {
    extra: TabExtra<'a>,
    accent_line: bool,
    /// A fixed width instead of the text's ([`Segmented::equal_width`]).
    width: Option<f32>,
}

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
    item_deco(ui, id, shape, label, active, enabled, Deco::default())
}

fn item_deco(
    ui: &mut Ui,
    id: Id,
    shape: Shape,
    label: &str,
    active: bool,
    enabled: bool,
    deco: Deco<'_>,
) -> (Response, State) {
    let style = shape.style();
    let regular = super::galley(ui, label, fonts::font_id(&style), style.line_height);
    let medium = super::galley(
        ui,
        label,
        super::font_at(&style, metrics::MEDIUM),
        style.line_height,
    );
    let extra = deco.extra;
    let mark_w = if extra.mark.is_some() {
        dock_metrics::MARK + dock_metrics::GAP
    } else {
        0.0
    };
    let secondary = (!extra.secondary.is_empty()).then(|| {
        super::galley(
            ui,
            extra.secondary,
            fonts::font_id(&style),
            style.line_height,
        )
    });
    let secondary_w = secondary
        .as_ref()
        .map_or(0.0, |g| dock_metrics::GAP + g.size().x.ceil());
    // As wide as the medium text, so the width does not change with the state.
    let width = deco
        .width
        .unwrap_or_else(|| mark_w + medium.size().x.ceil() + secondary_w + 2.0 * shape.padding());
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
    let name = extra.accessible_name(label);
    response.widget_info(|| WidgetInfo::selected(shape.role(), is_enabled, active, &name));
    super::accesskit(&response, |n| n.set_selected(active));
    if ui.is_rect_visible(rect) {
        let l = shape.look(active, st);
        let painter = ui.painter();
        if let Some(fill) = l.fill {
            px::fill(painter, rect, shape.radius(), fill);
        }
        let ppp = ui.pixels_per_point();
        let medium_w = medium.size().x.ceil();
        let galley = if l.medium { medium } else { regular };
        if extra.is_empty() {
            let pos = super::text_pos_centered(rect, &galley, ppp);
            painter.galley(pos, galley, l.text);
        } else {
            let mut x = rect.left() + shape.padding();
            if let Some(mark) = extra.mark {
                let r = egui::Rect::from_min_size(
                    egui::pos2(x, rect.center().y - dock_metrics::MARK / 2.0),
                    egui::Vec2::splat(dock_metrics::MARK),
                );
                px::fill(painter, r, radius::S, mark);
                x += dock_metrics::MARK + dock_metrics::GAP;
            }
            painter.galley(super::text_pos(rect, &galley, x, ppp), galley, l.text);
            if let Some(sec) = secondary {
                let x = x + medium_w + dock_metrics::GAP;
                let pos = super::text_pos(rect, &sec, x, ppp);
                let text = if l.text == color32(color::TEXT_4) {
                    l.text
                } else {
                    color32(color::TEXT_3)
                };
                painter.galley(pos, sec, text);
            }
        }
        if shape == Shape::Tab && active && deco.accent_line && !st.disabled {
            px::hline(painter, rect.x_range(), rect.top(), color32(color::ACCENT));
        }
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

/// One tab with [`TabExtra`]s (a dock tab); `accent_line` draws the focused-panel line when the
/// tab is active.
pub fn tab_with(
    ui: &mut Ui,
    label: &str,
    extra: TabExtra<'_>,
    active: bool,
    enabled: bool,
    accent_line: bool,
) -> Response {
    let id = ui.scope_id().with(("komponist_tab", label));
    let deco = Deco {
        extra,
        accent_line,
        width: None,
    };
    item_deco(ui, id, Shape::Tab, label, active, enabled, deco).0
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

#[allow(clippy::too_many_arguments)]
fn show_entries(
    ui: &mut Ui,
    shape: Shape,
    bar: Id,
    items: &[Item<'_>],
    selected: usize,
    extras: &[TabExtra<'_>],
    accent_line: bool,
    width: Option<f32>,
) -> (Option<usize>, bool) {
    let mut chosen = None;
    let mut focus_to = None;
    let mut ring = false;
    for (i, it) in items.iter().enumerate() {
        let active = i == selected;
        let deco = Deco {
            extra: extras.get(i).copied().unwrap_or_default(),
            accent_line,
            width,
        };
        let (r, st) = item_deco(
            ui,
            entry_id(bar, i),
            shape,
            it.label,
            active,
            it.enabled,
            deco,
        );
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
    extras: &'a [TabExtra<'a>],
    accent_line: bool,
}

impl<'a> TabBar<'a> {
    /// Tabs over `items`, showing and setting `selected`.
    pub fn new(name: &'a str, selected: &'a mut usize, items: &'a [Item<'a>]) -> Self {
        Self {
            name,
            selected,
            items,
            extras: &[],
            accent_line: false,
        }
    }

    /// Colour marks and secondary texts, one per item by index (missing entries: none).
    pub fn extras(mut self, extras: &'a [TabExtra<'a>]) -> Self {
        self.extras = extras;
        self
    }

    /// Draws the focused-panel accent line on the active tab (the bar's area has the focus).
    pub fn accent_line(mut self, on: bool) -> Self {
        self.accent_line = on;
        self
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
                show_entries(
                    ui,
                    Shape::Tab,
                    bar,
                    self.items,
                    *self.selected,
                    self.extras,
                    self.accent_line,
                    None,
                )
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
    equal_width: bool,
}

impl<'a> Segmented<'a> {
    /// Segments over `items`, showing and setting `selected`.
    pub fn new(name: &'a str, selected: &'a mut usize, items: &'a [Item<'a>]) -> Self {
        Self {
            name,
            selected,
            items,
            equal_width: false,
        }
    }

    /// The box fills the available width and the segments share it equally (the browser's
    /// "Alle · Instr. · Effekte · Dateien"), instead of each being as wide as its text.
    pub fn equal_width(mut self) -> Self {
        self.equal_width = true;
        self
    }
}

/// The width of each of `n` equal segments in a box `outer` wide (padding and gaps 2).
pub fn equal_segment_width(outer: f32, n: usize) -> f32 {
    let n = n.max(1) as f32;
    let inner = outer - 2.0 * space::SPACE_0_5 - (n - 1.0) * space::SPACE_0_5;
    (inner / n).max(0.0)
}

impl Widget for Segmented<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let bar = ui.scope_id().with(("komponist_segmented", self.name));
        let segment_width = self
            .equal_width
            .then(|| equal_segment_width(ui.available_width(), self.items.len()));
        let (items, selected) = (self.items, self.selected);
        let r = segmented_frame(
            ui,
            self.name,
            |(_, ring): &(Option<usize>, bool)| *ring,
            |ui| {
                show_entries(
                    ui,
                    Shape::Segment,
                    bar,
                    items,
                    *selected,
                    &[],
                    false,
                    segment_width,
                )
            },
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

    super::sample(
        ui,
        "Dock · Farbmarke, Zweittext, Akzentlinie (Panel fokussiert)",
        |ui| {
            dock_strip(ui, width);
        },
    );

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

/// The dock's tab strip of S01 as a still picture: "■ Lead A  Piano Roll" active with the
/// accent line, "Geräte", "Mixer" (`editorTabs` in `Screens.dc.html`).
fn dock_strip(ui: &mut Ui, width: f32) {
    let (strip, _) = ui.allocate_exact_size(vec2(width, size::TAB), Sense::hover());
    px::fill(ui.painter(), strip, 0.0, color32(color::SURFACE_0));
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(strip)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row.spacing_mut().item_spacing.x = 0.0;
    let lead = color32(komponist_tokens::TrackPalette::new(150).base);
    state::force(&mut row, State::REST, |ui| {
        tab_with(
            ui,
            "Lead A",
            TabExtra::new(Some(lead), "Piano Roll"),
            true,
            true,
            true,
        );
        tab(ui, "Geräte", false, true);
        tab(ui, "Mixer", false, true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot;

    #[test]
    fn dock_tab_snapshots() {
        let mut s = snapshot::Snapshots::new();
        let lead = color32(komponist_tokens::TrackPalette::new(150).base);
        for (accent, name) in [(true, "tab-dock-focused-panel"), (false, "tab-dock")] {
            s.add_state(name, State::REST, None, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    tab_with(
                        ui,
                        "Lead A",
                        TabExtra::new(Some(lead), "Piano Roll"),
                        true,
                        true,
                        accent,
                    );
                    tab_with(ui, "Geräte", TabExtra::default(), false, true, accent);
                });
            });
        }
        s.add_state("tab-dock-inactive", State::HOVERED, None, |ui| {
            tab_with(
                ui,
                "Beat B",
                TabExtra::new(Some(lead), "Step Sequencer"),
                false,
                true,
                true,
            );
        });
        s.finish();
    }

    #[test]
    fn extras_widen_the_tab_and_name_it() {
        let ctx = egui::Context::default();
        crate::install(&ctx);
        let mut widths = (0.0, 0.0);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let plain = tab(ui, "Lead A", true, true).rect.width();
            let extra = TabExtra::new(Some(Color32::RED), "Piano Roll");
            let rich = tab_with(ui, "Lead A ", extra, true, true, true)
                .rect
                .width();
            widths = (plain, rich);
        })
        .textures_delta
        .clear();
        assert!(widths.1 > widths.0 + dock_metrics::MARK + 2.0 * dock_metrics::GAP);
        assert_eq!(
            TabExtra::new(None, "Piano Roll").accessible_name("Lead A"),
            "Lead A · Piano Roll"
        );
        assert_eq!(TabExtra::default().accessible_name("Mixer"), "Mixer");
    }

    #[test]
    fn tab_bar_reports_extras_to_accesskit() {
        use egui_kittest::kittest::{NodeT as _, Queryable as _};
        let items = [Item::new("Lead A"), Item::new("Geräte"), Item::new("Mixer")];
        let extras = [TabExtra::new(Some(Color32::GREEN), "Piano Roll")];
        let mut selected = 0usize;
        let mut h = egui_kittest::Harness::builder().build_ui(move |ui| {
            crate::install(ui.ctx());
            ui.add(
                TabBar::new("Dock", &mut selected, &items)
                    .extras(&extras)
                    .accent_line(true),
            );
        });
        h.run();
        let tab = h.get_by_label("Lead A · Piano Roll");
        assert_eq!(tab.accesskit_node().role(), Role::Tab);
        assert!(tab.accesskit_node().is_selected().unwrap_or(false));
        h.get_by_label("Mixer").click();
        h.run();
    }

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
