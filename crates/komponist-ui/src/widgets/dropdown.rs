//! Dropdown (`ddBase`, `mItem`, `menuBox`, `dropdowns` of the design system).
//!
//! The field: `h 24 · min-w 96 · px 8 · radius.m`, value in `type.ui`, caret ▾ 8 × 5 at the right
//! with at least 12 px gap. It is as wide as its widest entry needs (at least
//! `size.dropdown-min-width`), so it keeps its width when the value changes.
//!
//! | | surface | text | caret |
//! |---|---|---|---|
//! | rest | surface.3 | text.1 | text.2 |
//! | hover | surface.4 | text.1 | text.1 |
//! | open / pressed | surface.5 | text.1 | text.1 |
//! | disabled | surface.2 | text.4 | text.4 |
//!
//! [`Dropdown::label`] puts a key inside the box before the value ("Raster 1/16 ▾", `text.3`,
//! 10 px gap) as the toolbars draw it; [`Dropdown::compact`] makes the field 20 high for panel
//! headers. Both set the field text in `type.small` (11 px) and drop the 96 px minimum width.
//!
//! The list opens below the field, as wide as the field: `surface.3`, `radius.l`, padding 4,
//! `shadow.float` with the 1 px edge; rows `size.menu-row` (24), `radius.s`, padding 0 8, a 14 px
//! ✓ column (`type.small`, text.2) for the current value. The row under the pointer or the
//! keyboard is `accent` with `accent.ink` text; disabled rows are text.4 and cannot be chosen.
//!
//! Keys (`ix.dropdown`): Enter, Space, ↓ or ↑ open the focused field; in the open list ↑ ↓ move,
//! Enter or Space take the entry, Esc closes, typing jumps to the first matching entry (type-ahead
//! prefix, reset after a 1 s pause). On a focused closed field, typing chooses the matching entry
//! directly. The mouse wheel over the closed field picks the previous (up) or next (down) value.
//! A press outside closes the list.

use egui::{
    Area, Color32, EventFilter, FocusDirection, Id, Key, Modifiers, Order, Rect, Response, Role,
    Sense, Ui, UiKind, Widget, WidgetInfo, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, radius, size, space};

use super::{TypeAhead, floating, metrics};
use crate::icons::{self, Caret};
use crate::state::{self, State};
use crate::{focus, fonts, px};

/// Gap between the field and its list (the design shows the list 8 px below the field).
pub const LIST_GAP: f32 = space::SPACE_2;

/// One entry of a dropdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item<'a> {
    /// The text ("1/16").
    pub label: &'a str,
    /// `false` for entries shown but not choosable ("Adaptiv").
    pub enabled: bool,
}

impl<'a> Item<'a> {
    /// A choosable entry.
    pub const fn new(label: &'a str) -> Self {
        Self {
            label,
            enabled: true,
        }
    }

    /// The entry, not choosable.
    #[must_use]
    pub const fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

impl<'a> From<&'a str> for Item<'a> {
    fn from(label: &'a str) -> Self {
        Self::new(label)
    }
}

/// Colours of the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldLook {
    /// Surface.
    pub fill: Color32,
    /// Value text.
    pub text: Color32,
    /// Caret ▾.
    pub caret: Color32,
    /// Draw the focus ring.
    pub focus_ring: bool,
}

/// The [`FieldLook`] of a field in `state`, `open` or not.
pub fn field_look(open: bool, state: State) -> FieldLook {
    let focus_ring = state.focused && !state.disabled;
    let (fill, text, caret) = if state.disabled {
        (color::SURFACE_2, color::TEXT_4, color::TEXT_4)
    } else if open || state.pressed {
        (color::SURFACE_5, color::TEXT_1, color::TEXT_1)
    } else if state.hovered {
        (color::SURFACE_4, color::TEXT_1, color::TEXT_1)
    } else {
        (color::SURFACE_3, color::TEXT_1, color::TEXT_2)
    };
    FieldLook {
        fill: color32(fill),
        text: color32(text),
        caret: color32(caret),
        focus_ring,
    }
}

/// Colours of a list row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowLook {
    /// Surface, `None` = none.
    pub fill: Option<Color32>,
    /// Text.
    pub text: Color32,
    /// The ✓.
    pub check: Color32,
}

/// The [`RowLook`] of a list row, `highlighted` (pointer or keyboard) and `enabled` or not.
pub fn row_look(highlighted: bool, enabled: bool) -> RowLook {
    if !enabled {
        RowLook {
            fill: None,
            text: color32(color::TEXT_4),
            check: color32(color::TEXT_4),
        }
    } else if highlighted {
        RowLook {
            fill: Some(color32(color::ACCENT)),
            text: color32(color::ACCENT_INK),
            check: color32(color::ACCENT_INK),
        }
    } else {
        RowLook {
            fill: None,
            text: color32(color::TEXT_1),
            check: color32(color::TEXT_2),
        }
    }
}

/// What happened in a list this pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListOutput {
    /// The enabled row the pointer moved onto, if any.
    pub hovered: Option<usize>,
    /// The enabled row clicked, if any.
    pub clicked: Option<usize>,
}

/// Draws the rows of a list, `width` wide (inside [`super::menu_frame`]): the current value
/// `selected` with ✓, the row `highlight` highlighted. Rows sense clicks but take no focus.
pub fn list_rows(
    ui: &mut Ui,
    items: &[Item<'_>],
    selected: usize,
    highlight: Option<usize>,
    width: f32,
) -> ListOutput {
    ui.vertical(|ui| list_rows_v(ui, items, selected, highlight, width))
        .inner
}

fn list_rows_v(
    ui: &mut Ui,
    items: &[Item<'_>],
    selected: usize,
    highlight: Option<usize>,
    width: f32,
) -> ListOutput {
    let style = typography::UI;
    let mut out = ListOutput::default();
    let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_pressed());
    ui.spacing_mut().item_spacing.y = 0.0;
    for (i, item) in items.iter().enumerate() {
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, size::MENU_ROW), super::sense(ui, Sense::CLICK));
        let (enabled, current) = (item.enabled && ui.is_enabled(), i == selected);
        response.widget_info(|| {
            WidgetInfo::selected(Role::ListBoxOption, enabled, current, item.label)
        });
        super::accesskit(&response, |n| n.set_selected(current));
        if enabled && response.hovered() && moved {
            out.hovered = Some(i);
        }
        if enabled && response.clicked() {
            out.clicked = Some(i);
        }
        let l = row_look(highlight == Some(i), enabled);
        let painter = ui.painter();
        if let Some(fill) = l.fill {
            px::fill(painter, rect, radius::S, fill);
        }
        let ppp = ui.pixels_per_point();
        let left = rect.left() + space::SPACE_2;
        if current {
            super::paint_check(ui, rect, left, l.check);
        }
        let galley = super::galley(ui, item.label, fonts::font_id(&style), style.line_height);
        let pos = super::text_pos(rect, &galley, left + metrics::CHECK_COLUMN, ppp);
        painter.galley(pos, galley, l.text);
    }
    out
}

/// Open-list state kept between passes.
#[derive(Clone, Debug, Default)]
struct Memory {
    highlight: Option<usize>,
    type_ahead: TypeAhead,
    list_rect: Option<Rect>,
}

/// A dropdown choosing one of `items` by index. The response is `changed()` when the value
/// changed.
#[must_use = "add it with `ui.add(…)`"]
pub struct Dropdown<'a> {
    name: &'a str,
    selected: &'a mut usize,
    items: &'a [Item<'a>],
    width: Option<f32>,
    still_open: Option<Option<usize>>,
    label: Option<&'a str>,
    compact: bool,
    device: bool,
}

impl<'a> Dropdown<'a> {
    /// A dropdown named `name` (accessible name, e.g. "Raster") over `items`, showing and setting
    /// `selected`.
    pub fn new(name: &'a str, selected: &'a mut usize, items: &'a [Item<'a>]) -> Self {
        Self {
            name,
            selected,
            items,
            width: None,
            still_open: None,
            label: None,
            compact: false,
            device: false,
        }
    }

    /// A label inside the box before the value ("Raster 1/16 ▾": `text.3`, then 10 px gap), as
    /// the toolbars draw it. Field text is then `type.small`; the list stays `type.ui`.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    /// `size.control-compact` (20) high with `type.small` text, for panel-header tools.
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// The dropdown of a device cell (`W_drop`): 18 high (`widget.dropdown.h`), `type.small`
    /// text, 6 px padding, a 6 × 4 caret; the list is the same menu.
    pub fn device(mut self) -> Self {
        self.device = true;
        self
    }

    fn pad_x(&self) -> f32 {
        if self.device {
            super::metrics::device::DROPDOWN_PADDING_X
        } else {
            space::SPACE_2
        }
    }

    fn caret_size(&self) -> [f32; 2] {
        if self.device {
            super::metrics::device::DROPDOWN_CARET
        } else {
            metrics::CARET
        }
    }

    /// The text style of the field.
    fn field_style(&self) -> typography::TypeStyle {
        if self.label.is_some() || self.compact || self.device {
            typography::SMALL
        } else {
            typography::UI
        }
    }

    fn height(&self) -> f32 {
        if self.device {
            komponist_tokens::widget::dropdown::H
        } else if self.compact {
            size::CONTROL_COMPACT
        } else {
            size::CONTROL
        }
    }

    /// Width of the label and its gap, 0 without a label.
    fn label_width(&self, ui: &Ui) -> f32 {
        self.label.map_or(0.0, |l| {
            let style = self.field_style();
            super::galley(ui, l, fonts::font_id(&style), style.line_height)
                .size()
                .x
                .ceil()
                + metrics::DROPDOWN_LABEL_GAP
        })
    }

    /// A fixed field width instead of the natural one.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Draws the list open below the field, in the layout, with row `highlight` highlighted
    /// (gallery and snapshots; use it inside [`state::force`]).
    pub fn open_still(mut self, highlight: Option<usize>) -> Self {
        self.still_open = Some(highlight);
        self
    }

    fn enabled_flags(&self) -> Vec<bool> {
        self.items.iter().map(|i| i.enabled).collect()
    }

    fn natural_width(&self, ui: &Ui) -> f32 {
        let widest = |style: typography::TypeStyle| {
            self.items
                .iter()
                .map(|it| {
                    super::galley(ui, it.label, fonts::font_id(&style), style.line_height)
                        .size()
                        .x
                })
                .fold(0.0_f32, f32::max)
                .ceil()
        };
        let field = 2.0 * space::SPACE_2
            + self.label_width(ui)
            + widest(self.field_style())
            + space::SPACE_3
            + metrics::CARET[0];
        let list = 2.0 * space::SPACE_1
            + 2.0 * space::SPACE_2
            + metrics::CHECK_COLUMN
            + widest(typography::UI);
        if self.label.is_some() || self.compact {
            // Toolbar fields are as wide as their content, not the 96 px minimum.
            field.max(list)
        } else {
            field.max(list).max(size::DROPDOWN_MIN_WIDTH)
        }
    }

    fn current_label(&self) -> &'a str {
        self.items.get(*self.selected).map_or("", |i| i.label)
    }

    fn paint_field(&self, ui: &Ui, rect: Rect, l: FieldLook) {
        let style = self.field_style();
        let painter = ui.painter();
        let ppp = ui.pixels_per_point();
        px::fill(painter, rect, radius::M, l.fill);
        let mut x = rect.left() + self.pad_x();
        if let Some(label) = self.label {
            let galley = super::galley(ui, label, fonts::font_id(&style), style.line_height);
            let pos = super::text_pos(rect, &galley, x, ppp);
            x += galley.size().x.ceil() + metrics::DROPDOWN_LABEL_GAP;
            let colour = if l.text == color32(color::TEXT_4) {
                l.text
            } else {
                color32(color::TEXT_3)
            };
            painter.galley(pos, galley, colour);
        }
        let galley = super::galley(
            ui,
            self.current_label(),
            fonts::font_id(&style),
            style.line_height,
        );
        let pos = super::text_pos(rect, &galley, x, ppp);
        if self.device {
            // Long values are cut before the caret.
            let room = rect.right() - self.pad_x() - self.caret_size()[0] - space::SPACE_1 - x;
            painter
                .with_clip_rect(Rect::from_min_max(
                    egui::pos2(x, rect.top()),
                    egui::pos2(x + room.max(0.0), rect.bottom()),
                ))
                .galley(pos, galley, l.text);
        } else {
            painter.galley(pos, galley, l.text);
        }
        let [w, h] = self.caret_size();
        let caret = px::snap_rect(
            Rect::from_min_size(
                egui::pos2(rect.right() - self.pad_x() - w, rect.center().y - h / 2.0),
                vec2(w, h),
            ),
            ppp,
        );
        icons::paint_caret(painter, caret, Caret::Down, l.caret);
        if l.focus_ring {
            focus::paint_ring(painter, rect, radius::M);
        }
    }

    fn show_list(
        &self,
        ui: &mut Ui,
        field: &Response,
        highlight: Option<usize>,
    ) -> (ListOutput, Rect) {
        let width = field.rect.width() - 2.0 * space::SPACE_1;
        let pos = field.rect.left_bottom() + vec2(0.0, LIST_GAP);
        let selected = *self.selected;
        let area = Area::new(field.id.with("list"))
            .order(Order::Foreground)
            .kind(UiKind::Popup)
            .fixed_pos(pos)
            .constrain(true)
            .accessibility_parent(field.id)
            .accessible_name(self.name)
            .role(Role::ListBox);
        let inner = area.show(ui.ctx(), |ui| {
            super::menu_frame()
                .show(ui, |ui| {
                    list_rows(ui, self.items, selected, highlight, width)
                })
                .inner
        });
        (inner.inner, inner.response.rect)
    }
}

impl Widget for Dropdown<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        if let Some(highlight) = self.still_open {
            let (items, selected) = (self.items, *self.selected);
            return ui
                .vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = LIST_GAP;
                    let r = self.field(ui, true);
                    let width = r.rect.width() - 2.0 * space::SPACE_1;
                    super::menu_frame()
                        .show(ui, |ui| list_rows(ui, items, selected, highlight, width));
                    r
                })
                .inner;
        }
        self.field(ui, false)
    }
}

impl Dropdown<'_> {
    fn field(self, ui: &mut Ui, still_open: bool) -> Response {
        let width = self.width.unwrap_or_else(|| self.natural_width(ui));
        let (rect, mut response) =
            ui.allocate_exact_size(vec2(width, self.height()), super::sense(ui, Sense::click()));
        let id = response.id;
        let list_id = id.with("list");
        let still = super::is_still(ui);
        let ctx = ui.ctx().clone();
        let mut mem: Memory = ui.data(|d| d.get_temp(list_id)).unwrap_or_default();
        let enabled = ui.is_enabled();
        let flags = self.enabled_flags();
        let mut open = !still && floating::is_open(&ctx, list_id);
        let mut choose: Option<usize> = None;

        if !still && enabled {
            let mut key_handled = false;
            if response.has_focus() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        id,
                        EventFilter {
                            vertical_arrows: true,
                            escape: open,
                            ..Default::default()
                        },
                    );
                });
            }
            let now = ui.input(|i| i.time);
            let typed = super::typed_text(ui);
            let typed = typed.trim();
            let labels = || self.items.iter().map(|i| (i.label, i.enabled));
            if open {
                let (down, up, accept, escape) = ui.input_mut(|i| {
                    (
                        i.consume_key(Modifiers::NONE, Key::ArrowDown),
                        i.consume_key(Modifiers::NONE, Key::ArrowUp),
                        i.consume_key(Modifiers::NONE, Key::Enter)
                            | i.consume_key(Modifiers::NONE, Key::Space),
                        i.consume_key(Modifiers::NONE, Key::Escape),
                    )
                });
                if down || up {
                    ui.memory_mut(|m| m.move_focus(FocusDirection::None));
                    let dir = if down { 1 } else { -1 };
                    mem.highlight =
                        super::step_enabled(&flags, mem.highlight, dir).or(mem.highlight);
                }
                if !typed.is_empty() {
                    let prefix = mem.type_ahead.push(typed, now).to_owned();
                    let h = mem.highlight.unwrap_or(0);
                    let start = if prefix.chars().count() == 1 {
                        h + 1
                    } else {
                        h
                    };
                    if let Some(i) =
                        super::type_ahead(labels(), &prefix, start % flags.len().max(1))
                    {
                        mem.highlight = Some(i);
                    }
                }
                if accept {
                    choose = mem.highlight.filter(|&h| flags.get(h) == Some(&true));
                    open = false;
                    key_handled = true;
                }
                if escape {
                    open = false;
                    key_handled = true;
                }
            } else if response.has_focus() {
                let (down, up) = ui.input_mut(|i| {
                    (
                        i.consume_key(Modifiers::NONE, Key::ArrowDown),
                        i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    )
                });
                if down || up {
                    open = true;
                    mem.highlight = Some(*self.selected);
                }
                if !typed.is_empty() {
                    let prefix = mem.type_ahead.push(typed, now).to_owned();
                    let start = if prefix.chars().count() == 1 {
                        *self.selected + 1
                    } else {
                        *self.selected
                    };
                    choose = super::type_ahead(labels(), &prefix, start % flags.len().max(1));
                }
            }
            if !open && response.hovered() {
                let steps = super::wheel_steps(ui, id.with("wheel"));
                if steps != 0 {
                    // Wheel up = previous value.
                    let dir = -steps.signum();
                    let mut v = Some(*self.selected);
                    for _ in 0..steps.abs() {
                        v = super::step_enabled(&flags, v, dir).or(v);
                    }
                    choose = v;
                }
            }
            if response.clicked() && !key_handled {
                open = !open;
                if open {
                    mem.highlight = Some(*self.selected);
                    mem.type_ahead = TypeAhead::default();
                }
            }
            if open && floating::is_open(&ctx, list_id) {
                let list = mem.list_rect.unwrap_or(Rect::NOTHING);
                if super::pressed_outside(ui.ctx(), &[rect, list]) {
                    open = false;
                }
            }
        }

        let st = State::of(ui, &response);
        let current = self.current_label().to_owned();
        let (name, is_enabled) = (self.name, enabled && !st.disabled);
        response.widget_info(|| WidgetInfo {
            current_text_value: Some(current.clone()),
            ..WidgetInfo::labeled(Role::ComboBox, is_enabled, name)
        });
        super::accesskit(&response, |n| n.set_expanded(open || still_open));
        if ui.is_rect_visible(rect) {
            self.paint_field(ui, rect, field_look(open || still_open, st));
        }

        if open {
            floating::open(&ctx, list_id);
            let (out, list_rect) = self.show_list(ui, &response, mem.highlight);
            mem.list_rect = Some(list_rect);
            if let Some(h) = out.hovered {
                mem.highlight = Some(h);
            }
            if let Some(c) = out.clicked {
                choose = Some(c);
                floating::close(&ctx, list_id);
            }
        } else {
            floating::close(&ctx, list_id);
            mem.list_rect = None;
        }
        if !still {
            ui.data_mut(|d| d.insert_temp(list_id, mem));
        }

        if let Some(c) = choose
            && c != *self.selected
            && flags.get(c) == Some(&true)
        {
            *self.selected = c;
            response.mark_changed();
        }
        response
    }
}

/// The dropdown entries of the design system's example (`dropdowns`).
pub const GRID_ITEMS: [Item<'static>; 6] = [
    Item::new("1/4"),
    Item::new("1/8"),
    Item::new("1/16"),
    Item::new("1/32"),
    Item::new("1/16 Triole"),
    Item::new("Adaptiv").disabled(),
];

/// The "Dropdown" card of the gallery: standard, hover, open with a highlighted row,
/// disabled; then a live dropdown.
pub fn gallery(ui: &mut Ui) {
    let samples = [
        ("standard", State::REST, None),
        ("hover", State::HOVERED, None),
        ("offen · Zeile hover", State::REST, Some(3)),
        ("deaktiviert", State::DISABLED, None),
    ];
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        for (caption, st, open) in samples {
            super::sample(ui, caption, |ui| {
                let mut selected = 2;
                let name = format!("Raster · {caption}");
                state::force(ui, st, |ui| {
                    let d = Dropdown::new(&name, &mut selected, &GRID_ITEMS);
                    match open {
                        Some(h) => ui.add(d.open_still(Some(h))),
                        None => ui.add(d),
                    }
                });
            });
        }
    });
    // The toolbars' variants: a label inside the box, 24 and 20 high.
    let mut grid = 2;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        ui.add(Dropdown::new("Raster · Werkzeugleiste", &mut grid, &GRID_ITEMS).label("Raster"));
        ui.add(
            Dropdown::new("Raster · Kopfleiste", &mut grid, &GRID_ITEMS)
                .label("Raster")
                .compact(),
        );
    });
    let id = Id::unique("komponist_gallery_dropdown_live");
    let mut selected: usize = ui.data(|d| d.get_temp(id)).unwrap_or(2);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_3;
        ui.add(Dropdown::new("Raster", &mut selected, &GRID_ITEMS));
        super::caption_text(ui, &format!("live · Raster {}", GRID_ITEMS[selected].label));
    });
    ui.data_mut(|d| d.insert_temp(id, selected));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_colours_follow_dd_base() {
        let rest = field_look(false, State::REST);
        assert_eq!(rest.fill, color32(color::SURFACE_3));
        assert_eq!(rest.caret, color32(color::TEXT_2));
        let hover = field_look(false, State::HOVERED);
        assert_eq!(
            (hover.fill, hover.caret),
            (color32(color::SURFACE_4), color32(color::TEXT_1))
        );
        let open = field_look(true, State::REST);
        assert_eq!(open.fill, color32(color::SURFACE_5));
        let dis = field_look(false, State::DISABLED);
        assert_eq!(
            (dis.fill, dis.text, dis.caret),
            (
                color32(color::SURFACE_2),
                color32(color::TEXT_4),
                color32(color::TEXT_4)
            )
        );
    }

    #[test]
    fn rows_follow_m_item() {
        assert_eq!(row_look(true, true).fill, Some(color32(color::ACCENT)));
        assert_eq!(row_look(true, true).text, color32(color::ACCENT_INK));
        assert_eq!(row_look(false, true).check, color32(color::TEXT_2));
        assert_eq!(row_look(true, false).fill, None);
        assert_eq!(row_look(false, false).text, color32(color::TEXT_4));
    }
}
