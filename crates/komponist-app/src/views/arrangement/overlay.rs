//! What lies around and over the tracks: the "Neue Spur" area below them, the S10 hint of an
//! empty project, the insert line of a track drag and the drop target of a browser drag.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, Id, Painter, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_i18n::Lang;
use komponist_model::{Project, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, space, typography};
use komponist_ui::state::State;
use komponist_ui::widgets::button::{self, Appearance, Kind};
use komponist_ui::widgets::metrics as wm;
use komponist_ui::{focus, fonts, px, widgets as w};

use super::geometry::Geometry;
use super::{Arrangement, Drag, metrics};
use crate::actions::{Action, DropTarget, EditAction, ProjectAction, UiAction};
use crate::views::ViewCtx;

impl Arrangement {
    /// "Neue Spur" with its hint in the header column below the last track (S01, S10), a button:
    /// click, Enter or Space open the dialog for new tracks. At rest it has the device chain's drop
    /// slot look (dashed `surface.4` frame, `text.3`); hovered or pressed it gets the brighter
    /// one (`surface.2` fill, dashed `surface.5` frame, `text.2`), focused it gets the ring. A plugin dropped on it is handled by `drop_target`.
    pub(super) fn new_track_area(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, g: &Geometry) {
        let area = g.new_track();
        let left = Rect::from_x_y_ranges(g.header_x, area.y_range());
        if left.height() <= 0.0 {
            return;
        }
        let ppp = ui.pixels_per_point();
        let [pad_x, pad_y] = metrics::NEW_TRACK_PADDING;
        let shortcut = ctx
            .keymap
            .label_for(&Action::Edit(EditAction::NewMidiTrack), ctx.lang)
            .unwrap_or_default();
        let t = ctx.lang;
        let audio_shortcut = ctx
            .keymap
            .label_for(&Action::Edit(EditAction::NewAudioTrack), ctx.lang)
            .unwrap_or_default();
        let hint = t.arrangement_new_track_hint_audio(&shortcut, &audio_shortcut);
        let hint = hint.trim_end();

        let inset = metrics::NEW_TRACK_BOX_INSET;
        let height = (left.height() - 2.0 * inset).min(metrics::NEW_TRACK_MIN_HEIGHT);
        if height <= 0.0 {
            return;
        }
        let boxed = Rect::from_min_size(
            left.min + vec2(inset, inset),
            vec2(left.width() - 2.0 * inset, height),
        );
        let id = ui.scope_id().with("komponist_arrangement_new_track");
        let response = ui.interact(boxed, id, Sense::click());
        response.widget_info(|| {
            WidgetInfo::labeled(Role::Button, true, t.arrangement_new_track_button())
        });
        w::accesskit(&response, |n| n.set_description(hint));
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let hot = response.hovered() || response.is_pointer_button_down_on();
        let ink = if hot { color::TEXT_2 } else { color::TEXT_3 };
        let edge = if hot {
            px::fill(ui.painter(), boxed, 0.0, color32(color::SURFACE_2));
            color::SURFACE_5
        } else {
            color::SURFACE_4
        };
        px::dashed_border(ui.painter(), boxed, color32(edge), metrics::NEW_TRACK_DASH);
        if focus::ring_visible(&response) {
            focus::paint_ring(ui.painter(), boxed, 0.0);
        }

        let x = px::snap(left.left() + pad_x, ppp);
        let mut y = px::snap(left.top() + pad_y, ppp);
        let title_ink = if hot { color::TEXT_1 } else { color::TEXT_2 };
        for (text, style, c) in [
            (t.arrangement_new_track(), typography::UI, title_ink),
            (hint, typography::SMALL, ink),
        ] {
            let mut job = fonts::layout_job(text, &style, color32(c));
            job.wrap.max_width = (left.width() - 2.0 * pad_x).max(0.0);
            let galley = ui.painter().layout_job(job);
            let r = Rect::from_min_size(pos2(x, y), galley.size());
            y = px::snap(r.bottom() + metrics::NEW_TRACK_GAP, ppp);
            ui.painter()
                .with_clip_rect(boxed.intersect(ui.clip_rect()))
                .galley(r.min, galley, color32(c));
        }
        if response.clicked() {
            ctx.send(Action::Ui(UiAction::OpenNewTrackDialog));
        }
    }

    /// The 2 px insert line while headers are dragged.
    pub(super) fn overlays(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
    ) {
        match &self.drag {
            Some(Drag::Reorder {
                slot: Some(slot), ..
            }) => insert_line(ui.painter(), g, g.slot_y(*slot)),
            Some(Drag::Reorder {
                tracks,
                onto: Some(bus),
                ..
            }) => self.paint_output_drop(ui, ctx.lang, project, g, tracks, *bus),
            _ => {}
        }
    }

    /// Headers dragged onto a group or effect bus: the accent frame around its header (the look
    /// of a plugin drop target), or, when the output would close a loop, no frame and the reason
    /// at the pointer.
    fn paint_output_drop(
        &self,
        ui: &Ui,
        lang: Lang,
        project: &Project,
        g: &Geometry,
        tracks: &[TrackId],
        bus: TrackId,
    ) {
        let Some(i) = g.rows.iter().position(|r| r.track == bus) else {
            return;
        };
        match crate::views::routing::output_drop(lang, project, tracks, bus) {
            crate::views::routing::OutputDrop::Set => {
                px::inner_border(ui.painter(), g.header(i), color32(color::ACCENT));
            }
            crate::views::routing::OutputDrop::Unchanged => {}
            crate::views::routing::OutputDrop::Refused(reason) => {
                if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                    let r = Rect::from_x_y_ranges(egui::Rangef::point(p.x), g.rows[i].y);
                    komponist_ui::tooltip::show_value(
                        ui.ctx(),
                        Id::unique("komponist_arrangement_output_refused"),
                        r,
                        komponist_ui::tooltip::Placement::Above,
                        &reason,
                    );
                }
            }
        }
    }

    /// S10: "Leeres Projekt" with its text and the actions "MIDI-Spur hinzufügen" (primary, with
    /// its shortcut) and "Projekt öffnen", centred over the lanes.
    pub(super) fn empty_hint(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, g: &Geometry) {
        let area = Rect::from_x_y_ranges(g.lane_x, g.tracks.y_range());
        let ppp = ui.pixels_per_point();
        let width = metrics::EMPTY_HINT_WIDTH.min(area.width() - 2.0 * space::SPACE_4);
        if width <= 0.0 {
            return;
        }
        let t = ctx.lang;
        let title = w::styled_galley(ui, t.arrangement_empty_title(), &typography::TITLE);
        let mut job = LayoutJob::single_section(
            t.arrangement_empty_text().to_owned(),
            TextFormat {
                font_id: fonts::font_id(&typography::BODY),
                line_height: Some(typography::BODY.line_height),
                color: color32(color::TEXT_2),
                ..Default::default()
            },
        );
        job.wrap = TextWrapping {
            max_width: width,
            ..Default::default()
        };
        let text = ui.painter().layout_job(job);
        let gap = metrics::EMPTY_HINT_GAP;
        let total = title.size().y
            + gap
            + text.size().y
            + gap
            + metrics::EMPTY_BUTTONS_TOP
            + metrics::EMPTY_BUTTON_HEIGHT;
        let x = px::snap(area.center().x - width / 2.0, ppp);
        let mut y = px::snap(area.center().y - total / 2.0, ppp);

        let title_rect = Rect::from_min_size(pos2(x, y), title.size());
        ui.painter()
            .galley(title_rect.min, title, color32(color::TEXT_1));
        label_node(ui, title_rect, t.arrangement_empty_title());
        y = px::snap(title_rect.bottom() + gap, ppp);
        let text_rect = Rect::from_min_size(pos2(x, y), text.size());
        ui.painter()
            .galley(text_rect.min, text, Color32::PLACEHOLDER);
        label_node(ui, text_rect, t.arrangement_empty_text());
        y = px::snap(text_rect.bottom() + gap + metrics::EMPTY_BUTTONS_TOP, ppp);

        let shortcut = ctx
            .keymap
            .label_for(&Action::Edit(EditAction::NewMidiTrack), ctx.lang);
        let add = hint_button(
            ui,
            pos2(x, y),
            t.arrangement_add_midi_track(),
            shortcut.as_deref(),
            Kind::Primary,
        );
        if add.clicked() {
            ctx.send(Action::Edit(EditAction::NewMidiTrack));
        }
        let open = hint_button(
            ui,
            pos2(add.rect.right() + metrics::EMPTY_BUTTON_GAP, y),
            t.arrangement_open_project(),
            None,
            Kind::Secondary,
        );
        if open.clicked() {
            ctx.send(Action::Project(ProjectAction::Open));
        }
    }
}

/// A label node for painted text.
fn label_node(ui: &Ui, rect: Rect, text: &str) {
    let response = ui.interact(
        rect,
        Id::unique(("komponist_arrangement_label", text)),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
}

/// A button of the S10 hint (`height:28px; padding:0 12px; font-size:12px`, primary semibold
/// with the shortcut in Mono 10 medium after a 10 px gap; secondary medium), in the colours of
/// the design system's button (`button::look`).
fn hint_button(
    ui: &mut Ui,
    at: egui::Pos2,
    label: &str,
    shortcut: Option<&str>,
    kind: Kind,
) -> egui::Response {
    let ppp = ui.pixels_per_point();
    let style = typography::UI;
    let font = match kind {
        Kind::Primary => w::font_at(&style, wm::SEMIBOLD),
        Kind::Secondary => fonts::emphasis_font_id(&style),
    };
    let text = w::galley(ui, label, font, style.line_height);
    let sc = shortcut.map(|s| {
        w::galley(
            ui,
            s,
            w::font_at(&typography::VALUE_S, wm::MEDIUM),
            typography::VALUE_S.line_height,
        )
    });
    let inner = text.size().x.ceil()
        + sc.as_ref()
            .map_or(0.0, |g| metrics::EMPTY_SHORTCUT_GAP + g.size().x.ceil());
    let rect = Rect::from_min_size(
        at,
        vec2(
            inner + 2.0 * metrics::EMPTY_BUTTON_PADDING_X,
            metrics::EMPTY_BUTTON_HEIGHT,
        ),
    );
    let response = ui.interact(
        rect,
        Id::unique(("komponist_arrangement_hint", label)),
        Sense::click(),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let st = State::of(ui, &response);
    let look = button::look(
        Appearance {
            kind,
            ..Appearance::default()
        },
        st,
    );
    let painter = ui.painter();
    if let Some(fill) = look.fill {
        px::fill(painter, rect, radius::M, fill);
    }
    let x = rect.left() + metrics::EMPTY_BUTTON_PADDING_X;
    let pos = w::text_pos(rect, &text, x, ppp);
    let text_right = pos.x + text.size().x;
    painter.galley(pos, text, look.text);
    if let Some(sc) = sc {
        let pos = w::text_pos(rect, &sc, text_right + metrics::EMPTY_SHORTCUT_GAP, ppp);
        painter.galley(pos, sc, look.text);
    }
    if look.focus_ring {
        focus::paint_ring(painter, rect, radius::M);
    }
    response
}

/// A 2 px accent line across headers and lanes, centred on `y` (`ix.header` "Einfügelinie").
fn insert_line(painter: &Painter, g: &Geometry, y: f32) {
    let h = metrics::INSERT_LINE;
    let r = Rect::from_min_max(
        pos2(g.body.left(), y - h / 2.0),
        pos2(g.body.right(), y + h / 2.0),
    );
    px::fill(painter, r, 0.0, color32(color::ACCENT));
}

/// Shows where a dragged plugin would go: an accent frame around the track's header and lane,
/// or the insert line below the last track for a new track.
pub(super) fn paint_drop_target(ui: &Ui, g: &Geometry, target: &DropTarget) {
    let painter = ui.painter();
    match target {
        DropTarget::Track(id) => {
            if let Some(row) = g.rows.iter().find(|r| r.track == *id) {
                let r = Rect::from_x_y_ranges(g.tracks.x_range(), row.y);
                px::inner_border(painter, r, color32(color::ACCENT));
            }
        }
        DropTarget::NewTrack { .. } => insert_line(painter, g, g.rows_end),
        DropTarget::Chain { .. } => {}
    }
}
