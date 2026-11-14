//! The browser (WP-44): the left panel with search, segments, the plugin / preset / file list and
//! the footer (`Screens.dc.html` `br()`, `B()`, `browserAll`, `browserSearch`, `segs`; `ks-view.js`
//! `browserView`; `Prototyp.dc.html` `brActivate`, `brDrag`).
//!
//! | Input | What happens |
//! |---|---|
//! | Strg F (app shortcut) | the search field takes the keyboard focus |
//! | typing | filters at once (`SetBrowserQuery`); the first hit is marked |
//! | ↑ ↓ | marks the previous / next row (in the field, or with the browser focused) |
//! | Enter, double-click | adds the marked plugin ([`model::activate`]): instrument → new track, effect → selected track; preset → pisynth of the selected track; take → opens its clip; folder → opens / closes |
//! | Esc in the field | clears the text, a second Esc leaves the field |
//! | click | marks the row; on a folder: opens / closes it; on the star: favourite on / off |
//! | drag a plugin | an egui drag-and-drop payload of type [`PluginRef`]; the arrangement, the mixer and the device chain take it |
//!
//! The rows come from [`model::rows`]; their view state (segment, search text, marked row, closed
//! folders, favourites) lives in [`crate::ui_state::BrowserView`].

pub mod metrics;
pub mod model;

use std::sync::Arc;

use egui::{
    Area, Id, Key, Modifiers, Order, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2,
    vec2,
};
use komponist_model::PluginRef;
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Icon, color, radius};
use komponist_ui::icons::{self, Caret};
use komponist_ui::widgets::tabs::{Item, Segmented};
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, px, state, tooltip};

use self::model::{Input, Row, RowKind, names};
use crate::actions::{Action, SettingsAction, UiAction};
use crate::ui_state::BrowserSegment;
use crate::views::{View, ViewCtx, elided_galley, group_bounds};

const SEGMENTS: [BrowserSegment; 4] = [
    BrowserSegment::All,
    BrowserSegment::Instruments,
    BrowserSegment::Effects,
    BrowserSegment::Files,
];

/// The browser view. Its view state is [`crate::ui_state::BrowserView`]; here are only the text
/// being edited and bookkeeping.
#[derive(Debug, Default)]
pub struct Browser {
    /// The search field's text (the view state's query follows it through `SetBrowserQuery`).
    query: String,
    /// The default-closed folders went into the view state.
    seeded: bool,
    /// The segment of the last frame (a new segment drops the mark).
    last_segment: Option<BrowserSegment>,
    /// The mark moved by key: scroll it into view.
    scroll_to_mark: bool,
}

/// The id of the search field (focus requests, tests).
pub fn search_id() -> Id {
    Id::unique("komponist_browser_search")
}

impl Browser {
    /// A browser with an empty search.
    pub fn new() -> Browser {
        Browser::default()
    }
}

/// The rects of the panel body.
struct Layout {
    search: Rect,
    segments: Rect,
    list: Rect,
    foot: Rect,
}

impl View for Browser {
    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        if !self.seeded {
            ctx.ui.browser.collapsed.extend(model::default_collapsed());
            self.seeded = true;
        }
        let segment = ctx.ui.browser.segment;
        if self.last_segment.is_some_and(|s| s != segment) {
            ctx.ui.browser.highlighted = None;
        }
        self.last_segment = Some(segment);
        let search_focused = ui.memory(|m| m.has_focus(search_id()));
        // The field's own edits arrive in the view state before the next frame, so a difference
        // here is a change from elsewhere (cleared by the app, a test).
        if self.query != ctx.ui.browser.query {
            self.query = ctx.ui.browser.query.clone();
        }

        let foot_text = if self.query.trim().is_empty() {
            names::FOOT
        } else {
            names::FOOT_SEARCH
        };
        let layout = layout(ui, foot_text);
        let catalog = ctx.engine.host().catalog();
        let exports = model::export_names(ctx.exports);
        let rows = model::rows(&Input {
            catalog,
            favorites: &ctx.ui.browser.favorites,
            segment,
            query: &self.query,
            collapsed: &ctx.ui.browser.collapsed,
            project: ctx.project,
            exports: &exports,
        });
        self.keys(ui, ctx, &rows, search_focused);
        self.search_field(ui, ctx, layout.search);
        segments(ui, ctx, layout.segments);
        self.list(ui, ctx, layout.list, &rows);
        footer(ui, layout.foot, foot_text);
        drag_ghost(ui.ctx());
    }
}

/// Search field and segments at the top (padding 8, gap 8), the footer at the bottom, the list
/// between.
fn layout(ui: &Ui, foot_text: &str) -> Layout {
    let r = ui.max_rect();
    let pad = metrics::TOP_PAD;
    let search = Rect::from_min_size(
        pos2(r.left() + pad, r.top() + pad),
        vec2(r.width() - 2.0 * pad, metrics::SEARCH_HEIGHT),
    );
    let segments = Rect::from_min_size(
        pos2(search.left(), search.bottom() + pad),
        vec2(search.width(), komponist_tokens::size::CONTROL),
    );
    let foot_h = foot_galley(ui, foot_text, r.width()).size().y + 2.0 * metrics::FOOT_PAD_Y;
    let foot = Rect::from_min_max(pos2(r.left(), r.bottom() - foot_h), r.max);
    let list = Rect::from_min_max(
        pos2(r.left() + metrics::LIST_PAD_X, segments.bottom() + pad),
        pos2(r.right() - metrics::LIST_PAD_X, foot.top()),
    );
    Layout {
        search,
        segments,
        list,
        foot,
    }
}

impl Browser {
    /// ↑ ↓ Enter (and Esc in the field) before the text field sees them.
    fn keys(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, rows: &[Row], search_focused: bool) {
        let free = ctx.focused && ui.memory(|m| m.focused().is_none());
        if !search_focused && !free {
            return;
        }
        let (down, up, enter) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        let current = model::effective_highlight(rows, ctx.ui.browser.highlighted, &self.query);
        let dir = i32::from(down) - i32::from(up);
        if dir != 0 {
            let selectable: Vec<bool> = rows.iter().map(Row::selectable).collect();
            if let Some(next) = w::step_enabled(&selectable, current, dir) {
                ctx.ui.browser.highlighted = Some(next);
                self.scroll_to_mark = true;
            }
        }
        if enter && let Some(row) = current.and_then(|i| rows.get(i)) {
            activate(ctx, row);
        }
        if search_focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.query.is_empty() {
                ui.memory_mut(|m| m.surrender_focus(search_id()));
            } else {
                self.query.clear();
                ctx.send(Action::Ui(UiAction::SetBrowserQuery(String::new())));
            }
        }
    }

    /// The search field (`searchS`): `surface.2`, with text or focus `surface.0` and a 1 px accent
    /// edge inside; the hint on the right ("Strg F", "Esc").
    fn search_field(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, rect: Rect) {
        if ctx.requests.focus_browser_search {
            ui.memory_mut(|m| m.request_focus(search_id()));
        }
        let focused = ui.memory(|m| m.has_focus(search_id()));
        let active = focused || !self.query.is_empty();
        let painter = ui.painter();
        let fill = if active {
            color::SURFACE_0
        } else {
            color::SURFACE_2
        };
        px::fill(painter, rect, radius::M, color32(fill));
        if active {
            px::outline(painter, rect, radius::M, -1.0, color32(color::ACCENT));
        }
        let hint = if self.query.is_empty() {
            ctx.keymap
                .label_for(&Action::Ui(UiAction::FocusBrowserSearch))
                .unwrap_or_default()
        } else {
            names::SEARCH_CLEAR.to_owned()
        };
        let hint_g = w::styled_galley(ui, &hint, &typography::VALUE_S);
        let hint_x = rect.right() - metrics::SEARCH_PAD_X - hint_g.size().x;
        let pos = w::text_pos(rect, &hint_g, hint_x, ui.pixels_per_point());
        ui.painter().galley(pos, hint_g, color32(color::TEXT_4));

        let style = typography::UI;
        let font = fonts::font_id(&style);
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let left = rect.left() + metrics::SEARCH_PAD_X;
        let field = Rect::from_min_max(
            pos2(left, rect.center().y - row_h / 2.0),
            pos2(
                hint_x - metrics::SEARCH_PAD_X,
                rect.center().y + row_h / 2.0,
            ),
        );
        let edit = egui::TextEdit::singleline(&mut self.query)
            .id(search_id())
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .font(font)
            .text_color(color32(color::TEXT_1))
            .hint_text(fonts::rich_text(names::SEARCH, &style).color(color32(color::TEXT_3)))
            .desired_width(field.width())
            // ↑ ↓ and Esc belong to the browser while typing (egui would move or drop the focus).
            .event_filter(egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
                tab: false,
            });
        let response = ui.put(field, edit);
        ui.ctx()
            .accesskit_node_builder(response.id, |n| n.set_label(names::SEARCH));
        if response.changed() {
            ctx.send(Action::Ui(UiAction::SetBrowserQuery(self.query.clone())));
        }
    }

    /// The rows in a scroll area.
    fn list(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, rect: Rect, rows: &[Row]) {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .id_salt("komponist_browser_list")
                .accessibility_role(Role::List)
                .accessibility_label(names::LIST),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        group_bounds(&child, rect);
        let mark = model::effective_highlight(rows, ctx.ui.browser.highlighted, &self.query);
        let scroll_to_mark = std::mem::take(&mut self.scroll_to_mark);
        egui::ScrollArea::vertical()
            .id_salt("komponist_browser_scroll")
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                let width = ui.available_width();
                let mut section = "";
                for (i, row) in rows.iter().enumerate() {
                    if row.kind == RowKind::Section {
                        section = &row.name;
                    }
                    let id = ui
                        .scope_id()
                        .with(("komponist_browser_row", section, row.key()));
                    let marked = mark == Some(i);
                    let response = row_ui(ui, ctx, id, row, width, marked);
                    if marked && scroll_to_mark {
                        response.scroll_to_me(None);
                    }
                    if response.clicked() && row.selectable() {
                        ctx.ui.browser.highlighted = Some(i);
                    }
                }
            });
    }
}

/// The segments "Alle · Instr. · Effekte · Dateien", equally wide over the panel (`segs`).
fn segments(ui: &mut Ui, ctx: &mut ViewCtx<'_>, rect: Rect) {
    let items = names::SEGMENT_LABELS.map(Item::new);
    let mut selected = SEGMENTS
        .iter()
        .position(|s| *s == ctx.ui.browser.segment)
        .unwrap_or(0);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt("komponist_browser_segments"),
    );
    let response = child.add(Segmented::new(names::SEGMENTS, &mut selected, &items).equal_width());
    if response.changed() {
        ctx.send(Action::Ui(UiAction::SetBrowserSegment(SEGMENTS[selected])));
    }
}

/// What Enter or a double-click on `row` does; a folder opens or closes.
fn activate(ctx: &mut ViewCtx<'_>, row: &Row) {
    if let RowKind::Folder { key, .. } = &row.kind {
        toggle_folder(ctx, key);
        return;
    }
    let track = model::selected_track(ctx.project, &ctx.ui.selection.tracks);
    for action in model::activate(row, ctx.project, track) {
        ctx.send(action);
    }
}

fn toggle_folder(ctx: &mut ViewCtx<'_>, key: &str) {
    let collapsed = &mut ctx.ui.browser.collapsed;
    if !collapsed.remove(key) {
        collapsed.insert(key.to_owned());
    }
}

/// One row (`B()`): its interaction, AccessKit node and painting.
fn row_ui(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    id: Id,
    row: &Row,
    width: f32,
    marked: bool,
) -> Response {
    let height = if row.kind == RowKind::Section {
        metrics::SECTION_HEIGHT
    } else {
        metrics::ROW_HEIGHT
    };
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let sense = match row.kind {
        RowKind::Section | RowKind::Plain => Sense::hover(),
        RowKind::Plugin { .. } => Sense::click_and_drag(),
        _ => Sense::click(),
    };
    let response = ui.interact(rect, id, sense);
    let indent = f32::from(row.level) * metrics::LEVEL_INDENT;
    let x0 = rect.left() + metrics::ROW_PAD_X + indent;
    let icon_rect = Rect::from_center_size(
        pos2(x0 + metrics::ICON / 2.0, rect.center().y),
        vec2(metrics::ICON, metrics::ICON),
    );

    // Interaction.
    match &row.kind {
        RowKind::Plugin { plugin, .. } => {
            response.dnd_set_drag_payload(plugin.clone());
            if response.double_clicked() {
                activate(ctx, row);
            }
        }
        RowKind::Folder { key, .. } => {
            if response.clicked() {
                toggle_folder(ctx, key);
            }
        }
        RowKind::Preset(_) | RowKind::Take { .. } => {
            if response.double_clicked() {
                activate(ctx, row);
            }
        }
        RowKind::Section | RowKind::Plain => {}
    }
    let st = state::State::of(ui, &response).with_selected(marked);
    accessibility(&response, row, marked);

    // The star sits on top of the row.
    let mut star_hovered = false;
    if let RowKind::Plugin {
        plugin, favorite, ..
    } = &row.kind
    {
        let hit = Rect::from_x_y_ranges(
            (icon_rect.left() - metrics::TAG_PAD_X)..=(icon_rect.right() + metrics::TAG_PAD_X),
            rect.y_range(),
        );
        let star = ui.interact(hit, id.with("star"), Sense::click());
        star_hovered = star.hovered();
        let name = names::star(&row.name);
        let fav = *favorite;
        star.widget_info(|| WidgetInfo::selected(Role::Button, true, fav, &name));
        if star.clicked() {
            ctx.send(Action::Settings(SettingsAction::ToggleFavorite(
                plugin.clone(),
            )));
        }
    }

    if ui.is_rect_visible(rect) {
        paint_row(
            ui,
            row,
            rect,
            x0,
            icon_rect,
            st,
            star_hovered || response.hovered(),
        );
    }
    response
}

/// Role, name and states of a row: list items (with "expanded" for folders), headings for
/// sections, labels for plain text.
fn accessibility(response: &Response, row: &Row, marked: bool) {
    let name = row.name.clone();
    match &row.kind {
        RowKind::Section => {
            response.widget_info(|| WidgetInfo::labeled(Role::Heading, true, &name));
        }
        RowKind::Plain => {
            response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
        }
        kind => {
            response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, marked, &name));
            let expanded = match kind {
                RowKind::Folder { expanded, .. } => Some(*expanded),
                _ => None,
            };
            let tag = row.tag.clone();
            w::accesskit(response, |n| {
                n.set_selected(marked);
                if let Some(e) = expanded {
                    n.set_expanded(e);
                }
                if !tag.is_empty() {
                    n.set_description(tag);
                }
            });
        }
    }
}

/// Paints a row: background (hover `surface.3`, marked `accent.surface`), icon, name, tag.
fn paint_row(
    ui: &Ui,
    row: &Row,
    rect: Rect,
    x0: f32,
    icon_rect: Rect,
    st: state::State,
    hovered: bool,
) {
    let ppp = ui.pixels_per_point();
    let painter = ui.painter();
    if row.selectable() {
        let fill = if st.selected {
            Some(color::ACCENT_SURFACE)
        } else if st.pressed {
            Some(color::SURFACE_4)
        } else if st.hovered {
            Some(color::SURFACE_3)
        } else {
            None
        };
        if let Some(fill) = fill {
            px::fill(painter, rect, radius::S, color32(fill));
        }
        if st.focused {
            focus::paint_ring(painter, rect, radius::S);
        }
    }
    if row.kind == RowKind::Section {
        let g = w::styled_galley(ui, &row.name, &typography::LABEL);
        let y = rect.bottom() - metrics::SECTION_PAD_BOTTOM - g.size().y;
        let pos = pos2(px::snap(x0, ppp), px::snap(y, ppp));
        painter.galley(pos, g, color32(color::TEXT_3));
        return;
    }

    // Icon column (not for plain text, as the prototype's `plain()`).
    let has_icon = row.kind != RowKind::Plain;
    match &row.kind {
        RowKind::Plugin { favorite, .. } => {
            if *favorite {
                icons::paint(ui, icon_rect, Icon::StarFilled, color32(color::STATE_SOLO));
            } else if hovered || st.selected {
                icons::paint(ui, icon_rect, Icon::Star, color32(color::TEXT_4));
            }
        }
        RowKind::Folder { expanded, .. } => {
            let [cw, ch] = wm::CARET;
            let caret = if *expanded { Caret::Down } else { Caret::Right };
            let size = if *expanded {
                vec2(cw, ch)
            } else {
                vec2(ch, cw)
            };
            let r = px::snap_rect(Rect::from_center_size(icon_rect.center(), size), ppp);
            icons::paint_caret(painter, r, caret, color32(color::TEXT_3));
        }
        RowKind::Preset(_) => {
            let g = w::galley(
                ui,
                "·",
                w::font_at(&typography::LABEL, 400),
                typography::LABEL.line_height,
            );
            let pos = w::text_pos_centered(icon_rect, &g, ppp);
            painter.galley(pos, g, color32(color::TEXT_3));
        }
        _ => {}
    }

    // Tag on the right.
    let mut right = rect.right() - metrics::ROW_PAD_X;
    if !row.tag.is_empty() {
        let g = w::styled_galley(ui, &row.tag, &typography::VALUE_S);
        let boxed = matches!(row.kind, RowKind::Plugin { .. } | RowKind::Folder { .. });
        let pad = if boxed { metrics::TAG_PAD_X } else { 0.0 };
        let tag = Rect::from_min_size(
            pos2(
                right - g.size().x - 2.0 * pad,
                rect.center().y - g.size().y / 2.0 - metrics::TAG_PAD_Y,
            ),
            vec2(
                g.size().x + 2.0 * pad,
                g.size().y + 2.0 * metrics::TAG_PAD_Y,
            ),
        );
        if boxed {
            px::fill(
                painter,
                px::snap_rect(tag, ppp),
                radius::S,
                color32(color::SURFACE_2),
            );
        }
        let pos = w::text_pos_centered(tag, &g, ppp);
        painter.galley(pos, g, color32(color::TEXT_3));
        right = tag.left() - metrics::ROW_GAP;
    }

    // Name.
    let name_x = if has_icon {
        x0 + metrics::ICON + metrics::ROW_GAP
    } else {
        x0
    };
    let style = typography::UI;
    let g = elided_galley(
        ui,
        &row.name,
        fonts::font_id(&style),
        style.line_height,
        right - name_x,
    );
    let text = match row.kind {
        RowKind::Preset(_) | RowKind::Plain => color::TEXT_2,
        _ => color::TEXT_1,
    };
    painter.galley(w::text_pos(rect, &g, name_x, ppp), g, color32(text));
}

fn foot_galley(ui: &Ui, text: &str, width: f32) -> Arc<egui::Galley> {
    let style = typography::SMALL;
    let mut job = fonts::layout_job(text, &style, color32(color::TEXT_3));
    job.wrap.max_width = (width - 2.0 * metrics::FOOT_PAD_X).max(0.0);
    ui.painter().layout_job(job)
}

/// The footer (`br().foot`): a 1 px `surface.0` line on top, `type.small` in `text.3`.
fn footer(ui: &mut Ui, rect: Rect, text: &str) {
    let g = foot_galley(ui, text, rect.width());
    let painter = ui.painter();
    px::hline(
        painter,
        rect.x_range(),
        rect.top(),
        color32(color::SURFACE_0),
    );
    let ppp = ui.pixels_per_point();
    let pos = pos2(
        px::snap(rect.left() + metrics::FOOT_PAD_X, ppp),
        px::snap(rect.top() + metrics::FOOT_PAD_Y, ppp),
    );
    let r = ui.allocate_rect(Rect::from_min_size(pos, g.size()), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    ui.painter().galley(pos, g, color32(color::TEXT_3));
}

/// While a plugin is dragged: its name at the pointer, in a tooltip frame.
fn drag_ghost(ctx: &egui::Context) {
    let Some(plugin) = egui::DragAndDrop::payload::<PluginRef>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos() else {
        return;
    };
    let offset = vec2(metrics::GHOST_OFFSET, metrics::GHOST_OFFSET);
    Area::new(Id::unique("komponist_browser_drag_ghost"))
        .order(Order::Tooltip)
        .fixed_pos(pos + offset)
        .interactable(false)
        .show(ctx, |ui| {
            tooltip::frame().show(ui, |ui| {
                ui.label(
                    fonts::rich_text(&plugin.name, &tooltip::TEXT).color(color32(color::TEXT_1)),
                );
            });
        });
}
