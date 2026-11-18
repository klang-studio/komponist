//! Groups and buses in the arrangement (WP-103; routing proposal 05-arrangement, 09-anlegen, owner
//! decision 5 A): a group is a folder row with its members indented below it and a 3 px bracket in
//! its colour; effect buses stand in a „Busse“ section below „Neue Spur“. Neither holds clips: a
//! group's lane shows the silhouette of its members' clips, a bus's lane is hatched.
//!
//! The header context menu of tracks and buses is here too (`09-anlegen`: „In Gruppe
//! zusammenfassen“, „Send zu neuem Bus“, „Send an …“, „Ausgang“; „Gruppe auflösen“ and the
//! output list on a bus).

use egui::{Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_model::automation::{AutomationMode, AutomationOwner};
use komponist_model::edits::TrackProp;
use komponist_model::{Hue, Project, Track, TrackId, TrackOutput, routing};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, space, typography};
use komponist_ui::State;
use komponist_ui::timeline::{Access, header_layout_with_reserve};
use komponist_ui::widgets::context_menu::{Colors, ContextMenu, Entry};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, accesskit};
use komponist_ui::{focus, fonts, icons, px, state};

use komponist_i18n::Lang;

use super::cache::Palettes;
use super::geometry::{Geometry, RowKind};
use super::{Arrangement, Rename, automation, metrics};
use crate::actions::{Action, EditAction};
use crate::views::routing as rt;
use crate::views::{ViewCtx, elided_galley};
use komponist_ui::widgets::rename_field::RenameState;

/// Where the parts of a bus header lie.
struct BusLayout {
    /// The fold triangle's hit area (groups).
    fold: Option<Rect>,
    /// The folder icon (groups).
    folder: Option<Rect>,
    /// The name: one `type.track` line.
    name: Rect,
    /// The line below the name („2 Spuren“; groups).
    sub: Option<Rect>,
}

fn bus_layout(rect: Rect, ppp: f32, kind: RowKind) -> BusLayout {
    let l = header_layout_with_reserve(rect, automation::HEADER_RESERVE);
    let group = matches!(kind, RowKind::Group { .. });
    let mut x = l.color_bar.max.x + space::SPACE_1;
    let cy = l.body.center().y;
    let (mut fold, mut folder) = (None, None);
    if group {
        let f = Rect::from_center_size(
            pos2(x + metrics::FOLD_HIT / 2.0, cy),
            vec2(metrics::FOLD_HIT, metrics::FOLD_HIT),
        );
        x = f.right() + space::SPACE_0_5;
        let i = Rect::from_center_size(
            pos2(x + metrics::FOLDER_ICON / 2.0, cy),
            vec2(metrics::FOLDER_ICON, metrics::FOLDER_ICON),
        );
        x = i.right() + space::SPACE_1_5;
        fold = Some(f);
        folder = Some(i);
    }
    // Effect buses place the automation icon directly before M and S.
    let right = if group {
        l.toggles[0].left() - space::SPACE_1
    } else {
        l.toggles[2].left() - automation::HEADER_RESERVE
    };
    let name_h = typography::TRACK.line_height;
    let sub_h = typography::SMALL.line_height;
    let block = if group {
        name_h + komponist_ui::widgets::metrics::timeline::HEADER_TEXT_GAP + sub_h
    } else {
        name_h
    };
    let top = px::snap(cy - block / 2.0, ppp);
    let name = Rect::from_min_max(pos2(x, top), pos2(right.max(x), top + name_h));
    let sub = group.then(|| {
        let y = top + name_h + komponist_ui::widgets::metrics::timeline::HEADER_TEXT_GAP;
        Rect::from_min_max(pos2(x, y), pos2(right.max(x), y + sub_h))
    });
    BusLayout {
        fold,
        folder,
        name,
        sub,
    }
}

/// The rename field's place in a bus header.
pub(super) fn bus_name_rect(rect: Rect, ppp: f32, kind: RowKind) -> Rect {
    bus_layout(rect, ppp, kind).name
}

/// The 3 px bracket in the colour of each group a member row is inside, one per level, in the
/// header column's indent.
pub(super) fn paint_brackets(
    painter: &egui::Painter,
    project: &Project,
    g: &Geometry,
    i: usize,
    palettes: &mut Palettes,
) {
    let row = g.rows[i];
    let groups = routing::group_ancestors(project, row.track);
    // Outermost first: its bracket stands leftmost.
    for (level, id) in groups.iter().rev().enumerate() {
        let Some(group) = project.track(*id) else {
            continue;
        };
        let x = g.header_x.min + level as f32 * metrics::MEMBER_INDENT;
        let r = Rect::from_min_max(pos2(x, row.y.min), pos2(x + metrics::BRACKET, row.y.max));
        px::fill(painter, r, 0.0, color32(palettes.get(group.hue.0).base));
    }
}

impl Arrangement {
    /// Paints a group's or effect bus's header at `rect` and handles its fold triangle: `surface.1`
    /// ground, the colour bar, then (group) triangle, folder, name and „n Spuren“, or (effect bus)
    /// the name alone. M, S and the meter are placed by the caller.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn bus_header(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        track: &Track,
        kind: RowKind,
        rect: Rect,
        state: State,
        implied_mute: bool,
        response: &Response,
    ) {
        let ppp = ui.pixels_per_point();
        let l = header_layout_with_reserve(rect, automation::HEADER_RESERVE);
        let lay = bus_layout(rect, ppp, kind);
        let dim = track.channel.mute || implied_mute;
        let palette = self.palettes.get(track.hue.0);
        let painter = ui.painter();
        px::fill(painter, l.body, 0.0, state::row_fill(1, state));
        px::fill(painter, l.separator, 0.0, color32(color::SURFACE_0));
        if state.focused {
            px::inner_border(painter, l.body, color32(color::ACCENT));
        }
        px::fill(
            painter,
            l.color_bar,
            0.0,
            color32(if dim { palette.dim } else { palette.base }),
        );
        let ink = if dim { color::TEXT_3 } else { color::TEXT_1 };
        let name = elided_galley(
            ui,
            &track.name,
            fonts::font_id(&typography::TRACK),
            typography::TRACK.line_height,
            lay.name.width(),
        );
        ui.painter().galley(lay.name.min, name, color32(ink));

        let folded = matches!(kind, RowKind::Group { folded: true });
        if let RowKind::Group { .. } = kind {
            if let Some(sub) = lay.sub {
                let text = ctx
                    .lang
                    .routing_tracks(routing::group_members(project, track.id).len());
                let g = elided_galley(
                    ui,
                    &text,
                    fonts::font_id(&typography::SMALL),
                    typography::SMALL.line_height,
                    sub.width(),
                );
                ui.painter().galley(sub.min, g, color32(color::TEXT_3));
            }
            if let Some(folder) = lay.folder {
                icons::paint_in(
                    ui,
                    folder,
                    if folded {
                        komponist_tokens::Icon::Folder
                    } else {
                        komponist_tokens::Icon::FolderOpen
                    },
                    color32(color::TEXT_2),
                );
            }
            if let Some(fold) = lay.fold {
                self.fold_button(ui, ctx, project, track, fold, folded);
            }
        }
        let access = match kind {
            RowKind::Group { folded } => Access::selectable(
                Role::Row,
                ctx.lang
                    .routing_group_name(&track.name, if folded { "folded" } else { "unfolded" }),
                state.selected,
            ),
            _ => Access::selectable(
                Role::Row,
                ctx.lang.routing_effect_bus_name(&track.name),
                state.selected,
            ),
        };
        super::register(response, &access);
    }

    /// The fold triangle: a click folds or unfolds the group, Alt+click all groups.
    fn fold_button(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        track: &Track,
        rect: Rect,
        folded: bool,
    ) {
        let r = ui.interact(
            rect,
            ui.scope_id()
                .with(("komponist_arrangement_fold", track.id.0)),
            Sense::click(),
        );
        let t = ctx.lang;
        let label =
            t.arrangement_group_fold(&track.name, if folded { "expand" } else { "collapse" });
        r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
        accesskit(&r, |n| n.set_expanded(!folded));
        let st = State::of(ui, &r);
        let ink = if st.hovered {
            color::TEXT_1
        } else {
            color::TEXT_2
        };
        let c = rect.center();
        let (w, h) = (6.0, 4.0);
        icons::paint_caret(
            ui.painter(),
            Rect::from_center_size(c, if folded { vec2(h, w) } else { vec2(w, h) }),
            if folded {
                icons::Caret::Right
            } else {
                icons::Caret::Down
            },
            color32(ink),
        );
        if focus::ring_visible(&r) {
            focus::paint_ring(ui.painter(), rect, radius::S);
        }
        Tip::new(if folded {
            t.arrangement_group_expand()
        } else {
            t.arrangement_group_collapse()
        })
        .attach(&r);
        if r.clicked() {
            let alt = ui.input(|i| i.modifiers.alt);
            let tracks: Vec<TrackId> = if alt {
                project
                    .tracks
                    .iter()
                    .filter(|t| t.is_group())
                    .map(|t| t.id)
                    .collect()
            } else {
                vec![track.id]
            };
            ctx.send(Action::Edit(EditAction::SetTrackProp {
                tracks,
                prop: TrackProp::Folded(!folded),
            }));
        }
    }

    /// The label row of the „Busse“ section below „Neue Spur“.
    pub(super) fn section_label(&mut self, ui: &mut Ui, lang: Lang, g: &Geometry) {
        let Some(area) = g.section else {
            return;
        };
        if !area.intersects(g.tracks) {
            return;
        }
        let left = Rect::from_x_y_ranges(g.header_x, area.y_range());
        px::fill(ui.painter(), left, 0.0, color32(color::SURFACE_1));
        let style = typography::LABEL;
        let gal = w::styled_galley(ui, lang.routing_buses(), &style);
        let pos = w::text_pos(
            left,
            &gal,
            left.left() + metrics::NEW_TRACK_PADDING[0],
            ui.pixels_per_point(),
        );
        ui.painter().galley(pos, gal, color32(color::TEXT_3));
        let r = ui.interact(
            left,
            ui.scope_id().with("komponist_arrangement_section"),
            Sense::hover(),
        );
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, lang.routing_buses()));
    }

    /// The header's context menu; returns what was chosen.
    pub(super) fn header_menu(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        track: &Track,
        response: &Response,
    ) -> Option<MenuAction> {
        #[derive(Clone, Copy)]
        enum Pick {
            Rename,
            Flatten,
            Record,
            Color(usize),
            Group,
            SendNew,
            SendTo(TrackId),
            Output(TrackOutput),
            NewBusOutput,
            AutomationShow,
            AutomationAdd,
            AutomationMode(AutomationMode),
            Ungroup,
            Delete,
        }
        let selected = &ctx.ui.selection.tracks;
        let tracks: Vec<TrackId> = if selected.contains(&track.id) {
            project
                .tracks
                .iter()
                .map(|t| t.id)
                .filter(|t| selected.contains(t))
                .collect()
        } else {
            vec![track.id]
        };
        let swatches = crate::views::hue_swatches(ctx.lang);
        let hue = Hue::ALL.iter().position(|h| *h == track.hue);
        let shortcut = ctx
            .keymap
            .label_for(&Action::Edit(EditAction::GroupSelection), ctx.lang)
            .unwrap_or_default();
        let delete_key = ctx
            .keymap
            .label_for(&Action::Edit(EditAction::DeleteSelection), ctx.lang);
        // Output rows (a bus can go to a group or the master as well).
        let outputs = rt::output_targets(ctx.lang, project, track.id);
        let output_entries: Vec<Entry<'_, Pick>> = outputs
            .iter()
            .map(|t| {
                let e = Entry::action(&t.label, Pick::Output(t.output()))
                    .checked(t.output() == track.output);
                if t.blocked.is_some() { e.disabled() } else { e }
            })
            .collect();
        let sends = rt::send_targets(ctx.lang, project, track.id);
        let mut send_entries: Vec<Entry<'_, Pick>> = sends
            .iter()
            .filter_map(|t| {
                let e = Entry::action(&t.label, Pick::SendTo(t.id?));
                Some(if t.blocked.is_some() { e.disabled() } else { e })
            })
            .collect();
        if !sends.is_empty() {
            send_entries.push(Entry::separator());
        }
        let t = ctx.lang;
        send_entries.push(Entry::action(t.routing_new_bus(), Pick::SendNew));
        let mut entries: Vec<Entry<'_, Pick>> = vec![
            Entry::action(t.routing_rename(), Pick::Rename),
            Entry::colors(
                t.routing_color(),
                Colors {
                    swatches: &swatches,
                    selected: hue,
                    pick: Pick::Color,
                },
            ),
            Entry::separator(),
        ];
        if track.is_bus() {
            entries.push(Entry::action(t.routing_record_bus(), Pick::Record));
            entries.push(Entry::submenu(t.routing_output_to(), &output_entries));
            entries.push(Entry::separator());
            if track.folder {
                entries.push(Entry::action(t.routing_ungroup(), Pick::Ungroup));
            } else {
                entries.push(Entry::action(t.routing_delete_bus(), Pick::Delete));
            }
        } else {
            entries.push(Entry::action(t.routing_group_tracks(), Pick::Group).shortcut(&shortcut));
            entries.push(Entry::action(t.routing_send_to_new_bus(), Pick::SendNew));
            entries.push(Entry::submenu(t.routing_send_to(), &send_entries));
            entries.push(Entry::submenu(t.routing_output_to(), &output_entries));
            if !track.is_audio() {
                let flatten = Entry::action(t.arrangement_flatten_track(), Pick::Flatten);
                entries.push(if track.clips.is_empty() {
                    flatten.disabled()
                } else {
                    flatten
                });
            }
            entries.push(Entry::separator());
            let delete = Entry::action(t.arrangement_delete(), Pick::Delete);
            entries.push(match &delete_key {
                Some(k) => delete.shortcut(k),
                None => delete,
            });
        }
        let modes = crate::views::automation::mode_names(t)
            .map(|(mode, label)| Entry::action(label, Pick::AutomationMode(mode)));
        entries.push(Entry::separator());
        entries.push(Entry::action(
            if track.automation.shown {
                t.automation_hide_menu()
            } else {
                t.automation_show_menu()
            },
            if track.automation.lanes.is_empty() {
                Pick::AutomationAdd
            } else {
                Pick::AutomationShow
            },
        ));
        entries.push(Entry::action(
            t.automation_add_lane_menu(),
            Pick::AutomationAdd,
        ));
        let bulk = Entry::submenu(t.automation_mode_all(), &modes);
        entries.push(if track.automation.lanes.is_empty() {
            bulk.disabled()
        } else {
            bulk
        });
        let _ = Pick::NewBusOutput;
        let name = if track.is_bus() {
            t.routing_kind_bus()
        } else {
            t.arrangement_track_menu()
        };
        let picked = ContextMenu::new(name, &entries).show_on(response)?;
        Some(match picked {
            Pick::Rename => MenuAction::Rename,
            Pick::Flatten => MenuAction::Edit(Action::Edit(EditAction::FlattenMidiTrack(track.id))),
            Pick::Record => MenuAction::Edit(Action::Edit(EditAction::RecordBus(track.id))),
            Pick::Color(i) => MenuAction::Edit(Action::Edit(EditAction::SetTrackProp {
                tracks,
                prop: TrackProp::Hue(Hue::ALL[i]),
            })),
            Pick::Group => MenuAction::Edit(Action::Edit(EditAction::GroupTracks(tracks))),
            Pick::SendNew => MenuAction::Edit(Action::Edit(EditAction::SendToNewBus(tracks))),
            Pick::SendTo(target) => MenuAction::Edit(Action::Edit(EditAction::AddSend {
                track: track.id,
                target,
            })),
            Pick::Output(output) => {
                MenuAction::Edit(Action::Edit(EditAction::SetOutput { tracks, output }))
            }
            Pick::NewBusOutput => MenuAction::Rename,
            Pick::AutomationShow => {
                MenuAction::Edit(Action::Automation(crate::views::automation::Action::Shown(
                    AutomationOwner::Track(track.id),
                    !track.automation.shown,
                )))
            }
            Pick::AutomationAdd => MenuAction::AutomationAdd,
            Pick::AutomationMode(mode) => MenuAction::Edit(Action::Automation(
                crate::views::automation::Action::AllModes(AutomationOwner::Track(track.id), mode),
            )),
            Pick::Ungroup | Pick::Delete => {
                MenuAction::Edit(Action::Edit(EditAction::RemoveTracks(tracks)))
            }
        })
    }
}

/// What a header context menu asks for.
pub(super) enum MenuAction {
    /// Edit the name in place.
    Rename,
    /// Open the parameter chooser of this track.
    AutomationAdd,
    /// Send an action.
    Edit(Action),
}

impl MenuAction {
    /// Carries the choice out.
    pub(super) fn apply(self, arrangement: &mut Arrangement, ctx: &mut ViewCtx<'_>, track: &Track) {
        match self {
            MenuAction::Rename => {
                arrangement.rename = Some(Rename {
                    track: track.id,
                    edit: RenameState::new(&track.name),
                });
            }
            MenuAction::AutomationAdd => arrangement
                .automation
                .request(AutomationOwner::Track(track.id)),
            MenuAction::Edit(action) => ctx.send(action),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Lanes
// ------------------------------------------------------------------------------------------------

/// A group's lane: the silhouette of the clips of its members (all levels), display only. An
/// effect bus's lane is hatched and says what it is.
pub(super) fn paint_bus_lane(ui: &Ui, lang: Lang, project: &Project, g: &Geometry, i: usize) {
    let row = g.rows[i];
    let lane = g.lane(i).intersect(g.tracks);
    if !lane.is_positive() {
        return;
    }
    let painter = ui.painter().with_clip_rect(lane.intersect(ui.clip_rect()));
    let Some(bus) = project.track(row.track) else {
        return;
    };
    match row.kind {
        RowKind::Group { .. } => {
            let (t0, t1) = g.visible_ticks();
            let inset = metrics::SILHOUETTE_INSET;
            let area = g.lane(i);
            let body = Rect::from_x_y_ranges(
                area.x_range(),
                area.top() + inset..=area.bottom() - inset - px::line_width(ui.pixels_per_point()),
            );
            let mut label = None;
            for member in project.tracks.iter().filter(|t| {
                !t.is_bus() && routing::group_ancestors(project, t.id).contains(&bus.id)
            }) {
                for clip in member
                    .clips
                    .iter()
                    .filter(|c| c.end() >= t0 && c.start <= t1)
                {
                    let r = Rect::from_x_y_ranges(
                        g.axis.x(clip.start)..=g.axis.x(clip.end()),
                        body.y_range(),
                    )
                    .shrink2(vec2(0.5, 0.0));
                    px::fill(&painter, r, radius::S, color32(color::SURFACE_3));
                    if label.is_none() && r.width() > 120.0 {
                        label = Some(r);
                    }
                }
            }
            // The label goes over the silhouette, after all of it is painted.
            if let Some(r) = label {
                let g2 = w::galley(
                    ui,
                    lang.routing_group_lane(),
                    fonts::font_id(&typography::SMALL),
                    typography::SMALL.line_height,
                );
                let pos = w::text_pos(
                    r,
                    &g2,
                    r.left().max(lane.left()) + space::SPACE_1_5,
                    ui.pixels_per_point(),
                );
                painter.galley(pos, g2, color32(color::TEXT_3));
            }
        }
        RowKind::EffectBus => {
            hatch(&painter, g.lane(i));
            let sends = rt::input_sends(project, bus.id);
            let text = lang.routing_bus_lane(sends);
            let gal = w::galley(
                ui,
                &text,
                fonts::font_id(&typography::SMALL),
                typography::SMALL.line_height,
            );
            let pos = w::text_pos(
                g.lane(i),
                &gal,
                g.lane(i).left().max(lane.left()) + space::SPACE_2,
                ui.pixels_per_point(),
            );
            painter.galley(pos, gal, color32(color::TEXT_3));
        }
        RowKind::Track | RowKind::Hidden => {}
    }
}

/// Diagonal hatching over a lane (`05-arrangement`: „schraffierte Lane“): 1 px lines in `line.0`.
fn hatch(painter: &egui::Painter, rect: Rect) {
    let step = space::SPACE_2;
    let stroke = egui::Stroke::new(1.0, color32(color::LINE_0));
    let h = rect.height();
    let mut x = rect.left() - h;
    while x < rect.right() {
        painter.line_segment([pos2(x, rect.bottom()), pos2(x + h, rect.top())], stroke);
        x += step;
    }
}
