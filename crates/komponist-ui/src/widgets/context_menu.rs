//! Context menu (`menu`, `menuColors` of the design system: `w 220 · Zeile 24 · p 4 · fläche-3 ·
//! shadow.float`).
//!
//! A floating panel at the pointer: rows 220 × 24 (`size.menu-width`, `size.menu-row`) in a
//! `surface.3` box with padding 4, `radius.l`, `shadow.float` and the 1 px edge. A row: padding
//! 0 8 0 4, a 14 px ✓ column (`type.small`, text.2), 4 px gap, the label in `type.ui` (text.1),
//! the shortcut on the right in Mono 10 text.3 ("damit man sie nebenbei lernt"); a submenu row
//! shows ▸ there. Separators: 1 px `line.2`, inset 6, 4 above and below.
//!
//! | row | surface | text | shortcut |
//! |---|---|---|---|
//! | normal | – | text.1 | text.3 |
//! | highlighted (pointer, keys) | accent | accent.ink | accent.ink |
//! | its submenu is open | surface.4 | text.1 | text.3 |
//! | disabled | – | text.4 | text.4 |
//!
//! Submenus open to the right of their row, or to the left where the screen ends on the right (the
//! transport bar's "⋯" menu, #95): a list of entries, or the colour grid of "Farbe"
//! (7 × 2 swatches 20 × 16, `radius.s`, padding 8, gap 4; the current colour with a 1 px text.1
//! ring at 1 px distance).
//!
//! Keys (`ix.menu`): ↑ ↓ move, → or Enter on a submenu row opens it, ← closes it, Enter or Space
//! runs the entry, Esc closes the submenu, then the menu. In the colour grid the arrows move
//! between swatches. A press outside closes the menu. The entries carry the caller's action
//! type `A`; [`ContextMenu::show_on`] returns the chosen one.

use egui::{
    Area, Color32, Context, FocusDirection, Id, Key, Modifiers, Order, Pos2, Rect, Response, Role,
    Sense, Ui, UiKind, WidgetInfo, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, radius, shadow, size, space};

use super::{floating, metrics};
use crate::icons::{self, Caret};
use crate::state::{self, State};
use crate::{fonts, px};

/// A colour swatch of a colour submenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Swatch<'a> {
    /// The colour.
    pub color: Color32,
    /// Its German name, for AccessKit ("Grün").
    pub name: &'a str,
}

/// The colour grid of a submenu ("Farbe").
#[derive(Clone, Copy, Debug)]
pub struct Colors<'a, A> {
    /// The swatches, row by row.
    pub swatches: &'a [Swatch<'a>],
    /// The current colour.
    pub selected: Option<usize>,
    /// The action for picking swatch `i`.
    pub pick: fn(usize) -> A,
}

/// What an entry does.
#[derive(Clone, Copy, Debug)]
pub enum Kind<'a, A> {
    /// Runs an action.
    Action(A),
    /// Opens a submenu of entries.
    Submenu(&'a [Entry<'a, A>]),
    /// Opens a colour grid.
    Colors(Colors<'a, A>),
    /// A separator line.
    Separator,
}

/// One entry of a menu.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a, A> {
    /// The label ("Duplizieren").
    pub label: &'a str,
    /// The shortcut ("Strg D").
    pub shortcut: Option<&'a str>,
    /// `false`: shown in text.4, not choosable.
    pub enabled: bool,
    /// A checkable entry and its state ("Loop ✓").
    pub checked: Option<bool>,
    /// What it does.
    pub kind: Kind<'a, A>,
    elide_label: bool,
}

impl<'a, A> Entry<'a, A> {
    fn with(label: &'a str, kind: Kind<'a, A>) -> Self {
        Self {
            label,
            shortcut: None,
            enabled: true,
            checked: None,
            kind,
            elide_label: false,
        }
    }

    /// An entry running `action`.
    pub fn action(label: &'a str, action: A) -> Self {
        Self::with(label, Kind::Action(action))
    }

    /// An entry opening `entries`.
    pub fn submenu(label: &'a str, entries: &'a [Entry<'a, A>]) -> Self {
        Self::with(label, Kind::Submenu(entries))
    }

    /// An entry opening a colour grid.
    pub fn colors(label: &'a str, colors: Colors<'a, A>) -> Self {
        Self::with(label, Kind::Colors(colors))
    }

    /// A separator.
    pub fn separator() -> Self {
        Self::with("", Kind::Separator)
    }

    /// Shows `shortcut` on the right.
    #[must_use]
    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// Not choosable.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// A checkable entry, `checked` or not.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// Elide a dynamic label at the end of its row. AccessKit retains the original text.
    #[must_use]
    pub fn elide_label(mut self) -> Self {
        self.elide_label = true;
        self
    }

    fn is_separator(&self) -> bool {
        matches!(self.kind, Kind::Separator)
    }

    fn has_submenu(&self) -> bool {
        matches!(self.kind, Kind::Submenu(_) | Kind::Colors(_))
    }

    fn choosable(&self) -> bool {
        self.enabled && !self.is_separator()
    }
}

/// Colours of a menu row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowLook {
    /// Surface, `None` = none.
    pub fill: Option<Color32>,
    /// Label.
    pub text: Color32,
    /// Shortcut and ▸.
    pub shortcut: Color32,
    /// The ✓.
    pub check: Color32,
}

/// The [`RowLook`] of a row: `highlighted` (pointer or keys), with its submenu open, `enabled`.
pub fn row_look(highlighted: bool, submenu_open: bool, enabled: bool) -> RowLook {
    let (fill, text, shortcut, check) = if !enabled {
        (None, color::TEXT_4, color::TEXT_4, color::TEXT_4)
    } else if highlighted {
        (
            Some(color::ACCENT),
            color::ACCENT_INK,
            color::ACCENT_INK,
            color::ACCENT_INK,
        )
    } else if submenu_open {
        (
            Some(color::SURFACE_4),
            color::TEXT_1,
            color::TEXT_3,
            color::TEXT_2,
        )
    } else {
        (None, color::TEXT_1, color::TEXT_3, color::TEXT_2)
    };
    RowLook {
        fill: fill.map(color32),
        text: color32(text),
        shortcut: color32(shortcut),
        check: color32(check),
    }
}

/// Height of a separator: 1 px line with 4 px above and below.
const SEPARATOR_HEIGHT: f32 = 1.0 + 2.0 * space::SPACE_1;

/// What happened in one panel this pass.
#[derive(Clone, Debug, Default)]
struct PanelOut {
    hovered: Option<usize>,
    clicked: Option<usize>,
    rows: Vec<Rect>,
}

fn entry_rows<A>(
    ui: &mut Ui,
    entries: &[Entry<'_, A>],
    highlight: Option<usize>,
    sub_open: Option<usize>,
) -> PanelOut {
    ui.vertical(|ui| entry_rows_v(ui, entries, highlight, sub_open))
        .inner
}

fn entry_rows_v<A>(
    ui: &mut Ui,
    entries: &[Entry<'_, A>],
    highlight: Option<usize>,
    sub_open: Option<usize>,
) -> PanelOut {
    let mut out = PanelOut::default();
    let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
    let ppp = ui.pixels_per_point();
    ui.spacing_mut().item_spacing.y = 0.0;
    for (i, e) in entries.iter().enumerate() {
        if e.is_separator() {
            let (rect, _) =
                ui.allocate_exact_size(vec2(size::MENU_WIDTH, SEPARATOR_HEIGHT), Sense::hover());
            let inset = space::SPACE_1_5;
            px::hline(
                ui.painter(),
                rect.left() + inset..=rect.right() - inset,
                rect.top() + space::SPACE_1,
                color32(color::LINE_2),
            );
            out.rows.push(rect);
            continue;
        }
        let (rect, response) = ui.allocate_exact_size(
            vec2(size::MENU_WIDTH, size::MENU_ROW),
            super::sense(ui, Sense::CLICK),
        );
        out.rows.push(rect);
        let enabled = e.enabled && ui.is_enabled();
        let role = if e.checked.is_some() {
            Role::MenuItemCheckBox
        } else {
            Role::MenuItem
        };
        response.widget_info(|| WidgetInfo {
            selected: e.checked,
            ..WidgetInfo::labeled(role, enabled, e.label)
        });
        if e.has_submenu() {
            let open = sub_open == Some(i);
            super::accesskit(&response, |n| {
                n.set_has_popup(egui::accesskit::HasPopup::Menu);
                n.set_expanded(open);
            });
        }
        if let Some(sc) = e.shortcut {
            let sc = sc.to_owned();
            super::accesskit(&response, |n| n.set_keyboard_shortcut(sc));
        }
        if enabled && response.hovered() && moved {
            out.hovered = Some(i);
        }
        if enabled && response.clicked() {
            out.clicked = Some(i);
        }

        let l = row_look(highlight == Some(i), sub_open == Some(i), enabled);
        let painter = ui.painter();
        if let Some(fill) = l.fill {
            px::fill(painter, rect, radius::S, fill);
        }
        let check_left = rect.left() + space::SPACE_1;
        if e.checked == Some(true) {
            super::paint_check(ui, rect, check_left, l.check);
        }
        let x = check_left + metrics::CHECK_COLUMN + space::SPACE_1;
        let right = rect.right() - space::SPACE_2;
        let mut label_right = right;
        if e.has_submenu() {
            let [w, h] = metrics::CARET;
            let caret = px::snap_rect(
                Rect::from_min_size(egui::pos2(right - h, rect.center().y - w / 2.0), vec2(h, w)),
                ppp,
            );
            icons::paint_caret(painter, caret, Caret::Right, l.shortcut);
            label_right = caret.left() - space::SPACE_1;
        } else if let Some(sc) = e.shortcut {
            let g = super::galley(
                ui,
                sc,
                fonts::font_id(&typography::VALUE_S),
                typography::VALUE_S.line_height,
            );
            let x = right - g.size().x;
            painter.galley(super::text_pos(rect, &g, x, ppp), g, l.shortcut);
            label_right = x - space::SPACE_1;
        }
        let mut label = super::galley(
            ui,
            e.label,
            fonts::font_id(&typography::UI),
            typography::UI.line_height,
        );
        let label_width = (label_right - x).max(0.0);
        if e.elide_label && label.size().x > label_width {
            let mut job = (*label.job).clone();
            job.wrap.max_width = label_width;
            job.wrap.max_rows = 1;
            job.break_on_newline = false;
            label = painter.layout_job(job);
        }
        painter.galley(super::text_pos(rect, &label, x, ppp), label, l.text);
    }
    out
}

/// Width of the colour grid.
fn grid_width() -> f32 {
    let cols = metrics::SWATCH_COLUMNS as f32;
    cols * metrics::SWATCH[0] + (cols - 1.0) * space::SPACE_1
}

fn color_cells<A>(ui: &mut Ui, colors: &Colors<'_, A>, highlight: Option<usize>) -> PanelOut {
    let mut out = PanelOut::default();
    let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
    let [w, h] = metrics::SWATCH;
    let origin = ui.cursor().min;
    let rows = colors.swatches.len().div_ceil(metrics::SWATCH_COLUMNS);
    let total = vec2(
        grid_width(),
        rows as f32 * h + rows.saturating_sub(1) as f32 * space::SPACE_1,
    );
    ui.allocate_exact_size(total, Sense::hover());
    for (i, s) in colors.swatches.iter().enumerate() {
        let (col, row) = (i % metrics::SWATCH_COLUMNS, i / metrics::SWATCH_COLUMNS);
        let rect = Rect::from_min_size(
            origin
                + vec2(
                    col as f32 * (w + space::SPACE_1),
                    row as f32 * (h + space::SPACE_1),
                ),
            vec2(w, h),
        );
        let response = ui.interact(
            rect,
            ui.scope_id().with(("swatch", i)),
            super::sense(ui, Sense::CLICK),
        );
        let current = colors.selected == Some(i);
        let enabled = ui.is_enabled();
        response
            .widget_info(|| WidgetInfo::selected(Role::MenuItemRadio, enabled, current, s.name));
        if response.hovered() && moved {
            out.hovered = Some(i);
        }
        if response.clicked() {
            out.clicked = Some(i);
        }
        out.rows.push(rect);
        let painter = ui.painter();
        px::fill(painter, rect, radius::S, s.color);
        if highlight == Some(i) {
            px::outline(painter, rect, radius::S, 1.0, color32(color::ACCENT));
        } else if current {
            px::outline(painter, rect, radius::S, 1.0, color32(color::TEXT_1));
        }
    }
    out
}

/// The open menu's state.
#[derive(Clone, Copy, Debug)]
struct MenuState {
    pos: Pos2,
    highlight: Option<usize>,
    sub: Option<usize>,
    sub_highlight: Option<usize>,
    in_sub: bool,
    rects: [Rect; 2],
}

impl MenuState {
    fn at(pos: Pos2) -> Self {
        Self {
            pos,
            highlight: None,
            sub: None,
            sub_highlight: None,
            in_sub: false,
            rects: [Rect::NOTHING; 2],
        }
    }
}

/// A context menu over `entries`.
#[must_use = "call `.show_on(&response)`"]
pub struct ContextMenu<'a, A> {
    name: &'a str,
    entries: &'a [Entry<'a, A>],
}

impl<'a, A: Clone> ContextMenu<'a, A> {
    /// A menu named `name` (accessible name, e.g. "Clip") over `entries`.
    pub fn new(name: &'a str, entries: &'a [Entry<'a, A>]) -> Self {
        Self { name, entries }
    }

    /// The id of the context menu of the widget `target`.
    pub fn id_for(target: Id) -> Id {
        target.with("komponist_context_menu")
    }

    /// Opens the menu of `target` at `pos` (also for the keyboard: menu key, Shift F10).
    pub fn open_at(ctx: &Context, target: Id, pos: Pos2) {
        let id = Self::id_for(target);
        floating::open(ctx, id);
        ctx.data_mut(|d| d.insert_temp(id, MenuState::at(pos)));
    }

    /// `true` if the menu of `target` is open.
    pub fn is_open(ctx: &Context, target: Id) -> bool {
        floating::is_open(ctx, Self::id_for(target))
    }

    /// Opens the menu at the pointer on a secondary click on `target`, shows it while open, and
    /// returns the action chosen in this pass, if any (the menu closes then).
    pub fn show_on(self, target: &Response) -> Option<A> {
        let ctx = target.ctx.clone();
        let id = Self::id_for(target.id);
        if target.secondary_clicked() {
            let pos = target
                .interact_pointer_pos()
                .or_else(|| ctx.pointer_latest_pos())
                .unwrap_or_else(|| target.rect.center());
            Self::open_at(&ctx, target.id, pos);
        }
        if !floating::is_open(&ctx, id) {
            return None;
        }
        let mut st: MenuState = ctx
            .data(|d| d.get_temp(id))
            .unwrap_or_else(|| MenuState::at(target.rect.center()));
        let mut chosen = None;
        let mut close = self.keys(&ctx, &mut st, &mut chosen);

        // The main panel.
        let main = Area::new(id)
            .order(Order::Foreground)
            .kind(UiKind::Menu)
            .fixed_pos(st.pos)
            .constrain(true)
            .accessibility_parent(target.id)
            .accessible_name(self.name)
            .show(&ctx, |ui| {
                super::menu_frame()
                    .show(ui, |ui| entry_rows(ui, self.entries, st.highlight, st.sub))
                    .inner
            });
        let out = main.inner;
        st.rects[0] = main.response.rect;
        if let Some(h) = out.hovered {
            st.highlight = Some(h);
            st.in_sub = false;
            st.sub = self.entries[h].has_submenu().then_some(h);
            if st.sub.is_none() {
                st.sub_highlight = None;
            }
        }
        if let Some(c) = out.clicked {
            match &self.entries[c].kind {
                Kind::Action(a) => chosen = Some(a.clone()),
                Kind::Submenu(_) | Kind::Colors(_) => st.sub = Some(c),
                Kind::Separator => {}
            }
        }

        // The submenu.
        let last_sub = std::mem::replace(&mut st.rects[1], Rect::NOTHING);
        if let Some(s) = st.sub
            && let Some(row) = out.rows.get(s)
        {
            let entry = &self.entries[s];
            let edge = shadow::EDGE.spread;
            // Right of the menu if it fits (its width from the last pass, else the menu's),
            // else left of it.
            let width = if last_sub.is_positive() {
                last_sub.width()
            } else {
                st.rects[0].width()
            };
            let right = st.rects[0].right() - 2.0 * edge + space::SPACE_1;
            let (x, pivot) = if right + width <= ctx.content_rect().right() {
                (right, egui::Align2::LEFT_TOP)
            } else {
                (
                    st.rects[0].left() + 2.0 * edge - space::SPACE_1,
                    egui::Align2::RIGHT_TOP,
                )
            };
            let y = match entry.kind {
                Kind::Colors(_) => row.top() - 2.0 * edge,
                _ => row.top() - edge - space::SPACE_1,
            };
            let sub = Area::new(id.with("sub"))
                .order(Order::Foreground)
                .kind(UiKind::Menu)
                .pivot(pivot)
                .fixed_pos(egui::pos2(x, y))
                .constrain(true)
                .accessibility_parent(id)
                .accessible_name(entry.label)
                .show(&ctx, |ui| match &entry.kind {
                    Kind::Submenu(entries) => {
                        super::menu_frame()
                            .show(ui, |ui| entry_rows(ui, entries, st.sub_highlight, None))
                            .inner
                    }
                    Kind::Colors(colors) => {
                        super::menu_frame()
                            .inner_margin(egui::Margin::same(space::SPACE_2 as i8))
                            .show(ui, |ui| color_cells(ui, colors, st.sub_highlight))
                            .inner
                    }
                    _ => PanelOut::default(),
                });
            st.rects[1] = sub.response.rect;
            let sub_out = sub.inner;
            if let Some(h) = sub_out.hovered {
                st.sub_highlight = Some(h);
                st.in_sub = true;
            }
            if let Some(c) = sub_out.clicked {
                chosen = chosen.or_else(|| self.sub_action(s, c));
            }
        }

        if chosen.is_some() {
            close = true;
        }
        if !close && super::pressed_outside(&ctx, &st.rects) && !target.secondary_clicked() {
            close = true;
        }
        if close {
            floating::close(&ctx, id);
            ctx.data_mut(|d| d.remove::<MenuState>(id));
        } else {
            ctx.data_mut(|d| d.insert_temp(id, st));
        }
        chosen
    }

    /// The action of entry `c` of submenu `s`, if choosable.
    fn sub_action(&self, s: usize, c: usize) -> Option<A> {
        match &self.entries.get(s)?.kind {
            Kind::Submenu(entries) => match &entries.get(c)?.kind {
                Kind::Action(a) if entries[c].enabled => Some(a.clone()),
                _ => None,
            },
            Kind::Colors(colors) => (c < colors.swatches.len()).then(|| (colors.pick)(c)),
            _ => None,
        }
    }

    /// Keyboard handling; returns `true` if the menu is to close.
    fn keys(&self, ctx: &Context, st: &mut MenuState, chosen: &mut Option<A>) -> bool {
        let pressed = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        let (down, up, left, right) = (
            pressed(Key::ArrowDown),
            pressed(Key::ArrowUp),
            pressed(Key::ArrowLeft),
            pressed(Key::ArrowRight),
        );
        let accept = pressed(Key::Enter) | pressed(Key::Space);
        let escape = pressed(Key::Escape);
        if down || up || left || right {
            ctx.memory_mut(|m| m.move_focus(FocusDirection::None));
        }
        let main_flags: Vec<bool> = self.entries.iter().map(Entry::choosable).collect();
        let sub = st.sub.map(|s| &self.entries[s].kind);

        if st.in_sub
            && let Some(kind) = sub
        {
            match kind {
                Kind::Colors(colors) => {
                    let n = colors.swatches.len();
                    let cols = metrics::SWATCH_COLUMNS;
                    let cur = st.sub_highlight.unwrap_or(0);
                    if left && cur.is_multiple_of(cols) {
                        st.in_sub = false;
                        st.sub_highlight = None;
                    } else {
                        let next = if left {
                            cur.saturating_sub(1)
                        } else if right {
                            (cur + 1).min(n - 1)
                        } else if down {
                            if cur + cols < n { cur + cols } else { cur }
                        } else if up {
                            cur.checked_sub(cols).unwrap_or(cur)
                        } else {
                            cur
                        };
                        if left || right || up || down {
                            st.sub_highlight = Some(next);
                        }
                    }
                    if accept && let Some(h) = st.sub_highlight {
                        *chosen = Some((colors.pick)(h));
                    }
                }
                Kind::Submenu(entries) => {
                    let flags: Vec<bool> = entries.iter().map(Entry::choosable).collect();
                    if down || up {
                        let dir = if down { 1 } else { -1 };
                        st.sub_highlight =
                            super::step_enabled(&flags, st.sub_highlight, dir).or(st.sub_highlight);
                    }
                    if left {
                        st.in_sub = false;
                        st.sub_highlight = None;
                    }
                    if accept
                        && let Some(h) = st.sub_highlight
                        && let Some(s) = st.sub
                    {
                        *chosen = self.sub_action(s, h);
                    }
                }
                _ => {}
            }
            if escape {
                st.in_sub = false;
                st.sub_highlight = None;
                st.sub = None;
            }
            return false;
        }

        if down || up {
            let dir = if down { 1 } else { -1 };
            st.highlight = super::step_enabled(&main_flags, st.highlight, dir).or(st.highlight);
            if st.sub != st.highlight {
                st.sub = None;
            }
        }
        let opens = |st: &MenuState| {
            st.highlight
                .filter(|&h| self.entries[h].has_submenu() && self.entries[h].enabled)
        };
        if (right || accept)
            && let Some(h) = opens(st)
        {
            st.sub = Some(h);
            st.in_sub = true;
            st.sub_highlight = match &self.entries[h].kind {
                Kind::Colors(c) => Some(c.selected.unwrap_or(0)),
                Kind::Submenu(entries) => {
                    let flags: Vec<bool> = entries.iter().map(Entry::choosable).collect();
                    super::step_enabled(&flags, None, 1)
                }
                _ => None,
            };
            return false;
        }
        if accept
            && let Some(h) = st.highlight
            && let Kind::Action(a) = &self.entries[h].kind
            && self.entries[h].enabled
        {
            *chosen = Some(a.clone());
        }
        if escape {
            if st.sub.is_some() {
                st.sub = None;
                return false;
            }
            return true;
        }
        false
    }

    /// Draws the menu open in place (not floating), for the gallery and snapshots: row
    /// `highlight` highlighted, the submenu of row `submenu` open to the right. Use it inside
    /// [`state::force`], so the rows are still pictures.
    pub fn show_still(&self, ui: &mut Ui, highlight: Option<usize>, submenu: Option<usize>) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::SPACE_1 - 2.0 * shadow::EDGE.spread;
            let main =
                super::menu_frame().show(ui, |ui| entry_rows(ui, self.entries, highlight, submenu));
            let rows = main.inner.rows;
            let Some(s) = submenu else { return };
            let (Some(row), Some(entry)) = (rows.get(s), self.entries.get(s)) else {
                return;
            };
            ui.vertical(|ui| {
                let top = main.response.rect.top();
                let edge = shadow::EDGE.spread;
                match &entry.kind {
                    Kind::Colors(colors) => {
                        ui.add_space(row.top() - top - edge);
                        super::menu_frame()
                            .inner_margin(egui::Margin::same(space::SPACE_2 as i8))
                            .show(ui, |ui| color_cells(ui, colors, None));
                    }
                    Kind::Submenu(entries) => {
                        ui.add_space(row.top() - top - space::SPACE_1);
                        super::menu_frame().show(ui, |ui| entry_rows(ui, entries, None, None));
                    }
                    _ => {}
                }
            });
        });
    }
}

/// The German names of the 14 track hues, in the order of `komponist_tokens::track::HUES`.
pub const HUE_NAMES: [&str; 14] = [
    "Rot", "Zinnober", "Orange", "Gelb", "Limette", "Grün", "Smaragd", "Petrol", "Cyan", "Blau",
    "Indigo", "Violett", "Magenta", "Rosa",
];

/// The 14 track colours (`base`) as swatches, in hue order.
pub fn track_swatches() -> [Swatch<'static>; 14] {
    let f = komponist_tokens::track::formula::BASE;
    std::array::from_fn(|i| Swatch {
        color: color32(komponist_tokens::oklch_to_srgb(
            f.l,
            f.c,
            f64::from(komponist_tokens::track::HUES[i].degrees),
        )),
        name: HUE_NAMES[i],
    })
}

/// The actions of the gallery's example menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoAction {
    /// Ausschneiden.
    Cut,
    /// Kopieren.
    Copy,
    /// Einfügen.
    Paste,
    /// Duplizieren.
    Duplicate,
    /// Umbenennen.
    Rename,
    /// Farbe: hue index.
    Color(usize),
    /// Quantisieren.
    Quantize,
    /// Loop.
    Loop,
    /// Löschen.
    Delete,
}

/// The entries of the design system's example menu (`menu`); `swatches` for "Farbe", `hue` the
/// current colour, `looped` the "Loop" check.
pub fn demo_entries<'a>(
    swatches: &'a [Swatch<'a>],
    hue: Option<usize>,
    looped: bool,
) -> [Entry<'a, DemoAction>; 12] {
    [
        Entry::action("Ausschneiden", DemoAction::Cut).shortcut("Strg X"),
        Entry::action("Kopieren", DemoAction::Copy).shortcut("Strg C"),
        Entry::action("Einfügen", DemoAction::Paste)
            .shortcut("Strg V")
            .disabled(),
        Entry::separator(),
        Entry::action("Duplizieren", DemoAction::Duplicate).shortcut("Strg D"),
        Entry::action("Umbenennen", DemoAction::Rename).shortcut("Strg R"),
        Entry::colors(
            "Farbe",
            Colors {
                swatches,
                selected: hue,
                pick: DemoAction::Color,
            },
        ),
        Entry::separator(),
        Entry::action("Quantisieren", DemoAction::Quantize).shortcut("Q"),
        Entry::action("Loop", DemoAction::Loop)
            .shortcut("Strg L")
            .checked(looped),
        Entry::separator(),
        Entry::action("Löschen", DemoAction::Delete).shortcut("Entf"),
    ]
}

/// The "Kontextmenü" card of the gallery: the design's menu with "Duplizieren" highlighted and
/// the colour submenu open, then a live target for a right click.
pub fn gallery(ui: &mut Ui) {
    let swatches = track_swatches();
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    state::force(ui, State::REST, |ui| {
        let entries = demo_entries(&swatches, Some(5), true);
        ContextMenu::new("Clip", &entries).show_still(ui, Some(4), Some(6));
    });

    let id = Id::unique("komponist_gallery_menu_live");
    let (mut hue, mut looped, mut last): (usize, bool, Option<DemoAction>) =
        ui.data(|d| d.get_temp(id)).unwrap_or((5, true, None));
    let entries = demo_entries(&swatches, Some(hue), looped);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_3;
        let (rect, target) =
            ui.allocate_exact_size(vec2(size::MENU_WIDTH, size::TRANSPORT), Sense::click());
        target.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Rechtsklick-Ziel"));
        px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_2));
        let g = super::galley(
            ui,
            "Rechtsklick hier",
            fonts::font_id(&typography::UI),
            typography::UI.line_height,
        );
        let pos = super::text_pos_centered(rect, &g, ui.pixels_per_point());
        ui.painter().galley(pos, g, color32(color::TEXT_3));
        if let Some(a) = ContextMenu::new("Clip", &entries).show_on(&target) {
            match a {
                DemoAction::Color(h) => hue = h,
                DemoAction::Loop => looped = !looped,
                _ => {}
            }
            last = Some(a);
        }
        let text = match last {
            Some(DemoAction::Color(h)) => format!("live · Farbe {}", HUE_NAMES[h]),
            Some(a) => format!("live · {a:?}"),
            None => "live".to_owned(),
        };
        super::caption_text(ui, &text);
    });
    ui.data_mut(|d| d.insert_temp(id, (hue, looped, last)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_colours_follow_menu() {
        let n = row_look(false, false, true);
        assert_eq!(
            (n.fill, n.text, n.shortcut),
            (None, color32(color::TEXT_1), color32(color::TEXT_3))
        );
        let h = row_look(true, false, true);
        assert_eq!(
            (h.fill, h.text, h.shortcut),
            (
                Some(color32(color::ACCENT)),
                color32(color::ACCENT_INK),
                color32(color::ACCENT_INK)
            )
        );
        let s = row_look(false, true, true);
        assert_eq!(s.fill, Some(color32(color::SURFACE_4)));
        let d = row_look(false, false, false);
        assert_eq!(
            (d.text, d.shortcut),
            (color32(color::TEXT_4), color32(color::TEXT_4))
        );
    }

    #[test]
    fn swatches_are_the_track_bases() {
        let s = track_swatches();
        for (i, p) in komponist_tokens::track::PRECOMPUTED.iter().enumerate() {
            let c = s[i].color;
            for (a, b) in [c.r(), c.g(), c.b()].into_iter().zip(p.base) {
                assert!(a.abs_diff(b) <= 1, "{} {i}", s[i].name);
            }
        }
        assert_eq!(grid_width(), 164.0, "the design's colour box content width");
    }

    #[test]
    fn demo_menu() {
        let s = track_swatches();
        let e = demo_entries(&s, Some(5), true);
        assert_eq!(e.iter().filter(|e| e.is_separator()).count(), 3);
        assert!(!e[2].enabled);
        assert_eq!(e[9].checked, Some(true));
        let m = ContextMenu::new("Clip", &e);
        assert_eq!(m.sub_action(6, 3), Some(DemoAction::Color(3)));
        assert_eq!(m.sub_action(4, 0), None);
    }
}
