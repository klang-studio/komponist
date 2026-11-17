//! Draws layout 2a: transport bar, browser, arrangement, dock, inspector, status bar, the
//! collapsed rails, the resize edges, the focus areas with their accent line; or the start screen
//! (S09). The views fill the panels ([`crate::views`]); the dialogs come from
//! [`crate::dialogs`] (the export dialog through [`crate::export`]).

use std::time::Instant;

use egui::{
    Align, CursorIcon, Id, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, size, space, typography};
use komponist_ui::icons::{self, Caret};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::panel_header::PanelHeader;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, px};

use crate::actions::{Action, UiAction};
use crate::app::App;
use crate::layout::{self, Edge, Side};
use crate::metrics::layout::RAIL_CARET_TOP;
use crate::panels::{dock, statusbar, transport};
use crate::ui_state::{DockTab, FocusArea, Panel, Screen};
use crate::views::{View, ViewCtx, centered_text};

/// Which view fills the dock body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockBody {
    PianoRoll,
    Waveform,
    StepSeq,
    NoClip,
    Devices,
    NoTrack,
    Mixer,
}

impl App {
    /// Draws the whole window (module docs) and queues what the user does.
    pub(crate) fn draw_shell(&mut self, ui: &mut Ui) {
        let screen = ui.max_rect();
        px::fill(ui.painter(), screen, 0.0, color32(color::SURFACE_0));
        if self.ui.screen == Screen::Start {
            self.draw_start(ui, screen);
            self.draw_dialogs(&ui.ctx().clone());
            return;
        }
        let l = layout::compute(screen, &self.ui.layout);
        self.focus_on_press(ui, &l);

        let plugin_windows = self.plugin_window_entries();
        let model = transport::TransportModel {
            project: self.doc.project(),
            dirty: self.doc.is_dirty(),
            state: self.engine.transport(),
            playhead: self.engine.playhead(),
            cpu: self.engine.stats().cpu_load(),
            audio: self.engine.audio_state(),
            keymap: &self.keymap,
            lang: self.lang,
            master_display: self.settings.master_display,
            plugin_windows: &plugin_windows,
        };
        transport::show(ui, l.transport, &model, &mut self.meters, &mut self.queue);

        match l.browser {
            Side::Open(r) => self.side_panel(ui, r, Panel::Browser),
            Side::Rail(r) => self.rail(ui, r, Panel::Browser),
        }
        match l.inspector {
            Side::Open(r) => self.side_panel(ui, r, Panel::Inspector),
            Side::Rail(r) => self.rail(ui, r, Panel::Inspector),
        }
        if let Some(r) = l.arrangement {
            self.arrangement(ui, r);
        }
        self.dock(ui, l.dock);

        let status = statusbar::model(self, Instant::now());
        if statusbar::show(ui, l.status, &status)
            && let Some((_, action)) = self.ui.status.as_ref().and_then(|s| s.link.clone())
        {
            self.dispatch(*action);
        }

        for edge in &l.edges {
            self.edge(ui, edge, screen);
        }
        self.draw_export(&ui.ctx().clone());
        self.draw_dialogs(&ui.ctx().clone());
    }

    /// A press inside a panel focuses its area (not while a dialog is open).
    fn focus_on_press(&mut self, ui: &Ui, l: &layout::Layout) {
        if self.ui.dialog.is_some() || self.dialogs.overlay.is_some() {
            return;
        }
        let press = ui.input(|i| {
            i.pointer
                .any_pressed()
                .then(|| i.pointer.press_origin())
                .flatten()
        });
        let Some(pos) = press else {
            return;
        };
        let area = if l.transport.contains(pos) {
            Some(FocusArea::Transport)
        } else if l.browser.is_open() && l.browser.rect().contains(pos) {
            Some(FocusArea::Browser)
        } else if l.inspector.is_open() && l.inspector.rect().contains(pos) {
            Some(FocusArea::Inspector)
        } else if l.arrangement.is_some_and(|r| r.contains(pos)) {
            Some(FocusArea::Arrangement)
        } else if l.dock.contains(pos) && self.ui.layout.dock_open {
            Some(self.dock_area())
        } else {
            None
        };
        if let Some(area) = area
            && area != self.ui.focus
        {
            self.dispatch(Action::Ui(UiAction::Focus(area)));
        }
    }

    /// Runs `f` with a [`ViewCtx`] for the view of `area`.
    fn with_view<R>(
        &mut self,
        area: FocusArea,
        f: impl FnOnce(&mut crate::views::Views, &mut ViewCtx<'_>) -> R,
    ) -> R {
        let focused = self.ui.focus == area;
        let project_dir = self.project_dir();
        let mut ctx = ViewCtx::new(
            self.doc.project(),
            &mut self.ui,
            &self.engine,
            &mut self.meters,
            &self.keymap,
            &mut self.queue,
        );
        ctx.project_dir = project_dir.as_deref();
        ctx.focused = focused;
        ctx.lang = self.lang;
        ctx.requests = self.shell.requests;
        ctx.recording = self.recording.as_ref();
        ctx.exports = &self.export.done;
        let midi = self.engine.telemetry().held_input_keys();
        ctx.held_notes = self
            .shell
            .held_notes
            .iter()
            .map(|(_, note)| *note)
            .filter(|note| midi & 1 << note == 0)
            .chain((0..128u8).filter(|key| midi & 1 << key != 0))
            .collect();
        f(&mut self.views, &mut ctx)
    }

    fn side_panel(&mut self, ui: &mut Ui, rect: Rect, panel: Panel) {
        let t = self.lang;
        let (title, area) = match panel {
            Panel::Browser => (t.shell_browser(), FocusArea::Browser),
            _ => (t.shell_inspector(), FocusArea::Inspector),
        };
        let toggle = Action::Ui(UiAction::TogglePanel(panel));
        let shortcut = self
            .keymap
            .label_for(&toggle, self.lang)
            .unwrap_or_default();
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(title));
        child.spacing_mut().item_spacing = vec2(0.0, 0.0);
        let mut open = true;
        let focused = self.ui.focus == area;
        let header = self.with_view(area, |views, ctx| {
            PanelHeader::new(title, &mut open)
                .focused(focused)
                .shortcut(&shortcut)
                .show(&mut child, |ui| {
                    let view: &mut dyn View = match panel {
                        Panel::Browser => &mut *views.browser,
                        _ => &mut *views.inspector,
                    };
                    view.header(ui, ctx);
                })
        });
        if header.changed() {
            self.queue.push(toggle);
        }
        let body = Rect::from_min_max(pos2(rect.left(), header.rect.bottom()), rect.max);
        px::fill(ui.painter(), body, 0.0, color32(color::SURFACE_1));
        let mut body_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(body)
                .id_salt((title, "body"))
                .layout(Layout::top_down(Align::Min)),
        );
        body_ui.set_clip_rect(body);
        self.with_view(area, |views, ctx| match panel {
            Panel::Browser => views.browser.show(&mut body_ui, ctx),
            _ => views.inspector.show(&mut body_ui, ctx),
        });
    }

    /// A collapsed side panel: a 24 px rail with ▸ (browser) or ◂ (inspector); a click opens it.
    fn rail(&mut self, ui: &mut Ui, rect: Rect, panel: Panel) {
        let name = match panel {
            Panel::Browser => self.lang.shell_browser(),
            _ => self.lang.shell_inspector(),
        };
        let id = Id::unique(("komponist_rail", name));
        let response = ui.interact(rect, id, Sense::click());
        let st = State::of(ui, &response);
        let painter = ui.painter();
        px::fill(painter, rect, 0.0, state::surface_for(2, st));
        let ink = state::text_for(color::TEXT_2, st);
        let [cw, ch] = wm::CARET;
        let caret = Rect::from_min_size(
            pos2(rect.center().x - ch / 2.0, rect.top() + RAIL_CARET_TOP),
            vec2(ch, cw),
        );
        let caret = px::snap_rect(caret, ui.pixels_per_point());
        if panel == Panel::Browser {
            icons::paint_caret(painter, caret, Caret::Right, ink);
        } else {
            let points = vec![
                caret.right_top(),
                pos2(caret.left(), caret.center().y),
                caret.right_bottom(),
            ];
            painter.add(egui::Shape::convex_polygon(points, ink, egui::Stroke::NONE));
        }
        if st.focused {
            focus::paint_ring(painter, rect, 0.0);
        }
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
        w::accesskit(&response, |n| n.set_expanded(false));
        let toggle = Action::Ui(UiAction::TogglePanel(panel));
        let shortcut = self.keymap.label_for(&toggle, self.lang);
        let tip = match shortcut.as_deref() {
            Some(sc) => Tip::new(name).shortcut(sc),
            None => Tip::new(name),
        };
        tip.attach(&response);
        if response.clicked() {
            self.queue.push(toggle);
        }
    }

    /// The arrangement panel: its header (title, accent line when focused, the view's toolbar)
    /// and the view below.
    fn arrangement(&mut self, ui: &mut Ui, rect: Rect) {
        let focused = self.ui.focus == FocusArea::Arrangement;
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), size::PANEL_HEADER));
        let painter = ui.painter();
        px::fill(painter, header, 0.0, color32(color::SURFACE_2));
        if focused {
            px::hline(
                painter,
                header.x_range(),
                header.top(),
                color32(color::ACCENT),
            );
        }
        let title_color = if focused {
            color::TEXT_1
        } else {
            color::TEXT_2
        };
        let t = self.lang;
        let title = w::styled_galley(ui, t.shell_arrangement(), &typography::LABEL);
        let x = header.left() + space::SPACE_2;
        let pos = w::text_pos(header, &title, x, ui.pixels_per_point());
        let title_rect = Rect::from_min_size(pos, title.size());
        ui.painter().galley(pos, title, color32(title_color));
        let r = ui.interact(
            title_rect,
            Id::unique("komponist_arrangement_title"),
            Sense::hover(),
        );
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, t.shell_arrangement()));

        // The view's toolbar right of the 208 px title column (`width:208px` + gap 8).
        let tools = Rect::from_min_max(
            pos2(header.left() + size::TRACK_HEADER, header.top()),
            pos2(header.right() - space::SPACE_2, header.bottom()),
        );
        let mut tools_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(tools)
                .id_salt("komponist_arrangement_tools")
                .layout(Layout::left_to_right(Align::Center)),
        );
        self.with_view(FocusArea::Arrangement, |views, ctx| {
            views.arrangement.header(&mut tools_ui, ctx)
        });

        let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        self.shell.timeline_width = (body.width() - size::TRACK_HEADER).max(0.0);
        px::fill(ui.painter(), body, 0.0, color32(color::SURFACE_1));
        let mut body_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(body)
                .id_salt("komponist_arrangement_body")
                .layout(Layout::top_down(Align::Min)),
        );
        body_ui.set_clip_rect(body);
        self.with_view(FocusArea::Arrangement, |views, ctx| {
            views.arrangement.show(&mut body_ui, ctx)
        });
    }

    fn dock_body(&self) -> DockBody {
        let p = self.doc.project();
        match self.ui.layout.dock_tab {
            // While recording, the editor shows the take (S02 "Piano Roll · live").
            DockTab::Editor if self.recording.is_some() => DockBody::PianoRoll,
            DockTab::Editor => match self.ui.editor_area(p) {
                _ if self.ui.editor.clip.and_then(|c| p.find_clip(c)).is_none() => DockBody::NoClip,
                FocusArea::StepSequencer => DockBody::StepSeq,
                FocusArea::Waveform => DockBody::Waveform,
                _ => DockBody::PianoRoll,
            },
            DockTab::Devices => {
                let selected = self
                    .ui
                    .selection
                    .tracks
                    .iter()
                    .any(|t| p.track(*t).is_some());
                if selected {
                    DockBody::Devices
                } else {
                    DockBody::NoTrack
                }
            }
            DockTab::Mixer => DockBody::Mixer,
        }
    }

    fn dock(&mut self, ui: &mut Ui, rect: Rect) {
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), size::TAB));
        let mut strip_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(strip)
                .id_salt("komponist_dock_strip"),
        );
        dock::tab_strip(
            &mut strip_ui,
            strip,
            self.doc.project(),
            &self.ui,
            self.recording.is_some(),
            &self.keymap,
            self.lang,
            &mut self.queue,
        );
        if !self.ui.layout.dock_open {
            return;
        }
        let body = Rect::from_min_max(pos2(rect.left(), strip.bottom()), rect.max);
        px::fill(ui.painter(), body, 0.0, color32(color::SURFACE_1));
        let mut body_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(body)
                .id_salt("komponist_dock_body")
                .layout(Layout::top_down(Align::Min)),
        );
        body_ui.set_clip_rect(body);
        let area = self.dock_area();
        match self.dock_body() {
            DockBody::NoClip => centered_text(
                &mut body_ui,
                body,
                self.lang.shell_no_clip_open(),
                Some(self.lang.shell_no_clip_hint()),
            ),
            DockBody::NoTrack => centered_text(
                &mut body_ui,
                body,
                self.lang.shell_no_track(),
                Some(self.lang.shell_no_track_hint()),
            ),
            kind => self.with_view(area, |views, ctx| {
                let view: &mut dyn View = match kind {
                    DockBody::StepSeq => &mut *views.step_seq,
                    DockBody::Waveform => &mut *views.waveform,
                    DockBody::Devices => &mut *views.devices,
                    DockBody::Mixer => &mut *views.mixer,
                    _ => &mut *views.piano_roll,
                };
                view.show(&mut body_ui, ctx);
            }),
        }
    }

    /// A resize edge: drag resizes within the token range, double-click resets.
    fn edge(&mut self, ui: &mut Ui, edge: &Edge, screen: Rect) {
        let name = match edge.panel {
            Panel::Browser => self.lang.shell_browser_edge(),
            Panel::Inspector => self.lang.shell_inspector_edge(),
            Panel::Dock => self.lang.shell_dock_edge(),
        };
        let id = Id::unique(("komponist_edge", name));
        let response = ui.interact(edge.grab, id, Sense::click_and_drag());
        let cursor = match edge.panel {
            Panel::Dock => CursorIcon::ResizeVertical,
            _ => CursorIcon::ResizeHorizontal,
        };
        if response.hovered() || response.dragged() {
            ui.set_cursor_icon(cursor);
        }
        let current = match edge.panel {
            Panel::Browser => self.ui.layout.browser_width,
            Panel::Inspector => self.ui.layout.inspector_width,
            Panel::Dock => layout::clamp_dock(self.ui.layout.dock_height, screen.height()),
        };
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::Splitter, true, name);
            info.value = Some(f64::from(current));
            info
        });
        if response.double_clicked() {
            self.queue
                .push(Action::Ui(UiAction::ResetPanelSize(edge.panel)));
        } else if response.dragged()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let size = layout::size_at(edge.panel, pos, screen);
            if size != current {
                self.queue.push(Action::Ui(UiAction::ResizePanel {
                    panel: edge.panel,
                    size,
                }));
            }
        }
    }
}
