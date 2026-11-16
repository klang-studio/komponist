//! The "Neue Spur" dialog (owner request 2026-11-15; the screens have none): opened by a click, Enter
//! or Space on the "Neue Spur" box below the tracks. Ctrl T stays the direct quick-create.
//!
//! | Row | Control |
//! |---|---|
//! | Spurtyp | a segmented control: "MIDI" or "Bus" (an effect bus, WP-103); "Audio" is one more [`kinds`] entry later, not a layout change |
//! | Instrument | a search field over the instruments of the catalog (the browser's matching, [`crate::views::browser::model::instrument_rows`]) and "Kein Instrument" at the top; ↑ ↓ choose, typing selects the first hit. Hidden for a Bus |
//! | Name | prefilled with the name the app would give ([`auto_name`], "Hexa", "MIDI 2", "Bus 1"); it follows the instrument until it is edited; blank = automatic |
//! | Anzahl | 1 – [`COUNT_MAX`]; several tracks get numbered names ("Lead 1", "Lead 2", or the automatic names "MIDI", "MIDI 2", …) |
//!
//! Enter (not while a button or the count field has the focus) creates, Esc and "Abbrechen" cancel.
//! The choice of instrument is remembered for the session ([`crate::dialogs::DialogState`]).

use egui::{Color32, Context, Id, Key, Modifiers, Role, Sense, Ui, UiBuilder, WidgetInfo, vec2};
use komponist_i18n::Lang;
use komponist_model::edits::{self, DefaultNames, NewTrack};
use komponist_model::{PluginRef, Project};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, space, typography};
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::tabs::{Item, Segmented};
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::{self as w};
use komponist_ui::{px, state};

use super::file::field_label;
use super::metrics::dialog as m;
use super::metrics::new_track as nm;
use super::{ActionButton, button_row, header, modal, styled};
use crate::views::browser::model::{Row, RowKind};
use crate::views::format_tag;

/// The names of the track types the dialog offers, in segment order; a later wave adds "Audio"
/// here.
fn kinds(t: Lang) -> [&'static str; 2] {
    [t.new_track_midi(), t.new_track_bus()]
}

/// Index of "Bus" in [`kinds`].
pub const KIND_BUS: usize = 1;

/// Most tracks one dialog creates.
pub const COUNT_MAX: u32 = 16;

const SEARCH_ID: &str = "komponist_new_track_search";
const NAME_ID: &str = "komponist_new_track_name";

/// State of the open dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// Index into [`kinds`].
    pub kind: usize,
    /// The chosen instrument, `None` = "Kein Instrument".
    pub instrument: Option<PluginRef>,
    /// The search text.
    pub query: String,
    /// The name field.
    pub name: String,
    /// The name was typed: it no longer follows the instrument.
    pub name_touched: bool,
    /// How many tracks.
    pub count: u32,
    /// The search field takes the focus in the first frame.
    focus_search: bool,
    /// The list scrolls to the chosen row (after a key).
    scroll: bool,
}

impl State {
    /// A fresh dialog with `instrument` chosen.
    pub fn new(instrument: Option<PluginRef>) -> Self {
        State {
            kind: 0,
            instrument,
            query: String::new(),
            name: String::new(),
            name_touched: false,
            count: 1,
            focus_search: true,
            scroll: false,
        }
    }

    /// What "Anlegen" creates.
    pub fn spec(&self) -> Spec {
        let name = self.name.trim();
        let bus = self.kind == KIND_BUS;
        Spec {
            bus,
            instrument: if bus { None } else { self.instrument.clone() },
            name: (self.name_touched && !name.is_empty()).then(|| name.to_owned()),
            count: self.count.clamp(1, COUNT_MAX) as usize,
        }
    }
}

/// What "Anlegen" creates.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// An effect bus instead of a MIDI track (no instrument).
    pub bus: bool,
    /// The instrument, `None` = none.
    pub instrument: Option<PluginRef>,
    /// The typed name; `None` = the automatic names.
    pub name: Option<String>,
    /// How many tracks (1 ..= [`COUNT_MAX`]).
    pub count: usize,
}

impl Spec {
    /// The tracks, in creation order. The last one stays armed (each is armed exclusively, as a
    /// Ctrl T or a drop would).
    pub fn tracks(&self) -> Vec<NewTrack> {
        (1..=self.count)
            .map(|i| NewTrack {
                name: self.name.as_ref().map(|n| {
                    if self.count == 1 {
                        n.clone()
                    } else {
                        format!("{n} {i}")
                    }
                }),
                hue: None,
                instrument: self.instrument.clone(),
                index: None,
                arm_exclusive: true,
            })
            .collect()
    }
}

/// What the user did.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// Create the tracks.
    Create(Spec),
    /// Closed without a change.
    Cancel,
}

/// What the dialog reads from the app.
pub struct View<'a> {
    /// The project (the automatic names).
    pub project: &'a Project,
    /// The instruments matching a search text, in list order.
    pub instruments: &'a dyn Fn(&str) -> Vec<Row>,
}

/// The words default names are made of in `t` (`MIDI`, `Drums`, `Bus`, `Gruppe` / `Group`): what
/// `edits::add_track_in` and `edits::add_bus_in` take for a track or bus without a name.
pub fn default_names(t: Lang) -> DefaultNames {
    DefaultNames {
        track: t.default_name_track(),
        drums: t.default_name_drums(),
        bus: t.default_name_bus(),
        group: t.default_name_group(),
    }
}

/// The name a new bus gets when none is given: „Bus 1“, „Bus 2“, … (`edits::add_bus_in`).
pub fn auto_bus_name(t: Lang, project: &Project) -> String {
    edits::numbered_track_name(project, default_names(t).bus)
}

/// The name a new track with `instrument` gets when none is given (`edits::add_track_in`).
pub fn auto_name(t: Lang, project: &Project, instrument: Option<&PluginRef>) -> String {
    let spec = NewTrack {
        name: None,
        hue: None,
        instrument: instrument.cloned(),
        index: None,
        arm_exclusive: false,
    };
    edits::add_track_in(project, spec, &default_names(t))
        .ok()
        .and_then(|(p, id)| p.track(id).map(|t| t.name.clone()))
        .unwrap_or_default()
}

fn plugin_of(row: &Row) -> Option<&PluginRef> {
    match &row.kind {
        RowKind::Plugin { plugin, .. } => Some(plugin),
        _ => None,
    }
}

/// Draws the dialog and handles its keys; returns the answer in the frame it is given.
pub fn show(ctx: &Context, s: &mut State, view: &View<'_>) -> Option<Output> {
    let t = komponist_ui::lang(ctx);
    let search_id = Id::unique(SEARCH_ID);
    let name_id = Id::unique(NAME_ID);
    let focused = ctx.memory(|mem| mem.focused());
    // Enter and ↑ ↓ belong to the dialog while no button and no number entry has the focus.
    let keys = focused.is_none() || focused == Some(search_id) || focused == Some(name_id);
    let bus = s.kind == KIND_BUS;
    let (mut up, mut down, mut enter) = (false, false, false);
    if keys {
        ctx.input_mut(|i| {
            // The instrument list is hidden for a bus: ↑ ↓ have nothing to move over.
            if !bus {
                up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
                down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
            }
            enter = i.consume_key(Modifiers::NONE, Key::Enter);
        });
    }
    let mut out = None;
    let mut rows = (view.instruments)(&s.query);
    if up || down {
        // The list is "Kein Instrument" and then the rows.
        let at = s
            .instrument
            .as_ref()
            .and_then(|p| rows.iter().position(|r| plugin_of(r) == Some(p)))
            .map_or(0, |i| i + 1);
        let to = if down {
            (at + 1).min(rows.len())
        } else {
            at.saturating_sub(1)
        };
        s.instrument = to.checked_sub(1).and_then(|i| plugin_of(&rows[i]).cloned());
        s.scroll = true;
    }

    let response = modal(ctx, "new-track", color32(color::SURFACE_2), |ui| {
        let width = nm::WIDTH;
        ui.set_width(width);
        let builder = UiBuilder::new()
            .accessibility_role(Role::Dialog)
            .accessibility_label(t.new_track_title());
        ui.scope_builder(builder, |ui| {
            if header(ui, width, t.new_track_title()) {
                out = Some(Output::Cancel);
            }
            let body = egui::Frame::new()
                .fill(color32(color::SURFACE_1))
                .inner_margin(egui::Margin::same(m::PADDING as i8));
            body.show(ui, |ui| {
                let inner = width - 2.0 * m::PADDING;
                ui.set_width(inner);
                let control = inner - nm::LABEL_COLUMN;
                row(ui, t.new_track_kind(), |ui| {
                    let items = kinds(t).map(Item::new);
                    ui.add(Segmented::new(t.new_track_kind(), &mut s.kind, &items));
                });
                gap(ui);
                if s.kind != KIND_BUS {
                    instrument_row(ui, t, s, view, &mut rows, control, search_id);
                    gap(ui);
                }
                row(ui, t.new_track_name(), |ui| {
                    let hint = if s.kind == KIND_BUS {
                        auto_bus_name(t, view.project)
                    } else {
                        auto_name(t, view.project, s.instrument.as_ref())
                    };
                    if !s.name_touched {
                        s.name.clone_from(&hint);
                    }
                    let r = ui.add(
                        TextField::new(&mut s.name, t.new_track_name())
                            .hint(hint)
                            .id(name_id)
                            .width(control),
                    );
                    if r.changed() {
                        s.name_touched = true;
                    }
                });
                gap(ui);
                row(ui, t.new_track_count(), |ui| {
                    ui.add(
                        NumberField::new(&mut s.count, t.new_track_count())
                            .range(1.0..=f64::from(COUNT_MAX))
                            .step(1.0)
                            .decimals(0)
                            .default_value(1.0)
                            .width(nm::COUNT_WIDTH),
                    );
                });
            });
            let footer = egui::Frame::new().inner_margin(egui::Margin::symmetric(
                m::PADDING as i8,
                m::FOOTER_PADDING_Y as i8,
            ));
            footer.show(ui, |ui| {
                let r = button_row(
                    ui,
                    width - 2.0 * m::PADDING,
                    vec![
                        ActionButton::dialog(t.dialog_cancel(), Kind::Secondary, 2),
                        ActionButton::dialog(t.new_track_create(), Kind::Primary, 2),
                    ],
                );
                if r[0].clicked() {
                    out = Some(Output::Cancel);
                }
                if r[1].clicked() {
                    out = Some(Output::Create(s.spec()));
                }
            });
        });
        ui.allocate_space(vec2(width, 0.0));
    });
    if out.is_none() {
        if enter {
            out = Some(Output::Create(s.spec()));
        } else if response.should_close() {
            out = Some(Output::Cancel);
        }
    }
    out
}

fn gap(ui: &mut Ui) {
    ui.add_space(nm::ROW_GAP);
}

/// A label and its control at the top of the row.
fn row(ui: &mut Ui, label: &str, controls: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        field_label(ui, label);
        controls(ui);
    });
}

/// "Instrument": the search field over the list.
fn instrument_row(
    ui: &mut Ui,
    t: Lang,
    s: &mut State,
    view: &View<'_>,
    rows: &mut Vec<Row>,
    width: f32,
    search_id: Id,
) {
    ui.horizontal_top(|ui| {
        field_label(ui, t.new_track_instrument());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = nm::LIST_GAP;
            let before = s.query.clone();
            let r = ui.add(
                TextField::new(&mut s.query, t.new_track_search())
                    .hint(t.new_track_search())
                    .id(search_id)
                    .width(width),
            );
            if s.focus_search {
                r.request_focus();
                s.focus_search = false;
            }
            if s.query != before {
                // Typing chooses the first hit (so typing and Enter add it), else "Kein Instrument".
                *rows = (view.instruments)(&s.query);
                s.instrument = rows.first().and_then(plugin_of).cloned();
                s.scroll = true;
            }
            list(ui, t, s, rows, width);
        });
    });
}

fn list(ui: &mut Ui, t: Lang, s: &mut State, rows: &[Row], width: f32) {
    let height = nm::LIST_ROWS * size::MENU_ROW;
    let frame = egui::Frame::new()
        .fill(color32(color::SURFACE_2))
        .corner_radius(komponist_ui::theme::corner_radius(radius::M))
        .inner_margin(egui::Margin::same(space::SPACE_1 as i8));
    let builder = UiBuilder::new()
        .accessibility_role(Role::ListBox)
        .accessibility_label(t.new_track_instrument());
    ui.scope_builder(builder, |ui| {
        frame.show(ui, |ui| {
            let inner = width - 2.0 * space::SPACE_1;
            ui.set_width(inner);
            ui.set_height(height);
            let scroll_to = std::mem::take(&mut s.scroll);
            egui::ScrollArea::vertical()
                .id_salt("komponist_new_track_list")
                .auto_shrink([false, false])
                .max_height(height)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut chosen = None;
                    let none = entry(
                        ui,
                        inner,
                        t.new_track_no_instrument(),
                        "",
                        s.instrument.is_none(),
                    );
                    if none.clicked() {
                        chosen = Some(None);
                    }
                    if scroll_to && s.instrument.is_none() {
                        none.scroll_to_me(None);
                    }
                    for row in rows.iter() {
                        let Some(plugin) = plugin_of(row) else {
                            continue;
                        };
                        let selected = s.instrument.as_ref() == Some(plugin);
                        let r = entry(ui, inner, &row.name, format_tag(t, plugin.format), selected);
                        if scroll_to && selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some(Some(plugin.clone()));
                        }
                    }
                    if let Some(c) = chosen {
                        s.instrument = c;
                    }
                });
        });
    });
}

/// One list row: the name, the format tag at the right; the chosen row `accent.surface`.
fn entry(ui: &mut Ui, width: f32, name: &str, tag: &str, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, size::MENU_ROW), Sense::click());
    let st = state::State::of(ui, &response);
    let fill = if selected {
        Some(color32(color::ACCENT_SURFACE))
    } else if st.hovered {
        Some(color32(color::SURFACE_3))
    } else {
        None
    };
    if let Some(fill) = fill {
        px::fill(ui.painter(), rect, radius::S, fill);
    }
    let ppp = ui.pixels_per_point();
    let g = styled(ui, name, &typography::UI, color32(color::TEXT_1), None);
    let x = rect.left() + space::SPACE_2;
    ui.painter()
        .galley(w::text_pos(rect, &g, x, ppp), g, Color32::PLACEHOLDER);
    if !tag.is_empty() {
        let g = styled(ui, tag, &typography::SMALL, color32(color::TEXT_3), None);
        let x = rect.right() - space::SPACE_2 - g.size().x;
        ui.painter()
            .galley(w::text_pos(rect, &g, x, ppp), g, Color32::PLACEHOLDER);
    }
    let label = name.to_owned();
    response.widget_info(|| WidgetInfo::selected(Role::ListBoxOption, true, selected, &label));
    w::accesskit(&response, |n| n.set_selected(selected));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_numbers_typed_names_only() {
        let mut s = State::new(None);
        assert_eq!(s.spec().name, None);
        s.name = "  Lead ".into();
        assert_eq!(s.spec().name, None, "untouched names follow the instrument");
        s.name_touched = true;
        s.count = 3;
        let names: Vec<_> = s.spec().tracks().into_iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                Some("Lead 1".to_owned()),
                Some("Lead 2".to_owned()),
                Some("Lead 3".to_owned())
            ]
        );
        s.count = 1;
        assert_eq!(s.spec().tracks()[0].name.as_deref(), Some("Lead"));
        s.name = " ".into();
        assert_eq!(s.spec().name, None);
        s.count = 99;
        assert_eq!(s.spec().count, COUNT_MAX as usize);
    }

    #[test]
    fn a_bus_has_no_instrument_and_gets_a_bus_name() {
        let mut s = State::new(Some(crate::app::hexa_ref()));
        s.kind = KIND_BUS;
        let spec = s.spec();
        assert!(spec.bus && spec.instrument.is_none());
        let p = Project::new("x");
        assert_eq!(auto_bus_name(Lang::De, &p), "Bus 1");
        assert_eq!(auto_bus_name(Lang::En, &p), "Bus 1");
    }

    #[test]
    fn the_automatic_name_is_the_one_add_track_gives() {
        let p = Project::new("x");
        assert_eq!(auto_name(Lang::De, &p, None), "MIDI");
        assert_eq!(auto_name(Lang::En, &p, None), "MIDI");
        let (p, _) = edits::add_track(
            &p,
            NewTrack {
                name: None,
                hue: None,
                instrument: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        assert_eq!(auto_name(Lang::De, &p, None), "MIDI 2");
        assert_eq!(auto_name(Lang::En, &p, None), "MIDI 2");
    }
}
