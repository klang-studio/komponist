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
use komponist_model::{AudioInput, HardwareInput, PluginRef, Project};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, space, typography};
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::dropdown::{Dropdown, Item as DropItem};
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
use crate::views::catalog_plugin_tag;

/// The names of the track types the dialog offers, in segment order; a later wave adds "Audio"
/// here.
fn kinds(t: Lang) -> [&'static str; 3] {
    [
        t.new_track_midi(),
        t.new_track_type_audio(),
        t.new_track_bus(),
    ]
}

/// Index of "Bus" in [`kinds`].
pub const KIND_BUS: usize = 2;
/// Index of Audio in the track type control.
pub const KIND_AUDIO: usize = 1;

/// Reusable input choices for a new track, before it has any output edges.
pub fn input_choices(project: &Project, t: Lang, channels: u16) -> Vec<(AudioInput, String)> {
    let mut choices = vec![(AudioInput::None, t.new_track_no_input().to_owned())];
    for first in 0..channels {
        choices.push((
            AudioInput::Hardware(HardwareInput {
                first,
                stereo: false,
            }),
            t.new_track_input_mono(first + 1),
        ));
    }
    for first in (0..channels.saturating_sub(1)).step_by(2) {
        choices.push((
            AudioInput::Hardware(HardwareInput {
                first,
                stereo: true,
            }),
            t.new_track_input_pair(first + 1, first + 2),
        ));
    }
    choices.extend(
        project
            .tracks
            .iter()
            .filter(|track| track.is_bus())
            .map(|track| {
                (
                    AudioInput::Bus(track.id),
                    t.new_track_input_bus(&track.name),
                )
            }),
    );
    choices
}

struct InputChoice {
    input: AudioInput,
    id: Option<String>,
    label: String,
    enabled: bool,
    heading: Option<&'static str>,
    detail: Option<String>,
}

fn jack_input_id(
    sources: &[komponist_audio::JackCaptureChannel],
    input: HardwareInput,
) -> Option<String> {
    let slot = usize::from(input.first);
    let first = &sources.get(slot)?.port.name;
    let second = if input.stereo {
        Some(&sources.get(slot + 1)?.port.name)
    } else {
        None
    };
    Some(format!("jack:{first:?}:{second:?}"))
}

fn track_input_items(
    project: &Project,
    lang: Lang,
    channels: u16,
    jack: Option<&[komponist_audio::JackCaptureChannel]>,
) -> Vec<InputChoice> {
    let generic = input_choices(project, lang, if jack.is_some() { 0 } else { channels });
    let mut choices: Vec<_> = generic
        .into_iter()
        .map(|(input, label)| InputChoice {
            input,
            id: match input {
                AudioInput::Bus(track) => Some(format!("bus:{}", track.0)),
                _ => None,
            },
            label,
            enabled: true,
            heading: None,
            detail: None,
        })
        .collect();
    if let Some(jack) = jack {
        use crate::views::audio_inputs::jack_input_choices;
        let groups = jack_input_choices(jack);
        let named = [
            lang.audio_input_section_mono(),
            lang.audio_input_section_stereo(),
        ]
        .into_iter()
        .zip(groups)
        .flat_map(|(heading, group)| {
            group
                .into_iter()
                .enumerate()
                .map(move |(index, source)| InputChoice {
                    input: AudioInput::Hardware(source.input),
                    id: jack_input_id(jack, source.input),
                    label: source.label(lang),
                    enabled: source.available,
                    heading: (index == 0).then_some(heading),
                    detail: source.detail,
                })
        });
        choices.splice(1..1, named);
        if let Some(bus) = choices
            .iter_mut()
            .find(|option| matches!(option.input, AudioInput::Bus(_)))
        {
            bus.heading = Some(lang.audio_input_section_bus());
        }
    }
    choices
}

/// First unused mono channel of the configured capture device; no input on the null backend.
pub fn first_unused_input(project: &Project, channels: u16) -> AudioInput {
    first_unused_mono(project, 0..channels)
}

/// Selects only connected channels in the JACK route, keeping their hardware indices.
fn first_unused_jack_input(
    project: &Project,
    channels: u16,
    sources: &[komponist_audio::JackCaptureChannel],
) -> AudioInput {
    first_unused_mono(
        project,
        (0..channels).filter(|&first| {
            sources
                .get(usize::from(first))
                .is_some_and(|source| source.connected)
        }),
    )
}

fn first_unused_mono(project: &Project, mut channels: impl Iterator<Item = u16>) -> AudioInput {
    channels.find(|channel| !project.tracks.iter().any(|track| {
        matches!(track.audio.input, AudioInput::Hardware(h) if h.first == *channel || (h.stereo && h.first.checked_add(1) == Some(*channel)))
    })).map_or(AudioInput::None, |first| AudioInput::Hardware(HardwareInput { first, stereo: false }))
}

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
    /// Input chosen for new audio tracks.
    pub input: AudioInput,
    /// Canonical JACK source selected for the current hardware slot.
    jack_input: Option<String>,
    /// Configured capture channel count; zero on the null backend.
    pub available_input_channels: u16,
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
            input: AudioInput::None,
            jack_input: None,
            available_input_channels: 0,
            query: String::new(),
            name: String::new(),
            name_touched: false,
            count: 1,
            focus_search: true,
            scroll: false,
        }
    }

    pub(crate) fn choose_default_input(
        &mut self,
        project: &Project,
        engine: &crate::engine_sync::EngineSync,
    ) {
        let channels = engine.audio_input_channels();
        self.available_input_channels = channels;
        let sources = engine.jack_capture_sources();
        let input = sources.map_or_else(
            || first_unused_input(project, channels),
            |sources| first_unused_jack_input(project, channels, sources),
        );
        self.select_input(input, sources);
    }

    fn select_input(
        &mut self,
        input: AudioInput,
        sources: Option<&[komponist_audio::JackCaptureChannel]>,
    ) {
        self.input = input;
        self.jack_input = match input {
            AudioInput::Hardware(input) => {
                sources.and_then(|sources| jack_input_id(sources, input))
            }
            _ => None,
        };
    }

    fn revalidate_jack_input(&mut self, engine: &crate::engine_sync::EngineSync) {
        let sources = engine.jack_capture_sources();
        if let AudioInput::Hardware(input) = self.input
            && (sources.is_some() || self.jack_input.is_some())
            && (self.jack_input != sources.and_then(|sources| jack_input_id(sources, input))
                || !engine.hardware_input_available(input))
        {
            self.select_input(AudioInput::None, sources);
        }
    }

    /// What "Anlegen" creates.
    pub fn spec(&self) -> Spec {
        let name = self.name.trim();
        let bus = self.kind == KIND_BUS;
        Spec {
            bus,
            audio: self.kind == KIND_AUDIO,
            input: self.input,
            instrument: if bus || self.kind == KIND_AUDIO {
                None
            } else {
                self.instrument.clone()
            },
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
    /// Audio track instead of MIDI.
    pub audio: bool,
    /// Input for each audio track.
    pub input: AudioInput,
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
    /// Current capture routes for the Audio Input menu.
    pub engine: &'a crate::engine_sync::EngineSync,
}

/// The words default names are made of in `t` (`MIDI`, `Drums`, `Bus`, `Gruppe` / `Group`): what
/// `edits::add_track_in` and `edits::add_bus_in` take for a track or bus without a name.
pub fn default_names(t: Lang) -> DefaultNames {
    DefaultNames {
        track: t.default_name_track(),
        instrument: None,
        bus: t.default_name_bus(),
        group: t.default_name_group(),
        audio: t.default_name_audio(),
    }
}

/// Track names reflect the selected plugin's advertised role before it is loaded.
pub(crate) fn midi_names(
    t: Lang,
    catalog: &komponist_plugins::Catalog,
    instrument: Option<&PluginRef>,
) -> DefaultNames {
    let mut names = default_names(t);
    if instrument
        .and_then(|plugin| catalog.resolve(plugin))
        .is_some_and(|entry| entry.features.iter().any(|feature| feature == "drum"))
    {
        names.instrument = Some(t.default_name_drums());
    }
    names
}

/// The name a new bus gets when none is given: „Bus 1“, „Bus 2“, … (`edits::add_bus_in`).
pub fn auto_bus_name(t: Lang, project: &Project) -> String {
    edits::numbered_track_name(project, default_names(t).bus)
}

/// The name a new track with `instrument` gets when none is given (`edits::add_track_in`).
fn auto_name(names: &DefaultNames, project: &Project, instrument: Option<&PluginRef>) -> String {
    let spec = NewTrack {
        name: None,
        hue: None,
        instrument: instrument.cloned(),
        index: None,
        arm_exclusive: false,
    };
    edits::add_track_in(project, spec, names)
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
    let jack_capture_sources = view.engine.jack_capture_sources();
    s.revalidate_jack_input(view.engine);
    let t = komponist_ui::lang(ctx);
    let search_id = Id::unique(SEARCH_ID);
    let name_id = Id::unique(NAME_ID);
    let input_id = Id::unique("new-track-audio-input");
    let input_popup = Dropdown::popup_id(input_id);
    let focused = ctx.memory(|mem| mem.focused());
    // An open input popup gets Enter first, including when its sources change this frame.
    let input_open =
        s.kind == KIND_AUDIO && komponist_ui::widgets::floating::is_open(ctx, input_popup);
    let keys = !input_open
        && (focused.is_none() || focused == Some(search_id) || focused == Some(name_id));
    let bus = s.kind != 0;
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
                if s.kind == 0 {
                    instrument_row(ui, t, s, view, &mut rows, control, search_id);
                    gap(ui);
                }
                if s.kind == KIND_AUDIO {
                    row(ui, t.new_track_input(), |ui| {
                        let mut choices = track_input_items(
                            view.project,
                            t,
                            s.available_input_channels,
                            jack_capture_sources,
                        );
                        if jack_capture_sources.is_some() {
                            for choice in &mut choices {
                                if let AudioInput::Hardware(input) = choice.input
                                    && !view.engine.hardware_input_available(input)
                                {
                                    if choice.enabled {
                                        choice.label = t.audio_input_unavailable(&choice.label);
                                    }
                                    choice.enabled = false;
                                }
                            }
                        }
                        let mut selected = choices
                            .iter()
                            .position(|choice| choice.input == s.input)
                            .unwrap_or(0);
                        let items: Vec<_> = choices
                            .iter()
                            .map(|choice| DropItem {
                                label: &choice.label,
                                enabled: choice.enabled,
                            })
                            .collect();
                        let dropdown = Dropdown::new(t.new_track_input(), &mut selected, &items)
                            .id(input_id)
                            .width(control);
                        let details: Vec<_> = choices
                            .iter()
                            .map(|choice| komponist_ui::widgets::dropdown::RowDetails {
                                heading: choice.heading,
                                detail: choice.detail.as_deref(),
                            })
                            .collect();
                        let response = if jack_capture_sources.is_some() {
                            let ids: Vec<_> =
                                choices.iter().map(|choice| choice.id.as_deref()).collect();
                            ui.add(
                                dropdown
                                    .fitting_list(
                                        crate::views::audio_inputs::MAX_ROWS as f32
                                            * size::MENU_ROW,
                                        &details,
                                    )
                                    .option_ids(&ids),
                            )
                        } else {
                            ui.add(dropdown)
                        };
                        if response.changed() {
                            s.select_input(choices[selected].input, jack_capture_sources);
                        }
                        if jack_capture_sources.is_some()
                            && matches!(choices[selected].input, AudioInput::Hardware(_))
                        {
                            let choice = &choices[selected];
                            let mut tip = komponist_ui::widgets::tooltip::Tip::new(&choice.label);
                            if let Some(detail) = &choice.detail {
                                tip = tip.detail(detail);
                            }
                            tip.attach(&response);
                        }
                    });
                    gap(ui);
                }
                row(ui, t.new_track_name(), |ui| {
                    let hint = if s.kind == KIND_BUS {
                        auto_bus_name(t, view.project)
                    } else if s.kind == KIND_AUDIO {
                        t.default_name_audio().to_owned()
                    } else {
                        let instrument = s.instrument.as_ref();
                        let names = midi_names(t, view.engine.host().catalog(), instrument);
                        auto_name(&names, view.project, instrument)
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
                    s.revalidate_jack_input(view.engine);
                    out = Some(Output::Create(s.spec()));
                }
            });
        });
        ui.allocate_space(vec2(width, 0.0));
    });
    if out.is_none() {
        if enter {
            s.revalidate_jack_input(view.engine);
            out = Some(Output::Create(s.spec()));
        } else if response.should_close() {
            out = Some(Output::Cancel);
        }
    }
    if s.kind != KIND_AUDIO || out.is_some() {
        komponist_ui::widgets::floating::close(ctx, input_popup);
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
                        let r = entry(
                            ui,
                            inner,
                            &row.name,
                            catalog_plugin_tag(t, plugin),
                            selected,
                        );
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
        let mut s = State::new(Some(
            komponist_model::fixtures::demo_project().tracks[1]
                .channel
                .devices[0]
                .plugin
                .clone(),
        ));
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
        assert_eq!(auto_name(&default_names(Lang::De), &p, None), "MIDI");
        assert_eq!(auto_name(&default_names(Lang::En), &p, None), "MIDI");
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
        assert_eq!(auto_name(&default_names(Lang::De), &p, None), "MIDI 2");
        assert_eq!(auto_name(&default_names(Lang::En), &p, None), "MIDI 2");
    }
}
