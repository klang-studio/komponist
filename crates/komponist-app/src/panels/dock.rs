//! The dock's tab strip (`size.tab` 28 on `surface.0`; `Screens.dc.html` `tab`, `dock`,
//! `editorTabs`; `Layout.dc.html` `dockRules`).
//!
//! Tabs Editor · Geräte · Mixer (fixed) and "+" (later views, disabled in v0.1); on the right
//! "Maximieren" / "Maximiert" (Ctrl ↑) and the caret that collapses the dock to this strip or
//! opens it again (Ctrl 3). The editor tab is titled with the open clip's name, its editor kind
//! as secondary text and the track colour as mark ("■ Lead A  Piano Roll"), else "Kein Clip";
//! while recording "Aufnahme  Piano Roll · live" with a `state.record` mark (S02). A
//! click on a tab of a collapsed dock opens it. The active tab carries the accent line while
//! the dock has the focus.

use egui::{
    Align, Color32, Layout, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_model::{ClipContent, Project};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, TrackPalette, color, size};
use komponist_ui::icons::{self, Caret};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::metrics as wm;
use komponist_ui::widgets::tabs::{Item, TabBar, TabExtra};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{focus, px};

use crate::actions::{Action, UiAction};
use crate::keymap::Keymap;
use crate::metrics::dock as m;
use crate::ui_state::{DockTab, Panel, UiState};

/// Accessible names of the strip's controls.
pub mod names {
    /// The tab list.
    pub const TABS: &str = "Dock";
    /// Editor tab without an open clip (`editorTabs('Kein Clip', …)`).
    pub const NO_CLIP: &str = "Kein Clip";
    /// Devices tab.
    pub const DEVICES: &str = "Geräte";
    /// Mixer tab.
    pub const MIXER: &str = "Mixer";
    /// The "+" for later views.
    pub const MORE: &str = "Weitere Ansichten";
    /// Maximise button, off.
    pub const MAXIMIZE: &str = "Maximieren";
    /// Maximise button, on.
    pub const MAXIMIZED: &str = "Maximiert";
    /// The caret while the dock is open.
    pub const COLLAPSE: &str = "Dock einklappen";
    /// The caret while the dock is collapsed.
    pub const EXPAND: &str = "Dock ausklappen";
    /// Secondary text of the editor tab for MIDI clips.
    pub const PIANO_ROLL: &str = "Piano Roll";
    /// Secondary text of the editor tab for step clips.
    pub const STEP_SEQUENCER: &str = "Step Sequencer";
}

/// Title, secondary text and colour mark of the editor tab.
pub fn editor_tab(project: &Project, ui: &UiState) -> (String, &'static str, Option<Color32>) {
    let Some((track, clip)) = ui.editor.clip.and_then(|c| project.find_clip(c)) else {
        return (names::NO_CLIP.to_owned(), "", None);
    };
    let kind = match clip.content {
        ClipContent::Midi(_) => names::PIANO_ROLL,
        ClipContent::Step(_) => names::STEP_SEQUENCER,
    };
    let mark = color32(TrackPalette::new(track.hue.0).base);
    (clip.name.clone(), kind, Some(mark))
}

const TABS: [DockTab; 3] = [DockTab::Editor, DockTab::Devices, DockTab::Mixer];

/// Draws the strip into `rect` (28 high); `recording`: the editor shows the take being recorded.
pub fn tab_strip(
    ui: &mut Ui,
    rect: Rect,
    project: &Project,
    state: &UiState,
    recording: bool,
    keymap: &Keymap,
    actions: &mut Vec<Action>,
) {
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_0));
    let layout = &state.layout;
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing.x = 0.0;
    let (title, kind, mark) = if recording {
        (
            crate::record::names::TAB.to_owned(),
            crate::record::names::TAB_KIND,
            Some(color32(color::STATE_RECORD)),
        )
    } else {
        editor_tab(project, state)
    };
    let items = [
        Item::new(&title),
        Item::new(names::DEVICES),
        Item::new(names::MIXER),
    ];
    let extras = [TabExtra::new(mark, kind)];
    let current = TABS.iter().position(|t| *t == layout.dock_tab).unwrap_or(0);
    // A collapsed dock shows no active tab; a click on any tab opens it.
    let mut selected = if layout.dock_open {
        current
    } else {
        usize::MAX
    };
    let before = selected;
    let focused = layout.dock_open && state.focus.in_dock();
    row.add(
        TabBar::new(names::TABS, &mut selected, &items)
            .extras(&extras)
            .accent_line(focused),
    );
    if selected != before
        && let Some(&tab) = TABS.get(selected)
    {
        actions.push(Action::Ui(UiAction::SetDockTab(tab)));
    }
    row.add_space(m::PLUS_PADDING_X - wm::BUTTON_PADDING_X);
    row.add_enabled(false, Button::icon(Icon::Plus, names::MORE).frameless(0));

    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.add_space(m::ACTIONS_PADDING_RIGHT);
        ui.spacing_mut().item_spacing.x = m::ACTIONS_GAP;
        let toggle = Action::Ui(UiAction::TogglePanel(Panel::Dock));
        let (name, caret) = if layout.dock_open {
            (names::COLLAPSE, Caret::Down)
        } else {
            (names::EXPAND, Caret::Right)
        };
        let shortcut = keymap.label_for(&toggle);
        if caret_button(ui, name, caret, layout.dock_open, shortcut.as_deref()).clicked() {
            actions.push(toggle);
        }
        let max = Action::Ui(UiAction::ToggleDockMaximized);
        let maximized = layout.dock_open && layout.dock_maximized;
        let label = if maximized {
            names::MAXIMIZED
        } else {
            names::MAXIMIZE
        };
        let shortcut = keymap.label_for(&max);
        let mut button = Button::new(label).on(maximized).compact().frameless(0);
        if let Some(sc) = shortcut.as_deref() {
            button = button.shortcut(sc);
        }
        if ui.add(button).clicked() {
            actions.push(max);
        }
    });
}

/// The 20 × 20 caret button: ▾ collapses an open dock, ▴ opens a collapsed one.
fn caret_button(
    ui: &mut Ui,
    name: &str,
    caret: Caret,
    open: bool,
    shortcut: Option<&str>,
) -> Response {
    let side = size::CONTROL_COMPACT;
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let st = State::of(ui, &response);
    let painter = ui.painter();
    if st.hovered || st.pressed {
        px::fill(
            painter,
            rect,
            komponist_tokens::radius::M,
            state::surface(if st.pressed { 3 } else { 2 }),
        );
    }
    let ink = state::text_for(color::TEXT_2, st);
    let [cw, ch] = wm::CARET;
    let r = px::snap_rect(
        Rect::from_center_size(rect.center(), vec2(cw, ch)),
        ui.pixels_per_point(),
    );
    if open {
        icons::paint_caret(painter, r, caret, ink);
    } else {
        // ▴: the caret turned up (`borderBottom` in `dockView`).
        let points = vec![
            r.left_bottom(),
            r.right_bottom(),
            pos2(r.center().x, r.top()),
        ];
        painter.add(egui::Shape::convex_polygon(points, ink, egui::Stroke::NONE));
    }
    if st.focused {
        focus::paint_ring(painter, rect, komponist_tokens::radius::M);
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    komponist_ui::widgets::accesskit(&response, |n| n.set_expanded(open));
    let tip = match shortcut {
        Some(sc) => Tip::new(name).shortcut(sc),
        None => Tip::new(name),
    };
    tip.attach(&response);
    response
}
