//! The gallery shell: the page of `Designsystem.dc.html` as an egui app.
//!
//! [`Gallery`] shows numbered [`Section`]s (01 Farben … 07 Globale Shortcuts) with a navigation bar
//! and a scale switch (1× / 2×). A section holds [`Group`]s of [`Card`]s laid out in a responsive
//! grid; a card has a title, a spec line, its content and the interaction list (`ix`) of the
//! design system.
//!
//! [`designsystem`] lists all sections. The token sections (01–03, 07) are filled here; the
//! component cards of 04–06 are placeholders that WP-20, WP-21 and WP-22 fill by setting
//! [`Card::content`] in [`designsystem`] (one line per card). A content function draws the
//! component in every state, e.g. with [`crate::state::force`].

mod tokens;

pub use tokens::{icon_examples, ix, shape_examples, track_colors};

use egui::epaint::RectShape;
use egui::{
    Align, Color32, Frame, Layout, Margin, Rect, Response, Role, Sense, Shape, Ui, WidgetInfo, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, radius, size, space};

use crate::state::{self, State};
use crate::{focus, fonts, px, theme, timeline, widgets};

/// Draws the content of a card.
pub type CardContent = fn(&mut Ui);

/// Presentation layout of the design page (its markup, not tokens): minimum card widths of the
/// page grids and the key column of the interaction lists. Only the gallery uses these.
pub mod page {
    /// Colour groups (`minmax(400px, 1fr)`).
    pub const CARDS_COLORS: f32 = 400.0;
    /// Components (`minmax(560px, 1fr)`).
    pub const CARDS_COMPONENTS: f32 = 560.0;
    /// State rules (`minmax(320px, 1fr)`).
    pub const CARDS_RULES: f32 = 320.0;
    /// Key column of an interaction list (`140px`).
    pub const IX_KEY_WIDTH: f32 = 140.0;
}

/// How a card's title is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heading {
    /// Component cards: title in `type.track`, spec line on the right.
    Title,
    /// Token cards: title as `type.label` (caps), spec line on the right.
    Label,
}

/// One card of a section.
#[derive(Clone, Copy, Debug)]
pub struct Card {
    /// Title ("Button", "Flächen").
    pub title: &'static str,
    /// Spec line in the top right ("h 24 · px 10 · radius.m · ui-medium"), may be empty.
    pub spec: &'static str,
    /// Title style.
    pub heading: Heading,
    /// A paragraph under the title, may be empty (the state rules).
    pub text: &'static str,
    /// Interaction list (`ix.*` of the design system): key, what happens.
    pub ix: &'static [(&'static str, &'static str)],
    /// What the card shows; `None` shows a placeholder naming [`Self::owner`].
    pub content: Option<CardContent>,
    /// The work package that fills the card.
    pub owner: &'static str,
}

impl Card {
    /// A component card titled `title`.
    pub const fn new(title: &'static str) -> Self {
        Self {
            title,
            spec: "",
            heading: Heading::Title,
            text: "",
            ix: &[],
            content: None,
            owner: "",
        }
    }

    /// A token card whose title is set as a label.
    pub const fn label(title: &'static str) -> Self {
        Self {
            heading: Heading::Label,
            ..Self::new(title)
        }
    }

    /// Sets the spec line.
    pub const fn spec(mut self, spec: &'static str) -> Self {
        self.spec = spec;
        self
    }

    /// Sets the paragraph under the title.
    pub const fn text(mut self, text: &'static str) -> Self {
        self.text = text;
        self
    }

    /// Sets the interaction list.
    pub const fn ix(mut self, ix: &'static [(&'static str, &'static str)]) -> Self {
        self.ix = ix;
        self
    }

    /// Sets the content.
    pub const fn content(mut self, content: CardContent) -> Self {
        self.content = Some(content);
        self
    }

    /// Marks the card as to be filled by `owner` (a work package).
    pub const fn owner(mut self, owner: &'static str) -> Self {
        self.owner = owner;
        self
    }
}

/// Cards in a responsive grid: as many columns of at least `min_width` as fit.
#[derive(Clone, Debug)]
pub struct Group {
    /// Minimum card width in points.
    pub min_width: f32,
    /// The cards.
    pub cards: Vec<Card>,
}

impl Group {
    /// A grid of `cards` with columns of at least `min_width`.
    pub fn grid(min_width: f32, cards: Vec<Card>) -> Self {
        Self { min_width, cards }
    }

    /// One card over the full width.
    pub fn full(card: Card) -> Self {
        Self {
            min_width: f32::INFINITY,
            cards: vec![card],
        }
    }
}

/// A numbered section of the page.
#[derive(Clone, Debug)]
pub struct Section {
    /// Number as shown ("01").
    pub number: &'static str,
    /// Title ("Farben").
    pub title: &'static str,
    /// Card groups, top to bottom.
    pub groups: Vec<Group>,
}

impl Section {
    /// A section without cards.
    pub fn new(number: &'static str, title: &'static str) -> Self {
        Self {
            number,
            title,
            groups: Vec::new(),
        }
    }

    /// Adds a group.
    #[must_use]
    pub fn group(mut self, group: Group) -> Self {
        self.groups.push(group);
        self
    }

    /// The label of its navigation button ("01 Farben").
    pub fn nav_label(&self) -> String {
        format!("{} {}", self.number, self.title)
    }
}

/// The sections of `Designsystem.dc.html`. WP-20/21/22 set the content of their cards here.
pub fn designsystem() -> Vec<Section> {
    let components = |cards: Vec<Card>| Group::grid(page::CARDS_COMPONENTS, cards);
    vec![
        tokens::colors(),
        tokens::typography_and_sizes(),
        tokens::state_rules(),
        Section::new("04", "Bedienelemente").group(components(vec![
            Card::new("Button")
                .spec("h 24 · px 10 · radius.m · ui-medium")
                .ix(ix::BUTTON)
                .content(crate::widgets::button::gallery)
                .owner("WP-20"),
            Card::new("Toggle")
                .spec("Spur-Toggle 18 × 16 · Schalter 28 × 16")
                .ix(ix::TOGGLE)
                .content(crate::widgets::toggle::gallery)
                .owner("WP-20"),
            Card::new("Dropdown")
                .spec("h 24 · min-w 96 · Menü radius.l")
                .ix(ix::DROPDOWN)
                .content(crate::widgets::dropdown::gallery)
                .owner("WP-20"),
            Card::new("Zahlenfeld, ziehbar")
                .spec("h 24 · px 8 · value-m · Einheit text.3")
                .ix(ix::NUMBER)
                .content(widgets::number_field::gallery)
                .owner("WP-21"),
            Card::new("Drehregler, flach")
                .spec("Ø 28 (klein 20) · Bogen 270° · Strich 2")
                .ix(ix::KNOB)
                .content(widgets::knob::gallery)
                .owner("WP-21"),
            Card::new("Fader und Pegelanzeige")
                .spec(
                    "Bahn 4 · Knopf 32 × 44, Doppeltrapez (Taille 22) · kompakt 24 × 28 \
                     (Taille 15, 1 Rille je Hälfte) · Pegel 2 × 4, Abstand 1",
                )
                .ix(ix::FADER)
                .content(widgets::fader::gallery)
                .owner("WP-21"),
        ])),
        // Full-width cards, as in the design page.
        Section::new("05", "Spur, Clip, Note")
            .group(Group::full(
                Card::new("Spur-Header")
                    .spec(
                        "224 × 56 (min 32, max 160) · Farbleiste 3 · Toggles M S R I · Pegel 2 × 3",
                    )
                    .ix(ix::HEADER)
                    .content(timeline::gallery::header_card),
            ))
            .group(Group::full(
                Card::new("Clip")
                    .spec(
                        "Höhe = Spur − 6 · Kopf 14 · radius.m · Kopf base, Fläche body, Inhalt L 0.88",
                    )
                    .ix(ix::CLIP)
                    .content(timeline::gallery::clip_card),
            ))
            .group(Group::full(
                Card::new("Note")
                    .spec("Rechteck, Höhe = Tastenhöhe − 2 · radius.s · Helligkeit = Velocity")
                    .ix(ix::NOTE)
                    .content(timeline::gallery::note_card),
            )),
        Section::new("06", "Rahmen und Navigation").group(components(vec![
            Card::new("Tabs und Umschalter")
                .spec("Tab h 28 · Umschalter h 24, Segment h 20")
                .ix(ix::TABS)
                .content(crate::widgets::tabs::gallery)
                .owner("WP-20"),
            Card::new("Panel-Kopf")
                .spec("h 28 · fläche-2 · px 8 · Titel label")
                .ix(ix::PANEL)
                .content(crate::widgets::panel_header::gallery)
                .owner("WP-20"),
            Card::new("Kontextmenü")
                .spec("w 220 · Zeile 24 · p 4 · fläche-3 · shadow.float")
                .ix(ix::MENU)
                .content(crate::widgets::context_menu::gallery)
                .owner("WP-20"),
            Card::new("Tooltip")
                .spec("h 22 · px 8 · fläche-5 · radius.m · shadow.float")
                .ix(ix::TOOLTIP)
                .content(crate::widgets::tooltip::gallery)
                .owner("WP-20"),
        ])),
        tokens::shortcuts_and_icons(),
    ]
}

/// The gallery app.
pub struct Gallery {
    sections: Vec<Section>,
    scroll_to: Option<usize>,
    current: Option<usize>,
}

impl Gallery {
    /// A gallery of `sections`.
    pub fn new(sections: Vec<Section>) -> Self {
        Self {
            sections,
            scroll_to: None,
            current: None,
        }
    }

    /// The gallery of [`designsystem`].
    pub fn designsystem() -> Self {
        Self::new(designsystem())
    }

    /// The sections.
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The section last chosen in the navigation, if any.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Draws the whole page into `ui`: navigation bar on top, the sections below.
    pub fn show(&mut self, ui: &mut Ui) {
        egui::Panel::top("komponist_gallery_nav")
            .exact_size(size::TRANSPORT)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(color32(color::SURFACE_1))
                    .inner_margin(Margin::symmetric(space::SPACE_4 as i8, 0)),
            )
            .show(ui, |ui| self.nav_bar(ui));
        egui::CentralPanel::no_frame()
            .frame(Frame::new().fill(color32(color::SURFACE_0)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.page(ui));
            });
    }

    fn nav_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = space::SPACE_1;
            ui.label(fonts::rich_text("komponist", &typography::TRACK));
            ui.add_space(space::SPACE_1);
            ui.label(fonts::rich_text("Galerie", &typography::SMALL));
            ui.add_space(space::SPACE_6);
            // The scale switch first, so it stays reachable when the bar overflows at 2×.
            let ppp = ui.ctx().pixels_per_point();
            for scale in [1.0_f32, 2.0] {
                let label = format!("{scale}×");
                if ui
                    .add(egui::Button::selectable(ppp == scale, label))
                    .on_hover_text("Maßstab")
                    .clicked()
                {
                    ui.ctx().set_pixels_per_point(scale);
                }
            }
            ui.add_space(space::SPACE_6);
            for (i, section) in self.sections.iter().enumerate() {
                let selected = self.current == Some(i);
                if ui
                    .add(egui::Button::selectable(selected, section.nav_label()))
                    .clicked()
                {
                    self.scroll_to = Some(i);
                    self.current = Some(i);
                }
            }
        });
    }

    fn page(&mut self, ui: &mut Ui) {
        Frame::new()
            .inner_margin(Margin::same(space::SPACE_8 as i8))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = space::SPACE_8;
                tokens::page_header(ui);
                for (i, section) in self.sections.iter().enumerate() {
                    let header = section_header(ui, section);
                    if self.scroll_to == Some(i) {
                        ui.scroll_to_rect(header.rect, Some(Align::TOP));
                        self.scroll_to = None;
                    }
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = space::SPACE_4;
                        for group in &section.groups {
                            show_group(ui, group);
                        }
                    });
                }
            });
    }
}

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// `text` in `style`, with the style's colour.
pub(crate) fn text(ui: &mut Ui, text: &str, style: &TypeStyle) -> Response {
    ui.label(fonts::rich_text(text, style))
}

/// `text` in `style` and `color`.
pub(crate) fn text_in(ui: &mut Ui, text: &str, style: &TypeStyle, color: [u8; 4]) -> Response {
    ui.label(fonts::rich_text(text, style).color(color32(color)))
}

fn section_header(ui: &mut Ui, section: &Section) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_3;
        text_in(ui, section.number, &typography::VALUE_M, color::ACCENT);
        text(ui, section.title, &typography::TITLE);
    })
    .response
}

/// Lays out `group` as a grid; cards in a row share their height.
fn show_group(ui: &mut Ui, group: &Group) {
    let gap = space::SPACE_4;
    let avail = ui.available_width();
    let cols = if group.min_width.is_finite() {
        (((avail + gap) / (group.min_width + gap)).floor() as usize).clamp(1, group.cards.len())
    } else {
        1
    };
    let width = ((avail - gap * (cols - 1) as f32) / cols as f32).floor();
    for row in group.cards.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let mut slots = Vec::with_capacity(row.len());
            let mut height: f32 = 0.0;
            for card in row {
                let bg = ui.painter().add(Shape::Noop);
                let r = ui.allocate_ui_with_layout(
                    vec2(width, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(width);
                        Frame::new()
                            .inner_margin(Margin::same(space::SPACE_4 as i8))
                            .show(ui, |ui| {
                                ui.set_width(width - 2.0 * space::SPACE_4);
                                show_card(ui, card);
                            });
                    },
                );
                height = height.max(r.response.rect.height());
                slots.push((bg, r.response.rect.min));
            }
            for (bg, min) in slots {
                let rect = Rect::from_min_size(min, vec2(width, height));
                ui.painter().set(
                    bg,
                    RectShape::filled(
                        rect,
                        theme::corner_radius(radius::L),
                        color32(color::SURFACE_1),
                    ),
                );
            }
        });
    }
}

fn show_card(ui: &mut Ui, card: &Card) {
    ui.spacing_mut().item_spacing.y = space::SPACE_3;
    let title_style = match card.heading {
        Heading::Title => typography::TRACK,
        Heading::Label => typography::LABEL,
    };
    let avail = ui.available_width();
    let fits = text_width(ui, card.title, &title_style)
        + space::SPACE_4
        + text_width(ui, card.spec, &typography::VALUE_S)
        <= avail;
    if fits {
        ui.horizontal(|ui| {
            text(ui, card.title, &title_style);
            if !card.spec.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    text(ui, card.spec, &typography::VALUE_S);
                });
            }
        });
    } else {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::SPACE_1;
            text(ui, card.title, &title_style);
            text(ui, card.spec, &typography::VALUE_S);
        });
    }
    if !card.text.is_empty() {
        text_in(ui, card.text, &typography::UI, color::TEXT_2);
    }
    match card.content {
        Some(content) => {
            ui.scope(content);
        }
        None if !card.text.is_empty() => {}
        None => {
            let note = if card.owner.is_empty() {
                "folgt".to_owned()
            } else {
                format!("folgt mit {}", card.owner)
            };
            text(ui, &note, &typography::VALUE_S);
        }
    }
    if !card.ix.is_empty() {
        ix_list(ui, card.ix);
    }
}

/// The interaction list under a component: a 1 px line, then key and description per row.
pub fn ix_list(ui: &mut Ui, ix: &[(&str, &str)]) {
    let top = ui.cursor().top();
    let x = ui.max_rect().x_range();
    px::hline(ui.painter(), x, top, color32(color::LINE_1));
    ui.add_space(space::SPACE_3);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = space::SPACE_1;
        for (key, what) in ix {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::SPACE_3;
                cell(ui, page::IX_KEY_WIDTH, |ui| {
                    text_in(ui, key, &typography::VALUE_M, color::TEXT_1);
                });
                let rest = ui.available_width();
                cell(ui, rest, |ui| {
                    text_in(ui, what, &typography::UI, color::TEXT_2);
                });
            });
        }
    });
}

/// A column of exactly `width` in a horizontal layout; text inside wraps at that width.
pub(crate) fn cell<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        add(ui)
    })
    .inner
}

/// Width of `text` set in `style` on one line.
pub(crate) fn text_width(ui: &Ui, text: &str, style: &TypeStyle) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    ui.painter()
        .layout_job(fonts::layout_job(text, style, Color32::PLACEHOLDER))
        .size()
        .x
}

/// A control in the plain state look ([`state::control_look`] on surface.3 with text.1), as
/// used by the "Zustände am Beispiel" card. Not the WP-20 button: it only demonstrates the state
/// helpers. Its accessible name is `name`.
pub fn state_sample(ui: &mut Ui, label: &str, name: &str) -> Response {
    let style = typography::UI;
    let galley = ui
        .painter()
        .layout_job(egui::text::LayoutJob::single_section(
            label.to_owned(),
            egui::TextFormat {
                font_id: fonts::emphasis_font_id(&style),
                line_height: Some(style.line_height),
                color: Color32::PLACEHOLDER,
                ..Default::default()
            },
        ));
    // Button padding "px 10" of the design system: space.2 + space.0.5.
    let pad = space::SPACE_2 + space::SPACE_0_5;
    let size = vec2(galley.size().x + 2.0 * pad, size::CONTROL);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, name));
    let st = State::of(ui, &response);
    let look = state::control_look(3, color::TEXT_1, st);
    let painter = ui.painter();
    px::fill(painter, rect, radius::M, look.fill);
    let ppp = ui.pixels_per_point();
    let pos = rect.center() - galley.size() / 2.0;
    let pos = egui::pos2(px::snap(pos.x, ppp), px::snap(pos.y, ppp));
    painter.galley(pos, galley, look.text);
    if look.focus_ring {
        focus::paint_ring(painter, rect, radius::M);
    }
    response
}

/// The state examples: the sample control in every state of [`State::CONTROL_STATES`] plus
/// selected, each with its label below.
pub fn state_examples(ui: &mut Ui) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        let states = State::CONTROL_STATES
            .iter()
            .copied()
            .chain([("ausgewählt", State::SELECTED)]);
        for (label, st) in states {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = space::SPACE_2;
                state::force(ui, st, |ui| {
                    if st.disabled {
                        ui.disable();
                    }
                    let r = state_sample(ui, "Quantisieren", &format!("Quantisieren · {label}"));
                    if st.selected {
                        state::paint_select_ring(ui.painter(), r.rect, radius::M);
                    }
                });
                text(ui, label, &typography::VALUE_S);
            });
        }
    });
}
