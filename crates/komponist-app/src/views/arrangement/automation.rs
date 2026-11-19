//! Track and master automation rows, with the same editing canvas as the clip editors.

use egui::{
    Align, Color32, CursorIcon, Id, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2,
    vec2,
};
use komponist_model::Project;
use komponist_model::automation::{
    self, AutomationMode, AutomationOwner, LANE_HEIGHT_DEFAULT, LANE_HEIGHT_MAX, LANE_HEIGHT_MIN,
    Lane,
};
use komponist_model::edits::ChannelProp;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, line, space, typography};
use komponist_ui::timeline::{header_layout, paint_playhead};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::meter::Meter;
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::{fonts, px, widgets};

use super::geometry::{AutomationRow, Geometry};
use super::metrics;
use crate::actions::{Action, EditAction};
use crate::meters::Meters;
use crate::views::ViewCtx;
use crate::views::automation::{
    Action as Automation, CHIP_HEIGHT, Catalog, Choice, Chooser, Editor, LaneFrame, Override,
    Paint, Source, TOGGLE_WIDTH, chooser_button, mode_chip, mode_chip_width, mode_names, toggle,
};

/// Space for the spline toggle in the header, with its own group gap.
pub(super) const HEADER_RESERVE: f32 = TOGGLE_WIDTH + widgets::metrics::TOGGLE_GROUP_GAP;

#[derive(Debug, Default)]
pub(super) struct State {
    editor: Editor,
    chooser: Chooser,
    request: Option<AutomationOwner>,
    resize: Option<Resize>,
}

#[derive(Clone, Copy, Debug)]
struct Resize {
    source: (AutomationOwner, automation::LaneId),
    origin: f32,
    height: f32,
    last: u16,
}

#[derive(Clone, Copy)]
enum Menu {
    Hide,
    Clear,
    Remove,
    Mode(AutomationMode),
    AllModes(AutomationMode),
    Add,
}

impl State {
    pub(super) fn request(&mut self, owner: AutomationOwner) {
        self.request = Some(owner);
    }

    pub(super) fn owner_toggle(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        owner: AutomationOwner,
        right: f32,
        y: f32,
        owner_ink: Color32,
    ) -> Option<egui::Response> {
        let lanes = crate::views::automation::lanes(ctx.project, owner)?;
        let id = ui.scope_id().with(("automation-owner-toggle", owner));
        let tint = if lanes.shown {
            color32(color::TEXT_1)
        } else if lanes.lanes.is_empty() {
            color32(color::TEXT_3)
        } else {
            owner_ink
        };
        let r = toggle(
            ui,
            right - space::SPACE_0_5,
            y,
            id,
            ctx.lang,
            lanes.shown,
            tint,
        );
        if r.clicked() {
            if lanes.lanes.is_empty() {
                self.chooser.open(id);
            } else {
                ctx.send(Action::Automation(Automation::Shown(owner, !lanes.shown)));
            }
        }
        if self.request == Some(owner) {
            self.chooser.open(id);
            self.request = None;
        }
        if let Some(catalog) = Catalog::new(ctx, owner)
            && let Some(Choice::Target { key, label }) =
                self.chooser.show(ui, id, r.rect, &catalog, false)
        {
            ctx.send(Action::Automation(Automation::ShowTarget {
                owner,
                replace: None,
                target: key,
                label,
            }));
        }
        Some(r)
    }

    pub(super) fn show(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
    ) {
        self.master(ui, ctx, project, g);
        let mut mode_width = None;
        for row in g
            .automation
            .iter()
            .filter(|row| row.y.intersects(g.tracks.y_range()))
        {
            if let Some(lanes) = crate::views::automation::lanes(project, row.owner)
                && let Some(lane) = lanes.lanes.iter().find(|lane| lane.id == row.lane)
            {
                let last = lanes.lanes.last().is_some_and(|last| last.id == lane.id);
                let mode_width = *mode_width.get_or_insert_with(|| mode_chip_width(ui, ctx.lang));
                self.lane(ui, ctx, project, g, row, lane, last, mode_width);
            }
        }
        self.editor.finish_frame(ui, ctx);
        if self.resize.is_some() && !ui.input(|input| input.pointer.primary_down()) {
            self.resize = None;
            ctx.send(Action::Edit(EditAction::EndGesture));
        }
    }

    fn master(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, project: &Project, g: &Geometry) {
        if !g.master.intersects(g.tracks.y_range()) {
            return;
        }
        let lang = ctx.lang;
        let area = Rect::from_x_y_ranges(g.tracks.x_range(), g.master);
        let group = ui.interact(
            area.intersect(g.tracks),
            Id::unique("arrangement-master"),
            Sense::hover(),
        );
        group.widget_info(|| WidgetInfo::labeled(Role::Group, true, lang.a11y_automation_master()));
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(area)
                .accessibility_parent(group.id),
        );
        child.set_clip_rect(ui.clip_rect().intersect(area));
        let header = Rect::from_x_y_ranges(g.header_x, g.master);
        let layout = header_layout(header);
        let timeline = Rect::from_x_y_ranges(g.lane_x, g.master);
        let painter = child.painter();
        px::fill(painter, header, 0.0, color32(color::SURFACE_2));
        px::fill(painter, timeline, 0.0, color32(color::SURFACE_1));
        px::fill(painter, layout.color_bar, 0.0, color32(color::TEXT_3));
        painter.hline(
            area.x_range(),
            area.top(),
            egui::Stroke::new(line::WIDTH, color32(color::LINE_2)),
        );
        painter.text(
            pos2(layout.text.left(), header.center().y),
            egui::Align2::LEFT_CENTER,
            lang.automation_master(),
            fonts::font_id(&typography::TRACK),
            color32(color::TEXT_1),
        );
        let info = lang.automation_master_summary(project.master.devices.len());
        painter.text(
            timeline.left_center() + vec2(space::SPACE_2, 0.0),
            egui::Align2::LEFT_CENTER,
            info,
            fonts::font_id(&typography::SMALL),
            color32(color::TEXT_3),
        );
        if timeline.x_range().contains(g.axis.x(ctx.engine.playhead())) {
            paint_playhead(painter, g.axis.x(ctx.engine.playhead()), timeline.y_range());
        }

        let mute = layout.toggles[3];
        let mut mute_ui = child.new_child(
            UiBuilder::new()
                .id_salt("master-mute")
                .max_rect(mute)
                .layout(Layout::left_to_right(Align::Min)),
        );
        let out = TrackToggle::new(ToggleKind::Mute, project.master.mute).show(&mut mute_ui);
        if let Some(ToggleEvent::Set { value, .. }) = out.event {
            ctx.send(Action::Edit(EditAction::SetMasterProp(ChannelProp::Mute(
                value,
            ))));
        }
        let _ = self.owner_toggle(
            &mut child,
            ctx,
            AutomationOwner::Master,
            mute.left() - space::SPACE_0_5,
            header.center().y,
            color32(color::ACCENT),
        );

        let mut meter_ui = child.new_child(
            UiBuilder::new()
                .max_rect(layout.meter)
                .layout(Layout::left_to_right(Align::Min)),
        );
        meter_ui.add(
            Meter::new(
                &mut ctx.meters.master,
                Meters::NO_PEAKS,
                lang.automation_master(),
            )
            .header()
            .length(layout.meter.height()),
        );
        let menu = child.interact(
            layout.text,
            Id::unique("arrangement-master-menu"),
            Sense::click(),
        );
        let modes =
            mode_names(lang).map(|(mode, label)| Entry::action(label, Menu::AllModes(mode)));
        let entries = [
            Entry::action(lang.automation_add_lane_menu(), Menu::Add),
            Entry::submenu(lang.automation_mode_all(), &modes),
        ];
        match ContextMenu::new(lang.automation_master(), &entries).show_on(&menu) {
            Some(Menu::Add) => self.request(AutomationOwner::Master),
            Some(Menu::AllModes(mode)) => ctx.send(Action::Automation(Automation::AllModes(
                AutomationOwner::Master,
                mode,
            ))),
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lane(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
        row: &AutomationRow,
        lane: &Lane,
        last: bool,
        mode_width: f32,
    ) {
        let t = ctx.lang;
        let Some(resolved) =
            Catalog::new(ctx, row.owner).map(|catalog| catalog.resolve(&lane.target))
        else {
            return;
        };
        let (target, warning) = match resolved {
            Ok(target) => (Some(target), None),
            Err(warning) => (None, warning),
        };
        let label = target
            .as_ref()
            .map_or(lane.label.as_str(), |target| target.label.as_str());
        let track = match row.owner {
            AutomationOwner::Track(id) => project.track(id),
            AutomationOwner::Master => None,
        };
        let track_name = track.map_or(t.automation_master(), |track| track.name.as_str());
        let access = if let Some(warning) = &warning {
            t.a11y_automation_lane_missing(label, track_name, warning.trim_start_matches("⚠ "))
        } else {
            t.a11y_automation_lane(label, track_name)
        };
        let header = Rect::from_x_y_ranges(g.header_x, row.y);
        let body = Rect::from_x_y_ranges(g.lane_x, row.y);
        let id = ui.scope_id().with(("automation-row", row.owner, row.lane));
        let group = ui.interact(
            header.union(body).intersect(ui.clip_rect()),
            id,
            Sense::hover(),
        );
        group.widget_info(|| WidgetInfo::labeled(Role::Group, true, &access));
        widgets::accesskit(&group, |node| {
            if let Some(warning) = &warning {
                node.set_description(warning.as_str());
            }
        });
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(header.union(body))
                .id_salt(id)
                .accessibility_parent(group.id),
        );
        child.set_clip_rect(ui.clip_rect().intersect(header.union(body)));
        let source = Source::Lane(row.owner, row.lane);
        let palette = track.map(|track| TrackPalette::new(track.hue.0));
        let color = palette
            .as_ref()
            .map_or(color32(color::TEXT_2), |p| color32(p.base));
        let fill = palette
            .as_ref()
            .map_or(color32(color::SURFACE_3), |p| color32(p.body))
            .gamma_multiply(0.5);
        let curve = if lane.mode == AutomationMode::Off || warning.is_some() {
            color32(color::TEXT_4)
        } else {
            color
        };
        px::fill(
            child.painter(),
            header,
            0.0,
            color32(if self.editor.selected(&source) {
                color::CANVAS_LANE_SELECTED
            } else {
                color::SURFACE_2
            }),
        );
        let stripe = Rect::from_min_size(
            header.min,
            vec2(
                widgets::metrics::timeline::HEADER_COLOR_BAR,
                header.height(),
            ),
        );
        px::fill(child.painter(), stripe, 0.0, color);

        let top = if header.height() >= f32::from(LANE_HEIGHT_DEFAULT) {
            header.top() + space::SPACE_0_5
        } else {
            header.center().y - CHIP_HEIGHT / 2.0
        };
        let chip = Rect::from_min_size(
            pos2(header.right() - space::SPACE_1 - mode_width, top),
            vec2(mode_width, CHIP_HEIGHT),
        );
        let left = header.left() + f32::from(row.depth) * metrics::MEMBER_INDENT + space::SPACE_1;
        let chooser = Rect::from_min_max(
            pos2(left.min(chip.left()), top),
            pos2(chip.left() - space::SPACE_1, top + CHIP_HEIGHT),
        );
        let heading = child.interact(header, id.with("header-menu"), Sense::click());
        let choice_id = id.with("parameter");
        if chooser.is_positive()
            && chooser_button(&mut child, chooser, choice_id, t, label).clicked()
        {
            self.chooser.open(choice_id);
        }
        if let Some(mode) = mode_chip(&mut child, chip, id.with("mode"), t, label, lane.mode) {
            ctx.send(Action::Automation(Automation::Mode(
                row.owner, row.lane, mode,
            )));
        }

        let base = target.as_ref().map_or(0.0, |target| target.base);
        let head = ctx.engine.playhead();
        let effective_now =
            crate::views::automation::automation_controls(ctx, row.owner, &lane.target)
                .unwrap_or(base);
        if header.height() >= f32::from(LANE_HEIGHT_DEFAULT) {
            let value = target.as_ref().map_or_else(
                || crate::views::automation::percentage(t, effective_now),
                |target| target.format(effective_now),
            );
            let detail = warning.as_deref().unwrap_or(&value);
            let style = typography::SMALL;
            let text = crate::views::elided_galley(
                &child,
                detail,
                fonts::font_id(&style),
                style.line_height,
                header.right() - left - space::SPACE_1,
            );
            child.painter().galley(
                pos2(left, top + CHIP_HEIGHT),
                text,
                color32(if warning.is_some() {
                    color::STATE_ERROR
                } else {
                    color::TEXT_2
                }),
            );
        }

        if let Some(action) = self.lane_menu(t, row, lane, &heading) {
            ctx.send(Action::Automation(action));
        }
        let Some(catalog) = Catalog::new(ctx, row.owner) else {
            return;
        };
        match self
            .chooser
            .show(&child, choice_id, chooser, &catalog, last)
        {
            Some(Choice::Target { key, label }) => {
                ctx.send(Action::Automation(Automation::ShowTarget {
                    owner: row.owner,
                    replace: lane.points.is_empty().then_some(lane.id),
                    target: key,
                    label,
                }))
            }
            Some(Choice::Add) => self.request(row.owner),
            None => {}
        }
        let overrides = track
            .filter(|_| lane.mode != AutomationMode::Off)
            .map_or_else(Vec::new, |track| {
                automation::clip_override_spans(track, &lane.target)
                    .into_iter()
                    .filter_map(|span| {
                        let (left, right) = g.visible_ticks();
                        (span.end > left && span.start < right).then_some(Override {
                            start: span.start,
                            end: span.end,
                        })
                    })
                    .collect()
            });
        let effective = track
            .filter(|_| !overrides.is_empty())
            .map_or_else(Vec::new, |track| {
                if let Some(curve) = automation::effective_repeating_points(track, lane, base) {
                    let (left, right) = g.visible_ticks();
                    curve.visible_points(left, right, (body.width().ceil() as usize).clamp(2, 4096))
                } else {
                    automation::effective_points(track, &lane.target, base)
                }
            });
        let frame = LaneFrame {
            source,
            label,
            access: &access,
            warning: warning.as_deref(),
            points: lane.points.as_slice(),
            target: target.as_ref(),
            rect: body,
            axis: g.axis,
            grid: ctx.ui.arrangement.grid,
            snap: ctx.ui.arrangement.snap,
            end: None,
            head,
        };
        self.editor.show(
            &mut child,
            ctx,
            &frame,
            &Paint {
                curve,
                fill,
                effective: &effective,
                overrides: &overrides,
                beyond: None,
            },
        );
        self.height(ui, ctx, header, row, lane);
        ui.painter().hline(
            header.union(body).x_range(),
            body.bottom() - line::WIDTH / 2.0,
            egui::Stroke::new(line::WIDTH, color32(color::SURFACE_0)),
        );
    }

    fn lane_menu(
        &self,
        t: komponist_i18n::Lang,
        row: &AutomationRow,
        lane: &Lane,
        response: &egui::Response,
    ) -> Option<Automation> {
        let modes = mode_names(t)
            .map(|(mode, label)| Entry::action(label, Menu::Mode(mode)).checked(lane.mode == mode));
        let all = mode_names(t).map(|(mode, label)| Entry::action(label, Menu::AllModes(mode)));
        let entries = [
            Entry::action(t.automation_hide_lane(), Menu::Hide),
            Entry::action(t.automation_clear_lane(), Menu::Clear),
            Entry::action(t.automation_delete_lane(), Menu::Remove),
            Entry::submenu(t.automation_mode(), &modes),
            Entry::submenu(t.automation_mode_all(), &all),
        ];
        Some(
            match ContextMenu::new(t.automation_title(), &entries).show_on(response)? {
                Menu::Hide => Automation::Shown(row.owner, false),
                Menu::Clear => Automation::Clear(row.owner, row.lane),
                Menu::Remove => Automation::Remove(Source::Lane(row.owner, row.lane)),
                Menu::Mode(mode) => Automation::Mode(row.owner, row.lane, mode),
                Menu::AllModes(mode) => Automation::AllModes(row.owner, mode),
                Menu::Add => return None,
            },
        )
    }

    fn height(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        header: Rect,
        row: &AutomationRow,
        lane: &Lane,
    ) {
        let key = (row.owner, row.lane);
        let grip = Rect::from_x_y_ranges(
            header.x_range(),
            header.bottom() - metrics::HEIGHT_GRAB..=header.bottom() + metrics::HEIGHT_GRAB,
        );
        let r = ui.interact(
            grip,
            ui.scope_id().with(("automation-height", key)),
            Sense::drag(),
        );
        let label = ctx.lang.arrangement_height_grip(&lane.label);
        r.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::Splitter, true, &label);
            info.value = Some(f64::from(lane.height));
            info
        });
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if r.drag_started()
            && let Some(origin) = ui.input(|input| input.pointer.press_origin())
        {
            self.resize = Some(Resize {
                source: key,
                origin: origin.y,
                height: f32::from(lane.height),
                last: lane.height,
            });
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: "undo-automation-lane-height".to_owned(),
            }));
        }
        if (r.dragged() || r.drag_stopped())
            && let Some(resize) = self.resize.as_mut().filter(|resize| resize.source == key)
            && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
        {
            let height = (resize.height + pointer.y - resize.origin)
                .round()
                .clamp(f32::from(LANE_HEIGHT_MIN), f32::from(LANE_HEIGHT_MAX))
                as u16;
            if resize.last != height {
                resize.last = height;
                ctx.send(Action::Automation(Automation::Height(
                    row.owner, row.lane, height,
                )));
            }
        }
    }
}
