//! The inspector (WP-43): the right panel (S01 inspector; `Screens.dc.html` `sec`, `F`,
//! `trackSec`, `swatchesFor`, `D` and the inspector markup; `ks-view.js` `inspView`).
//!
//! Sections top to bottom, each a group with a title (`type.label`), an optional meta text or
//! action on the right, and a 1 px `surface.0` line below ([`model::Sections`] says which show):
//!
//! | Section | Content | Changes |
//! |---|---|---|
//! | Clip ([`clip`]) | name (double-click or F2 renames), Start, Länge, Loop an/aus, Loop-Länge, Transponieren, Velocity; the takes after loop recording; step clips: Pattern, Auflösung, Swing; the take being recorded ("nimmt auf") | `SetClipProp`, `SetClipLength`, `SetClipLoop`, `SetStepClipProp`, `SelectTake` |
//! | Spur ([`track`]) | name, colour swatches, Instrument, Eingang, Kanal | `SetTrackProp` (Name, Hue, Input, MidiChannel), `Plugin(Add)` replaces the instrument |
//! | Geräte ([`track`]) | the chain with power square, status and format tag; "+ Effekt" | `OpenWindow`, `SetDeviceBypass`, browser on Effekte |
//! | Parameter ([`param`]) | the device parameter touched last: name, device · band/op · track, Wert, Standard, Bereich | – |
//! | Kanal ([`channel`]) | compact strip: fader, meter, level, pan, M S; "im Mixer zeigen" | as the mixer strip |
//! | Routing ([`routing`], WP-103) | with a bus in the project: Ausgang, the sends (level, pre / post, ×, "+"), "Sidechain-Quelle für:"; a bus lists its inputs and shows "Bus" instead of "Spur" | `SetOutput`, `AddSend`, `SetSend`, `RemoveSend` |
//! | Projekt, Auswahl ([`project`]) | with nothing selected: Tempo, Taktart, Spuren, Länge | `SetProjectProp(Tempo)` |
//!
//! Number fields drag (1 px = one step), Shift fine, Alt-click default, double-click text entry;
//! a drag is one undo step. The panel scrolls with the wheel when the sections are taller (not over
//! the channel fader or pan knob: they take the wheel, owner decision 2026-11-15).

mod audio;
mod channel;
mod clip;
pub mod metrics;
pub mod model;
mod param;
mod project;
mod routing;
mod track;

use std::sync::Arc;

use egui::{
    Align, Layout, Rangef, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_i18n::Lang;
use komponist_model::DeviceId;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Srgba, color, radius, size, typography};
use komponist_ui::state::State;
use komponist_ui::widgets::number_field;
use komponist_ui::widgets::rename_field::{RenameField, RenameOutcome, RenameState};
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px};

use self::metrics as m;
use self::model::{ClipSection, Sections};
use crate::ui_state::InspectorRename;
use crate::views::late::LateTracker;
use crate::views::{View, ViewCtx, elided_galley, group_bounds};

/// What is being renamed in the inspector.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RenameTarget {
    /// A clip's name.
    Clip(komponist_model::ClipId),
    /// A track's name.
    Track(komponist_model::TrackId),
}

/// A name being edited (double-click on a section's name).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rename {
    target: RenameTarget,
    edit: RenameState,
}

/// The inspector view.
#[derive(Debug, Default)]
pub struct Inspector {
    /// Vertical scroll in design px.
    scroll: f32,
    /// Height of the sections in the last frame.
    content_height: f32,
    rename: Option<Rename>,
    /// Per device: the late-block count last seen and when it last grew (egui time).
    late: LateTracker,
    audio_source:crate::views::audio_editor::source::Source,
}

impl Inspector {
    /// The view.
    pub fn new() -> Inspector {
        Inspector::default()
    }
}

impl View for Inspector {
    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        let project = Arc::clone(ctx.project);
        let body = ui.max_rect();
        let recording: Vec<komponist_model::TrackId> = ctx
            .recording
            .map(|r| r.tracks().collect())
            .unwrap_or_default();
        let sections = Sections::of(
            &project,
            &ctx.ui.selection.tracks,
            &ctx.ui.selection.clips,
            &recording,
            &ctx.ui.layout,
        );
        if self
            .rename
            .as_ref()
            .is_some_and(|r| !self.renaming_shown(r, &sections))
        {
            self.rename = None;
        }
        // F2: the name of the clip or track goes into the edit mode.
        if let Some(request) = ctx.ui.inspector.rename_request.take() {
            match request {
                InspectorRename::Clip(c) if sections.clip == Some(ClipSection::Clip(c)) => {
                    if let Some((_, clip)) = project.find_clip(c) {
                        self.start_rename(RenameTarget::Clip(c), &clip.name);
                    }
                }
                InspectorRename::Track(t) if sections.track == Some(t) => {
                    if let Some(track) = project.track(t) {
                        self.start_rename(RenameTarget::Track(t), &track.name);
                    }
                }
                _ => {}
            }
        }

        let max_scroll = (self.content_height - body.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        if max_scroll > 0.0 && ui.rect_contains_pointer(body) {
            let d = ui.input(|i| i.smooth_scroll_delta.y);
            if d != 0.0 {
                self.scroll = (self.scroll - d).clamp(0.0, max_scroll);
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
        }
        let x = body.x_range();
        let top = body.top() - self.scroll;
        let mut y = top;
        match sections.clip {
            Some(ClipSection::Clip(id)) => {
                clip::section(self, ui, ctx, &project, x, &mut y, id);
                audio::file_section(ui,ctx,&project,x,&mut y,id,&mut self.audio_source);
            },
            Some(ClipSection::Recording(track)) => {
                clip::recording_section(ui, ctx, &project, x, &mut y, track);
            }
            None => {}
        }
        if let Some(track) = sections.track.and_then(|t| project.track(t)) {
            track::section(self, ui, ctx, &project, x, &mut y, track);
            track::devices_section(self, ui, ctx, x, &mut y, track);
            if let Some(touched) = param::touched(ctx, track.id) {
                param::section(ui, ctx.lang, x, &mut y, &touched);
            }
            if sections.channel {
                channel::section(ui, ctx, x, &mut y, track);
            }
            routing::section(ui, ctx, &project, x, &mut y, track);
        }
        if sections.is_empty() {
            project::section(ui, ctx, &project, x, &mut y);
            project::selection_section(ui, ctx.lang, x, &mut y);
        }
        self.content_height = y - top;
        let live: Vec<DeviceId> = project
            .tracks
            .iter()
            .flat_map(|t| t.channel.devices.iter().map(|d| d.id))
            .collect();
        self.late.retain(&live);
    }
}

impl Inspector {
    /// Whether the name being renamed is still on screen.
    fn renaming_shown(&self, rename: &Rename, sections: &Sections) -> bool {
        match rename.target {
            RenameTarget::Clip(c) => sections.clip == Some(ClipSection::Clip(c)),
            RenameTarget::Track(t) => sections.track == Some(t),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Layout

/// The content column of a section and the y below its last part.
#[derive(Debug, Clone, Copy)]
struct Col {
    left: f32,
    right: f32,
    y: f32,
    first: bool,
}

impl Col {
    /// The next part, `height` high, [`m::GAP`] below the previous one.
    fn take(&mut self, height: f32) -> Rect {
        if !self.first {
            self.y += m::GAP;
        }
        self.first = false;
        let r = Rect::from_x_y_ranges(self.left..=self.right, self.y..=self.y + height);
        self.y += height;
        r
    }

    fn width(&self) -> f32 {
        self.right - self.left
    }
}

/// The meta text of a section title row.
#[derive(Debug, Clone, Copy)]
enum Meta<'a> {
    /// Nothing.
    None,
    /// Information in `text.3` ("MIDI", "3 / 6").
    Text(&'a str),
    /// An action in `text.2` ("+ Effekt", "im Mixer zeigen").
    Action(&'a str),
    /// The recording state in `state.record` ("nimmt auf").
    Record(&'a str),
}

/// One section at `*y` across `x`: a group named `title` with its title row, `content` below,
/// the padding and the 1 px `surface.0` line; moves `*y` below it. Returns what `content`
/// returned and the response of a meta action.
fn frame<R>(
    ui: &mut Ui,
    x: Rangef,
    y: &mut f32,
    title: &str,
    meta: Meta<'_>,
    content: impl FnOnce(&mut Ui, &mut Col) -> R,
) -> (R, Option<Response>) {
    let top = *y;
    let clip = ui.clip_rect();
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_x_y_ranges(
                x,
                top..=top + clip.height().max(1.0) * 4.0,
            ))
            .id_salt(("komponist_inspector", title))
            .accessibility_role(Role::Group)
            .accessibility_label(title),
    );
    child.set_clip_rect(clip);
    let mut col = Col {
        left: x.min + m::PAD_X,
        right: x.max - m::PAD_X,
        y: top + m::PAD_Y,
        first: true,
    };
    let row = col.take(typography::LABEL.line_height);
    let ppp = child.pixels_per_point();
    let g = w::styled_galley(&child, title, &typography::LABEL);
    let pos = w::text_pos(row, &g, row.left(), ppp);
    child
        .painter()
        .galley(pos, g, fonts::style_color(&typography::LABEL));
    let meta_response = meta_text(&mut child, row, meta);
    let out = content(&mut child, &mut col);
    let bottom = col.y + m::PAD_Y;
    let lw = px::line_width(ppp);
    px::hline(child.painter(), x, bottom, color32(color::SURFACE_0));
    let rect = Rect::from_x_y_ranges(x, top..=bottom + lw);
    group_bounds(&child, rect);
    *y = bottom + lw;
    (out, meta_response)
}

/// The meta text right in the title row; an action is a link.
fn meta_text(ui: &mut Ui, row: Rect, meta: Meta<'_>) -> Option<Response> {
    let (text, ink, action) = match meta {
        Meta::None => return None,
        Meta::Text(t) => (t, color::TEXT_3, false),
        Meta::Action(t) => (t, color::TEXT_2, true),
        Meta::Record(t) => (t, color::STATE_RECORD, false),
    };
    let style = typography::VALUE_S;
    let g = w::galley(ui, text, fonts::font_id(&style), style.line_height);
    let ppp = ui.pixels_per_point();
    let pos = w::text_pos(row, &g, row.right() - g.size().x, ppp);
    let r = Rect::from_min_size(pos, g.size());
    let sense = if action {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(r, ui.scope_id().with(("meta", text)), sense);
    let st = State::of(ui, &response);
    let ink = if action {
        komponist_ui::state::text_for(ink, st)
    } else {
        color32(ink)
    };
    ui.painter().galley(pos, g, ink);
    if action && st.focused {
        komponist_ui::focus::paint_ring(ui.painter(), r, radius::S);
    }
    let role = if action { Role::Link } else { Role::Label };
    response.widget_info(|| WidgetInfo::labeled(role, true, text));
    if action && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    action.then_some(response)
}

/// Runs `add` in a child of `ui` filling `rect`, laid out left to right from its top-left corner
/// (vertically centred), with a stable id.
fn place<R>(
    ui: &mut Ui,
    rect: Rect,
    salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(salt)
            .layout(Layout::left_to_right(Align::Center)),
    );
    add(&mut child)
}

/// Paints `text` in `style` (at `weight` if given) left in `rect`, cut with "…".
fn label(
    ui: &Ui,
    rect: Rect,
    text: &str,
    style: &komponist_tokens::typography::TypeStyle,
    weight: Option<u16>,
    ink: Srgba,
) -> Rect {
    let font = match weight {
        Some(wt) => w::font_at(style, wt),
        None => fonts::font_id(style),
    };
    let g = elided_galley(ui, text, font, style.line_height, rect.width());
    let pos = w::text_pos(rect, &g, rect.left(), ui.pixels_per_point());
    let r = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color32(ink));
    r
}

/// An AccessKit label over `rect`.
fn access_label(ui: &mut Ui, rect: Rect, salt: impl std::hash::Hash + std::fmt::Debug, text: &str) {
    let r = ui.interact(rect, ui.scope_id().with(salt), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
}

/// The cells of a two-column field grid with `n` fields: each its key rect and its field rect
/// (`F`: key `type.small` `text.3`, 4 px, field 24 high; columns and rows 8 apart).
fn field_grid(col: &mut Col, n: usize) -> Vec<(Rect, Rect)> {
    let rows = n.div_ceil(2);
    let h = rows as f32 * m::FIELD_HEIGHT + rows.saturating_sub(1) as f32 * m::FIELD_GAP;
    let area = col.take(h);
    let cw = (area.width() - m::FIELD_GAP) / 2.0;
    (0..n)
        .map(|i| {
            let (c, r) = (i % 2, i / 2);
            let x = area.left() + c as f32 * (cw + m::FIELD_GAP);
            let y = area.top() + r as f32 * (m::FIELD_HEIGHT + m::FIELD_GAP);
            let key = Rect::from_min_size(pos2(x, y), vec2(cw, typography::SMALL.line_height));
            let field = Rect::from_min_size(
                pos2(x, key.bottom() + m::FIELD_KEY_GAP),
                vec2(cw, size::CONTROL),
            );
            (key, field)
        })
        .collect()
}

/// Paints a field's key.
fn field_key(ui: &Ui, key: Rect, text: &str) {
    label(ui, key, text, &typography::SMALL, None, color::TEXT_3);
}

/// A field that only shows a value (recording: "Noten 14"; project: "Taktart 4/4"): the number
/// field's look at rest, an AccessKit label "Noten 14".
fn read_only_field(ui: &mut Ui, rect: Rect, key: &str, value: &str, unit: &str) {
    let look = number_field::look(State::REST, false);
    number_field::paint(ui.painter(), rect, &look, value, unit, 0.0);
    let text = if unit.is_empty() {
        format!("{key} {value}")
    } else {
        format!("{key} {value} {unit}")
    };
    access_label(ui, rect, ("field", key), &text);
}

/// An on/off field ("Loop an"): `accent.surface` when on (`F(…, hl)`), else the number field's
/// look; a click switches it unless `fixed`. AccessKit: a check box named `key`.
fn switch_field(ui: &mut Ui, t: Lang, rect: Rect, key: &str, on: bool, fixed: bool) -> bool {
    let sense = if fixed {
        Sense::hover()
    } else {
        Sense::click()
    };
    let response = ui.interact(rect, ui.scope_id().with(("switch", key)), sense);
    let st = State::of(ui, &response).with_disabled(false);
    let mut look = number_field::look(if fixed { State::REST } else { st }, false);
    if on {
        look.fill = color32(color::ACCENT_SURFACE);
    }
    let text = if on {
        t.inspector_value_on()
    } else {
        t.inspector_value_off()
    };
    number_field::paint(ui.painter(), rect, &look, text, "", 0.0);
    let enabled = !fixed;
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, enabled, on, key));
    if !fixed && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    !fixed && response.clicked()
}

/// A name row: chip and the name in `type.track`; a double-click asks to rename (the caller
/// opens the text field). Returns the name's rect and whether it was double-clicked.
fn name_row(
    ui: &mut Ui,
    col: &mut Col,
    name: &str,
    chip: Srgba,
    bar: bool,
    renamable: bool,
) -> (Rect, bool) {
    let style = typography::TRACK;
    let row = col.take(style.line_height);
    let (cw, ch) = if bar {
        (m::CHIP_BAR[0], m::CHIP_BAR[1])
    } else {
        (m::CHIP, m::CHIP)
    };
    let chip_rect = Rect::from_min_size(pos2(row.left(), row.center().y - ch / 2.0), vec2(cw, ch));
    let r = if bar { 0.0 } else { radius::S };
    px::fill(ui.painter(), chip_rect, r, color32(chip));
    let text_rect =
        Rect::from_x_y_ranges(chip_rect.right() + m::NAME_GAP..=row.right(), row.y_range());
    label(ui, text_rect, name, &style, None, color::TEXT_1);
    let sense = if renamable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(text_rect, ui.scope_id().with("name"), sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
    (text_rect, renamable && response.double_clicked())
}

impl Inspector {
    /// The text field over a name being renamed: `surface.0` with an accent ring; Enter or a
    /// click elsewhere applies (`commit` gets the trimmed new name), Esc discards.
    fn rename_field(
        &mut self,
        ui: &mut Ui,
        target: &RenameTarget,
        current: &str,
        rect: Rect,
        commit: impl FnOnce(String),
    ) {
        let Some(rename) = self.rename.as_mut().filter(|r| &r.target == target) else {
            return;
        };
        let id = ui.scope_id().with(("rename", format!("{target:?}")));
        let font = fonts::font_id(&typography::TRACK);
        let label = komponist_ui::lang(ui.ctx()).inspector_rename();
        match RenameField::new(&mut rename.edit, id, label, current, font).show(ui, rect) {
            RenameOutcome::Editing => {}
            RenameOutcome::Commit(name) => {
                commit(name);
                self.rename = None;
            }
            RenameOutcome::Cancel => self.rename = None,
        }
    }

    /// Starts renaming `target` (its current name preselected).
    fn start_rename(&mut self, target: RenameTarget, current: &str) {
        self.rename = Some(Rename {
            target,
            edit: RenameState::new(current),
        });
    }

    /// Whether `target` is being renamed.
    fn renaming(&self, target: &RenameTarget) -> bool {
        self.rename.as_ref().is_some_and(|r| &r.target == target)
    }
}

/// The weight of semibold texts (instrument rows).
const SEMIBOLD: u16 = wm::SEMIBOLD;
