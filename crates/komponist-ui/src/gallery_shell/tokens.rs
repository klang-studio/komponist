//! The token sections of the gallery (01 Farben, 02 Typografie …, 03 Zustandsregeln, 07 Globale
//! Shortcuts) and the interaction lists of `Designsystem.dc.html`.
//!
//! Texts are verbatim from the design system's script (`colorGroups`, `typeScale`, `shapes`,
//! `sizes`, `rules`, `ix`, `shortcuts`); every value shown comes from `komponist-tokens`.

use egui::{Align, Layout, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::track::{self, TrackPalette};
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{Icon, Srgba, color, focus as focus_token, line, radius, size, space};

use super::{
    Card, Group, Section, cell, page, state_examples, state_sample, text, text_in, text_width,
};
use crate::fonts::spec as font_spec;
use crate::icons::{self, Caret};
use crate::theme::{corner_radius, egui_shadow};
use crate::{focus, px, state};

/// The interaction lists (`ix.*`) of the design system.
pub mod ix {
    /// `ix.button`.
    pub const BUTTON: &[(&str, &str)] = &[
        (
            "Klick",
            "Löst die Aktion aus. Enter oder Leertaste, wenn fokussiert.",
        ),
        (
            "Tab / Shift Tab",
            "Fokus vor und zurück innerhalb des Panels.",
        ),
        ("Tooltip", "Nach 600 ms Hover, mit Shortcut."),
    ];
    /// `ix.toggle`.
    pub const TOGGLE: &[(&str, &str)] = &[
        ("Klick", "Schaltet um."),
        ("Alt-Klick", "Solo bzw. Arm exklusiv: nur diese Spur."),
        (
            "Ziehen über Spuren",
            "Überträgt den Zustand auf alle überstrichenen Spuren.",
        ),
        (
            "M · S · R",
            "Für die ausgewählten Spuren, wenn Arrangement fokussiert.",
        ),
        (
            "Implizit stumm",
            "M mit Kontur, Spurname text.3. Kein Klickziel, folgt aus Solo.",
        ),
    ];
    /// `ix.dropdown`.
    pub const DROPDOWN: &[(&str, &str)] = &[
        ("Klick", "Öffnet die Liste unterhalb, aktueller Wert mit ✓."),
        ("↑ ↓ / Enter / Esc", "Bewegen, übernehmen, schließen."),
        ("Tippen", "Springt zum ersten passenden Eintrag."),
        (
            "Mausrad",
            "Über geschlossenem Feld: vorheriger oder nächster Wert.",
        ),
    ];
    /// `ix.number`.
    pub const NUMBER: &[(&str, &str)] = &[
        (
            "Ziehen ↕",
            "1 px = 1 Schritt. Cursor verschwindet, Wert läuft am Feld mit.",
        ),
        ("Shift-Ziehen", "Fein, Schritt × 0,1."),
        ("Strg-Ziehen", "Grob, Schritt × 10."),
        (
            "Doppelklick",
            "Texteingabe, Wert vorausgewählt. Enter übernimmt, Esc verwirft.",
        ),
        ("Alt-Klick", "Setzt den Standardwert."),
        ("↑ ↓ / Mausrad", "Ein Schritt, mit Shift fein."),
    ];
    /// `ix.knob`.
    pub const KNOB: &[(&str, &str)] = &[
        (
            "Ziehen ↕",
            "200 px = voller Bereich. Wert-Tooltip über dem Regler.",
        ),
        ("Shift / Strg", "Fein × 0,1 / grob × 10, wie Zahlenfeld."),
        ("Doppelklick / Alt-Klick", "Standardwert (Pan: Mitte)."),
        (
            "Enter",
            "Texteingabe im Tooltip. Im Programm auch Rechtsklick: „Wert eingeben“.",
        ),
        ("Bipolar", "Bogen läuft von der Mitte aus."),
    ];
    /// `ix.fader`.
    pub const FADER: &[(&str, &str)] = &[
        (
            "Ziehen",
            "Relativ ab Knopf, kein Springen beim Klick auf die Bahn. Wertlinie wird akzent, \
             Wert-Tooltip.",
        ),
        ("Shift", "Fein."),
        ("Doppelklick / Alt-Klick", "0,0 dB."),
        (
            "Enter",
            "Texteingabe in dB. Im Programm auch Rechtsklick: „Wert eingeben“.",
        ),
        ("Doppelklick Wert", "Texteingabe in dB."),
        ("Peak-Hold", "Hält 1,5 s, fällt dann mit 20 dB/s."),
        (
            "Clip-Anzeige",
            "Bleibt rot bis Klick auf Anzeige oder Wert.",
        ),
    ];
    /// `ix.header`.
    pub const HEADER: &[(&str, &str)] = &[
        ("Klick", "Wählt die Spur und zeigt sie im Inspector."),
        (
            "Shift-Klick / Strg-Klick",
            "Bereich wählen / einzeln hinzufügen.",
        ),
        ("Doppelklick Name", "Umbenennen, Enter bestätigt."),
        ("Ziehen", "Spuren umsortieren, Einfügelinie 2 px akzent."),
        (
            "Rechte Kante ziehen",
            "Spurhöhe 32 bis 160 px. Alt-Ziehen: alle Spuren.",
        ),
        (
            "Strg T / Entf",
            "Neue MIDI-Spur / ausgewählte Spuren löschen.",
        ),
        (
            "Instrument ablegen",
            "Drop aus dem Browser auf Header oder leere Fläche.",
        ),
    ];
    /// `ix.clip`.
    pub const CLIP: &[(&str, &str)] = &[
        (
            "Klick / Rahmen ziehen",
            "Auswählen / Rahmenauswahl auf leerer Fläche.",
        ),
        (
            "Ziehen",
            "Verschieben im Raster. Shift: ohne Snap. Alt: Kopie, Original gestrichelt.",
        ),
        ("Kante ziehen", "Kürzen oder verlängern, Länge im Tooltip."),
        (
            "Ecke oben rechts",
            "Loopen: zieht Wiederholungen auf, Grenzen als 1-px-Fuge.",
        ),
        (
            "Doppelklick",
            "Öffnet Piano Roll bzw. Step Sequencer unten. Auf leerer Fläche: neuer Clip, 1 Takt.",
        ),
        (
            "Strg D / Strg E / 0",
            "Duplizieren / am Playhead teilen / Clip stumm.",
        ),
        (
            "Aufnahme",
            "Clip wächst live mit rotem Kopf, Noten erscheinen sofort.",
        ),
    ];
    /// `ix.note`.
    pub const NOTE: &[(&str, &str)] = &[
        (
            "Stift: Klick / Ziehen",
            "Note setzen, Länge = Raster / Länge aufziehen.",
        ),
        ("Zeiger: Doppelklick", "Note setzen oder löschen."),
        ("Ziehen", "Zeit und Tonhöhe. Shift: ohne Snap. Alt: Kopie."),
        (
            "Kante ziehen",
            "Länge ändern, gilt für alle ausgewählten Noten.",
        ),
        (
            "Strg-Ziehen ↕ auf Note",
            "Velocity ändern, Wert im Tooltip.",
        ),
        ("↑ ↓ / Shift ↑ ↓", "Halbton / Oktave."),
        ("← → / Q", "Ein Rasterschritt / quantisieren."),
        ("Strg A / Entf", "Alle wählen / löschen."),
    ];
    /// `ix.tabs`.
    pub const TABS: &[(&str, &str)] = &[
        ("Klick", "Wechselt die Ansicht im selben Bereich."),
        ("Strg Tab", "Nächster Tab im fokussierten Bereich."),
        ("Tab (Taste)", "Arrangement ↔ Mixer."),
    ];
    /// `ix.panel`.
    pub const PANEL: &[(&str, &str)] = &[
        ("Klick auf Kopf", "Klappt ein und aus."),
        ("Strg 1 · 2 · 3", "Browser · Inspector · Editor ein/aus."),
        (
            "Kante ziehen",
            "Breite oder Höhe ändern, Doppelklick: Standardmaß.",
        ),
    ];
    /// `ix.menu`.
    pub const MENU: &[(&str, &str)] = &[
        ("Rechtsklick", "Öffnet am Cursor, Inhalt abhängig vom Ziel."),
        (
            "↑ ↓ → / Enter / Esc",
            "Navigieren, Untermenü öffnen, ausführen, schließen.",
        ),
        (
            "Shortcuts",
            "Stehen rechts in text.3, damit man sie nebenbei lernt.",
        ),
    ];
    /// `ix.tooltip`.
    pub const TOOLTIP: &[(&str, &str)] = &[
        ("Hover 600 ms", "Erscheint unter dem Element, 6 px Abstand."),
        ("Ziehen", "Werte-Tooltip sofort, folgt dem Wert."),
        (
            "Weiter hovern",
            "Nachbar-Tooltips erscheinen ohne Verzögerung.",
        ),
    ];
}

/// Presentation sizes of the design page used only here (bars and demo shapes of the token
/// cards, from the markup of `Designsystem.dc.html` and the transport of `ks-view.js`).
mod demo {
    /// Track colour rows: base, body, dim, velocity steps.
    pub const TRACK_BARS: [f32; 4] = [28.0, 18.0, 10.0, 8.0];
    /// Velocities of the five note steps per hue.
    pub const VELOCITY_STEPS: [u8; 5] = [20, 50, 80, 105, 127];
    /// Spacing bars: width = value × 4, height 8.
    pub const SPACE_BAR: [f32; 2] = [4.0, 8.0];
    /// Max width of the intro paragraph.
    pub const INTRO_WIDTH: f32 = 860.0;
    /// Play triangle (9 × 12), record dot (Ø 10), caret (8 × 5).
    pub const PLAY: [f32; 2] = [9.0, 12.0];
    /// Record dot diameter.
    pub const RECORD: f32 = 10.0;
    /// Caret ▾ width and height.
    pub const CARET: [f32; 2] = [8.0, 5.0];
    /// Minimum column width of the size and shortcut tables.
    pub const TABLE_COLUMN: f32 = 240.0;
    /// Minimum column width of the shortcut table.
    pub const SHORTCUT_COLUMN: f32 = 300.0;
}

/// German names of the 14 track hues, in the order of `track::HUES` (`HUES` of the design).
pub const HUE_NAMES: [&str; 14] = [
    "Rot", "Zinnober", "Orange", "Gelb", "Limette", "Grün", "Smaragd", "Petrol", "Cyan", "Blau",
    "Indigo", "Violett", "Magenta", "Rosa",
];

/// `#rrggbb` of a token colour.
pub fn hex(c: Srgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The header of the page.
pub fn page_header(ui: &mut Ui) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::SPACE_2;
        ui.set_max_width(demo::INTRO_WIDTH);
        text(
            ui,
            "komponist · Designsystem · Richtung Graphit · v0.1",
            &typography::LABEL,
        );
        text(ui, "Tokens und Komponenten", &typography::TITLE);
        text_in(
            ui,
            "Alle Maße in px bei 1×. Farben als Hex für das dunkle Theme; die Token-Namen sind \
             themenneutral, damit ein helles Theme dieselben Namen mit anderen Werten belegen \
             kann. Jede Komponente zeigt alle Zustände und darunter, was eine Interaktion \
             auslöst.",
            &typography::UI,
            color::TEXT_2,
        );
    });
}

// ---------------------------------------------------------------------------------------------
// 01 Farben

#[derive(Clone, Copy)]
enum Swatch {
    Color(Srgba),
    ShadowFloat,
    ShadowEdge,
}

type ColorRow = (&'static str, Swatch, &'static str);

const SURFACES: &[ColorRow] = &[
    (
        "surface.0",
        Swatch::Color(color::SURFACE_0),
        "Fensterhintergrund, Fugen zwischen Panels, Pegel-Bahn",
    ),
    (
        "surface.1",
        Swatch::Color(color::SURFACE_1),
        "Panels, Timeline, Piano-Roll-Raster",
    ),
    (
        "surface.2",
        Swatch::Color(color::SURFACE_2),
        "Spur-Header, Panel-Kopf, Eingabefelder",
    ),
    (
        "surface.3",
        Swatch::Color(color::SURFACE_3),
        "Buttons, Menüs, Hover auf Spur-Header",
    ),
    (
        "surface.4",
        Swatch::Color(color::SURFACE_4),
        "Hover auf Controls, Griffe",
    ),
    (
        "surface.5",
        Swatch::Color(color::SURFACE_5),
        "Gedrückt, Tooltip",
    ),
];

const TEXT_AND_LINES: &[ColorRow] = &[
    ("text.1", Swatch::Color(color::TEXT_1), "Primärtext, Werte"),
    ("text.2", Swatch::Color(color::TEXT_2), "Sekundär, Icons"),
    (
        "text.3",
        Swatch::Color(color::TEXT_3),
        "Labels, Einheiten, Hinweise",
    ),
    ("text.4", Swatch::Color(color::TEXT_4), "Deaktiviert"),
    (
        "line.1",
        Swatch::Color(color::LINE_1),
        "Trenner, Schlaglinien",
    ),
    (
        "line.2",
        Swatch::Color(color::LINE_2),
        "Taktlinien, Oktavlinien",
    ),
];

const ACCENT: &[ColorRow] = &[
    (
        "accent",
        Swatch::Color(color::ACCENT),
        "Auswahl-Kontur, Fokusring, Loop-Bereich",
    ),
    (
        "accent.hover",
        Swatch::Color(color::ACCENT_HOVER),
        "Hover auf Primär-Button",
    ),
    (
        "accent.surface",
        Swatch::Color(color::ACCENT_SURFACE),
        "Ausgewählte Zeilen und Spur-Header",
    ),
    (
        "accent.ink",
        Swatch::Color(color::ACCENT_INK),
        "Text auf Akzentfläche",
    ),
];

const STATES: &[ColorRow] = &[
    (
        "state.record",
        Swatch::Color(color::STATE_RECORD),
        "Record, Arm, laufende Aufnahme",
    ),
    (
        "state.record.surface",
        Swatch::Color(color::STATE_RECORD_SURFACE),
        "Fläche des Aufnahme-Clips",
    ),
    ("state.solo", Swatch::Color(color::STATE_SOLO), "Solo aktiv"),
    (
        "state.play",
        Swatch::Color(color::STATE_PLAY),
        "Wiedergabe läuft",
    ),
    (
        "state.mute",
        Swatch::Color(color::STATE_MUTE),
        "Mute aktiv (neutral)",
    ),
    (
        "state.error",
        Swatch::Color(color::STATE_ERROR),
        "Fehler: Gerät fehlt, Plugin abgestürzt",
    ),
];

const METERS: &[ColorRow] = &[
    ("meter.ok", Swatch::Color(color::METER_OK), "bis −6 dBFS"),
    (
        "meter.loud",
        Swatch::Color(color::METER_LOUD),
        "−6 bis 0 dBFS",
    ),
    (
        "meter.clip",
        Swatch::Color(color::METER_CLIP),
        "über 0 dBFS, Clip-Anzeige",
    ),
    (
        "meter.peak",
        Swatch::Color(color::METER_PEAK),
        "Peak-Hold-Linie unter −6 dBFS",
    ),
    ("meter.track", Swatch::Color(color::METER_TRACK), "Bahn"),
];

const SHADOWS: &[ColorRow] = &[
    (
        "shadow.float",
        Swatch::ShadowFloat,
        "Menüs, Dropdown-Listen, Tooltips, Plugin-Fenster",
    ),
    (
        "shadow.edge",
        Swatch::ShadowEdge,
        "1-px-Kante um schwebende Flächen",
    ),
];

fn swatch_value(swatch: Swatch) -> String {
    use komponist_tokens::shadow;
    match swatch {
        Swatch::Color(c) => hex(c),
        Swatch::ShadowFloat => {
            let s = shadow::FLOAT;
            format!(
                "{} {} {} / {}%",
                s.offset[0],
                s.offset[1],
                s.blur,
                (f32::from(s.color[3]) / 255.0 * 100.0).round()
            )
        }
        Swatch::ShadowEdge => hex(shadow::EDGE.color),
    }
}

fn paint_swatch(ui: &mut Ui, swatch: Swatch) {
    use komponist_tokens::shadow;
    let (rect, _) = ui.allocate_exact_size(vec2(space::SPACE_8, size::CONTROL), Sense::hover());
    let painter = ui.painter();
    let ppp = ui.pixels_per_point();
    let r = corner_radius(radius::M);
    let inset = Stroke::new(px::line_width(ppp), color32(color::LINE_1));
    match swatch {
        Swatch::Color(c) => {
            px::fill(painter, rect, radius::M, color32(c));
            painter.rect_stroke(px::snap_rect(rect, ppp), r, inset, StrokeKind::Inside);
        }
        Swatch::ShadowFloat => {
            let inner = rect.shrink(space::SPACE_1);
            painter.add(egui_shadow(&shadow::FLOAT).as_shape(inner, r));
            px::fill(painter, inner, radius::M, color32(color::SURFACE_3));
        }
        Swatch::ShadowEdge => {
            let inner = rect.shrink(space::SPACE_1);
            px::fill(painter, inner, radius::M, color32(color::SURFACE_3));
            px::outline(painter, inner, radius::M, 0.0, color32(shadow::EDGE.color));
        }
    }
}

fn color_rows(ui: &mut Ui, rows: &[ColorRow]) {
    let gap = space::SPACE_3;
    let token_w = rows
        .iter()
        .map(|r| text_width(ui, r.0, &typography::VALUE_M))
        .fold(0.0, f32::max)
        .ceil();
    let value_w = rows
        .iter()
        .map(|r| text_width(ui, &swatch_value(r.1), &typography::VALUE_S))
        .fold(0.0, f32::max)
        .ceil();
    ui.spacing_mut().item_spacing.y = space::SPACE_1_5;
    for &(token, swatch, used) in rows {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            paint_swatch(ui, swatch);
            let centred = |ui: &mut Ui, w: f32, add: &dyn Fn(&mut Ui)| {
                ui.allocate_ui_with_layout(
                    vec2(w, size::CONTROL),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_size(vec2(w, size::CONTROL));
                        add(ui);
                    },
                );
            };
            centred(ui, token_w, &|ui| {
                text_in(ui, token, &typography::VALUE_M, color::TEXT_1);
            });
            centred(ui, value_w, &|ui| {
                text_in(
                    ui,
                    &swatch_value(swatch),
                    &typography::VALUE_S,
                    color::TEXT_2,
                );
            });
            let rest = ui.available_width();
            cell(ui, rest, |ui| {
                // Centred on the swatch, also when the text wraps to two lines.
                let mut job =
                    crate::fonts::layout_job(used, &typography::SMALL, color32(color::TEXT_3));
                job.wrap.max_width = rest;
                let height = ui.painter().layout_job(job).size().y;
                ui.add_space(((size::CONTROL - height) / 2.0).max(0.0).floor());
                text_in(ui, used, &typography::SMALL, color::TEXT_3);
            });
        });
    }
}

fn surfaces(ui: &mut Ui) {
    color_rows(ui, SURFACES);
}
fn text_and_lines(ui: &mut Ui) {
    color_rows(ui, TEXT_AND_LINES);
}
fn accent(ui: &mut Ui) {
    color_rows(ui, ACCENT);
}
fn states(ui: &mut Ui) {
    color_rows(ui, STATES);
}
fn meters(ui: &mut Ui) {
    color_rows(ui, METERS);
}
fn shadows(ui: &mut Ui) {
    color_rows(ui, SHADOWS);
}

/// The 14 track hues: base, body, dim and five velocity steps per hue.
pub fn track_colors(ui: &mut Ui) {
    use track::formula as f;
    text(
        ui,
        &format!(
            "base L {:.2} C {:.2} · body L {:.2} C {:.2} · dim L {:.2} C {:.2} · note L {:.2} + \
             {:.2} × vel/127",
            f::BASE.l,
            f::BASE.c,
            f::BODY.l,
            f::BODY.c,
            f::DIM.l,
            f::DIM.c,
            f::NOTE.l_min,
            f::NOTE.l_range
        ),
        &typography::VALUE_S,
    );
    let gap = space::SPACE_2;
    let n = track::HUES.len() as f32;
    let w = ((ui.available_width() - gap * (n - 1.0)) / n).floor();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (hue, name) in track::HUES.iter().zip(HUE_NAMES) {
            let p = TrackPalette::new(hue.degrees);
            ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.set_width(w);
                ui.spacing_mut().item_spacing.y = space::SPACE_1;
                for (c, h) in [p.base, p.body, p.dim].into_iter().zip(demo::TRACK_BARS) {
                    let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
                    px::fill(ui.painter(), r, radius::S, color32(c));
                }
                let (r, _) = ui.allocate_exact_size(vec2(w, demo::TRACK_BARS[3]), Sense::hover());
                let steps = demo::VELOCITY_STEPS.len() as f32;
                let sw = (r.width() - (steps - 1.0) * line::WIDTH) / steps;
                for (i, v) in demo::VELOCITY_STEPS.into_iter().enumerate() {
                    let x = r.left() + i as f32 * (sw + line::WIDTH);
                    let step = Rect::from_min_size(egui::pos2(x, r.top()), vec2(sw, r.height()));
                    px::fill(ui.painter(), step, 0.0, color32(p.note(v)));
                }
                text_in(ui, name, &typography::SMALL, color::TEXT_1);
                text(ui, &hex(p.base), &typography::VALUE_S);
                text(ui, &format!("h {}", hue.degrees), &typography::VALUE_S);
            });
        }
    });
    text_in(
        ui,
        "Zeilen: base (Spur-Leiste, Clip-Kopf) · body (Clip-Fläche) · dim (stumm) · \
         Velocity-Verlauf der Noten in 5 Stufen",
        &typography::SMALL,
        color::TEXT_3,
    );
}

/// 01 Farben.
pub fn colors() -> Section {
    Section::new("01", "Farben")
        .group(Group::grid(
            page::CARDS_COLORS,
            vec![
                Card::label("Flächen").content(surfaces),
                Card::label("Text und Linien").content(text_and_lines),
                Card::label("Akzent · nur Auswahl, Fokus, Loop").content(accent),
                Card::label("Zustände").content(states),
                Card::label("Pegel").content(meters),
                Card::label("Schatten · nur schwebend").content(shadows),
            ],
        ))
        .group(Group::full(
            Card::label("Spurfarben · 14 · oklch, gleiche Helligkeit und Chroma")
                .content(track_colors),
        ))
}

// ---------------------------------------------------------------------------------------------
// 02 Typografie, Abstände, Radien, Linien, Maße

const TYPE_SAMPLES: [(&str, TypeStyle, &str); 8] = [
    ("type.title", typography::TITLE, "WAV exportieren"),
    ("type.track", typography::TRACK, "Lead · Drums · Pad"),
    ("type.ui", typography::UI, "Quantisieren auf 1/16"),
    ("type.small", typography::SMALL, "pisynth · 6-Operator-FM"),
    (
        "type.label",
        typography::LABEL,
        "Raster · Snap · Instrument",
    ),
    ("type.value.l", typography::VALUE_L, "17.3.240  00:41.250"),
    ("type.value.m", typography::VALUE_M, "124.00  −3.2 dB  C#4"),
    (
        "type.value.s",
        typography::VALUE_S,
        "9  10  11  12  −12  −24",
    ),
];

fn type_scale(ui: &mut Ui) {
    for (i, (token, style, sample)) in TYPE_SAMPLES.iter().enumerate() {
        if i > 0 {
            let y = ui.cursor().top();
            px::hline(
                ui.painter(),
                ui.max_rect().x_range(),
                y,
                color32(color::LINE_1),
            );
            ui.add_space(space::SPACE_1);
        }
        ui.horizontal_top(|ui| {
            cell(ui, page::IX_KEY_WIDTH, |ui| {
                text_in(ui, token, &typography::VALUE_M, color::TEXT_1);
            });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = space::SPACE_0_5;
                text(ui, sample, style);
                text(ui, &font_spec(style), &typography::VALUE_S);
            });
        });
    }
}

fn spacing(ui: &mut Ui) {
    let scale: [(&str, f32); 8] = [
        ("space.0.5", space::SPACE_0_5),
        ("space.1", space::SPACE_1),
        ("space.1.5", space::SPACE_1_5),
        ("space.2", space::SPACE_2),
        ("space.3", space::SPACE_3),
        ("space.4", space::SPACE_4),
        ("space.6", space::SPACE_6),
        ("space.8", space::SPACE_8),
    ];
    egui::Grid::new("spacing")
        .num_columns(3)
        .spacing(vec2(space::SPACE_3, space::SPACE_1_5))
        .show(ui, |ui| {
            for (token, v) in scale {
                text_in(ui, token, &typography::VALUE_M, color::TEXT_1);
                text(ui, &format!("{v} px"), &typography::VALUE_S);
                let (r, _) = ui.allocate_exact_size(
                    vec2(v * demo::SPACE_BAR[0], demo::SPACE_BAR[1]),
                    Sense::hover(),
                );
                px::fill(ui.painter(), r, 0.0, color32(color::SURFACE_4));
                ui.end_row();
            }
        });
}

/// The shapes card: radii, the 1 px line, the focus ring and the selection ring.
pub fn shape_examples(ui: &mut Ui) {
    let demo = vec2(space::SPACE_8, size::CONTROL_COMPACT);
    type Paint = fn(&egui::Painter, Rect);
    let rows: [(String, &str, Vec2, Paint); 6] = [
        (
            format!("radius.s · {}", radius::S),
            "Noten, Spur-Toggles, Segmente",
            demo,
            |p, r| px::fill(p, r, radius::S, color32(color::SURFACE_4)),
        ),
        (
            format!("radius.m · {}", radius::M),
            "Buttons, Felder, Clips, Tooltips",
            demo,
            |p, r| px::fill(p, r, radius::M, color32(color::SURFACE_4)),
        ),
        (
            format!("radius.l · {}", radius::L),
            "Menüs, Dialoge, Plugin-Fenster",
            demo,
            |p, r| px::fill(p, r, radius::L, color32(color::SURFACE_4)),
        ),
        (
            format!("line · {}", line::WIDTH),
            "Alle Linien sind 1 px; bei 2× 2 Gerätepixel",
            vec2(space::SPACE_8, line::WIDTH),
            |p, r| px::hline(p, r.x_range(), r.top(), color32(color::LINE_2)),
        ),
        (
            format!(
                "focus · {} + {}",
                focus_token::OUTLINE_WIDTH,
                focus_token::OFFSET
            ),
            "Akzent-Kontur 1 px mit 1 px Abstand",
            vec2(size::SWITCH[0], size::SWITCH[1]),
            |p, r| {
                px::fill(p, r, radius::M, color32(color::SURFACE_3));
                focus::paint_ring(p, r, radius::M);
            },
        ),
        (
            format!("select · {}", komponist_tokens::select::RING.spread),
            "Akzent-Kontur 1 px direkt am Objekt",
            vec2(space::SPACE_8, size::TOGGLE[1]),
            |p, r| {
                px::fill(p, r, radius::M, color32(TrackPalette::new(150).body));
                state::paint_select_ring(p, r, radius::M);
            },
        ),
    ];
    let token_w = rows
        .iter()
        .map(|r| text_width(ui, &r.0, &typography::VALUE_M))
        .fold(0.0, f32::max)
        .ceil();
    let demo_w = space::SPACE_8 + 2.0 * space::SPACE_1;
    ui.spacing_mut().item_spacing.y = space::SPACE_2;
    for (token, used, size, paint) in &rows {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::SPACE_4;
            let (r, _) =
                ui.allocate_exact_size(vec2(demo_w, size::CONTROL_COMPACT), Sense::hover());
            paint(ui.painter(), Rect::from_center_size(r.center(), *size));
            for (w, t, style, c) in [
                (token_w, token.as_str(), typography::VALUE_M, color::TEXT_1),
                (
                    ui.available_width() - token_w - space::SPACE_4,
                    *used,
                    typography::SMALL,
                    color::TEXT_3,
                ),
            ] {
                ui.allocate_ui_with_layout(
                    vec2(w, size::CONTROL_COMPACT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_size(vec2(w, size::CONTROL_COMPACT));
                        text_in(ui, t, &style, c);
                    },
                );
            }
        });
    }
}

/// A table of `(name, value)` cells in columns of at least `min_width`, each cell with a 1 px
/// line on top: name left in `type.ui` text.2, value right in mono text.1.
fn table(ui: &mut Ui, min_width: f32, cells: &[(String, String)]) {
    let gap = space::SPACE_6;
    let avail = ui.available_width();
    let cols = (((avail + gap) / (min_width + gap)).floor() as usize).max(1);
    let w = ((avail - gap * (cols - 1) as f32) / cols as f32).floor();
    ui.spacing_mut().item_spacing.y = 0.0;
    for row in cells.chunks(cols) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (name, value) in row {
                ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(w);
                    let top = ui.cursor().top();
                    px::hline(
                        ui.painter(),
                        ui.max_rect().x_range(),
                        top,
                        color32(color::LINE_1),
                    );
                    ui.add_space(space::SPACE_1_5);
                    ui.horizontal(|ui| {
                        text_in(ui, name, &typography::UI, color::TEXT_2);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            text_in(ui, value, &typography::VALUE_M, color::TEXT_1);
                        });
                    });
                    ui.add_space(space::SPACE_1_5);
                });
            }
        });
    }
}

fn range(r: [f32; 3]) -> String {
    format!("{} · {}–{}", r[0], r[1], r[2])
}

fn sizes(ui: &mut Ui) {
    use size::{browser, dock, inspector, key, track};
    let cells: Vec<(&str, String)> = vec![
        ("Transportleiste", size::TRANSPORT.to_string()),
        ("Panel-Kopf", size::PANEL_HEADER.to_string()),
        ("Tab", size::TAB.to_string()),
        ("Lineal", size::RULER.to_string()),
        ("Control", size::CONTROL.to_string()),
        ("Control kompakt", size::CONTROL_COMPACT.to_string()),
        ("Menüzeile", size::MENU_ROW.to_string()),
        ("Spur", range([track::DEFAULT, track::MIN, track::MAX])),
        ("Spur-Header", size::TRACK_HEADER.to_string()),
        (
            "Seitenleiste / Browser",
            range([browser::DEFAULT, browser::MIN, browser::MAX]),
        ),
        (
            "Inspector",
            range([inspector::DEFAULT, inspector::MIN, inspector::MAX]),
        ),
        (
            "Editor unten",
            format!("{} · min {}", dock::DEFAULT, dock::MIN),
        ),
        ("Klaviatur", size::KEYBOARD.to_string()),
        ("Taste", range([key::DEFAULT, key::MIN, key::MAX])),
        ("Velocity-Lane", size::VELOCITY_LANE.to_string()),
        ("Kanalzug Mixer", size::MIXER_STRIP.to_string()),
        ("Icon", size::ICON.to_string()),
        ("Statuszeile", size::STATUSBAR.to_string()),
        ("Clip-Kopf", size::CLIP_HEADER.to_string()),
        ("Menübreite", size::MENU_WIDTH.to_string()),
    ];
    let cells: Vec<(String, String)> = cells
        .into_iter()
        .map(|(n, v)| (n.to_owned(), format!("{v} px")))
        .collect();
    table(ui, demo::TABLE_COLUMN, &cells);
}

/// 02 Typografie, Abstände, Radien, Linien, Maße.
pub fn typography_and_sizes() -> Section {
    Section::new("02", "Typografie, Abstände, Radien, Linien, Maße")
        .group(Group::grid(
            page::CARDS_COMPONENTS,
            vec![
                Card::label(
                    "Schriftstufen · IBM Plex Sans / IBM Plex Mono (OFL), Zahlen immer mit tnum",
                )
                .content(type_scale),
                Card::label("Abstände · space.*").content(spacing),
                Card::label("Radien, Linien, Schatten").content(shape_examples),
            ],
        ))
        .group(Group::full(
            Card::label("Feste Maße · size.*").content(sizes),
        ))
}

// ---------------------------------------------------------------------------------------------
// 03 Zustandsregeln

const RULES: [(&str, &str); 6] = [
    (
        "Hover",
        "Fläche steigt eine Stufe (surface.3 → surface.4), Text wechselt von text.2 auf text.1. \
         Keine Rahmen, keine Animation. Cursor zeigt die mögliche Aktion (Ziehen, Kürzen, \
         Zeichnen).",
    ),
    (
        "Gedrückt",
        "Fläche steigt zwei Stufen (surface.5). Bei ziehbaren Werten erscheint sofort der \
         Werte-Tooltip.",
    ),
    (
        "Fokus (Tastatur)",
        "1 px Akzent-Kontur mit 1 px Abstand. Das fokussierte Panel trägt zusätzlich eine \
         1-px-Akzentlinie oben im Kopf. Fokus ist nur bei Tastaturbedienung als Ring sichtbar, \
         das Panel-Signal immer.",
    ),
    (
        "Auswahl",
        "Objekte auf der Zeitachse (Clips, Noten) bekommen eine 1-px-Akzent-Kontur und behalten \
         ihre Farbe. Zeilen (Spuren, Listen) bekommen accent.surface als Fläche.",
    ),
    (
        "Mehrfachauswahl",
        "Gleiche Darstellung für jedes Element. Die Anzahl steht in der Statuszeile und im \
         Inspector („3 Clips“). Keine eigene Farbe für das zuletzt gewählte Element.",
    ),
    (
        "Deaktiviert",
        "surface.2 und text.4, kein Hover, kein Cursorwechsel. Werte bleiben lesbar, damit klar \
         ist, was eingestellt wäre.",
    ),
];

fn live_state_example(ui: &mut Ui) {
    let id = egui::Id::unique("komponist_gallery_state_clicks");
    state_examples(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_3;
        let r = state_sample(ui, "Quantisieren", "Quantisieren");
        let clicks: u32 = ui.data(|d| d.get_temp(id)).unwrap_or(0);
        let clicks = clicks + u32::from(r.clicked());
        ui.data_mut(|d| d.insert_temp(id, clicks));
        let st = state::State::of(ui, &r);
        text(
            ui,
            &format!("live · {} · Klicks {clicks}", st.slug()),
            &typography::VALUE_S,
        );
    });
    text_in(
        ui,
        "Tab fokussiert, der Ring erscheint nur nach Tastaturbedienung.",
        &typography::SMALL,
        color::TEXT_3,
    );
}

/// 03 Zustandsregeln.
pub fn state_rules() -> Section {
    let rules = RULES
        .iter()
        .map(|&(name, rule)| Card::new(name).text(rule))
        .collect();
    Section::new("03", "Zustandsregeln")
        .group(Group::grid(page::CARDS_RULES, rules))
        .group(Group::full(
            Card::new("Zustände am Beispiel")
                .spec("state::control_look · focus::paint_ring · state::paint_select_ring")
                .content(live_state_example),
        ))
}

// ---------------------------------------------------------------------------------------------
// 07 Globale Shortcuts

const SHORTCUTS: [(&str, &str); 18] = [
    ("Leertaste", "Play / Stop"),
    ("R", "Aufnahme"),
    ("L", "Loop an/aus"),
    ("C", "Metronom"),
    ("Pos1", "Zum Anfang"),
    ("Tab", "Arrangement ↔ Mixer"),
    ("Strg T", "Neue MIDI-Spur"),
    ("Strg N / O / S", "Neu / Öffnen / Speichern"),
    ("Strg Shift E", "WAV exportieren"),
    ("Strg 1 · 2 · 3", "Browser · Inspector · Editor"),
    ("Strg F", "Browser-Suche"),
    ("+ / −", "Horizontal zoomen"),
    ("Alt + / Alt −", "Vertikal zoomen"),
    ("Z", "Auswahl einpassen"),
    ("Strg [ / Strg ]", "Raster feiner / gröber"),
    ("Strg Z / Strg Shift Z", "Rückgängig / Wiederholen"),
    ("Strg ,", "Einstellungen"),
    ("Esc", "Auswahl aufheben"),
];

fn shortcuts(ui: &mut Ui) {
    let cells: Vec<(String, String)> = SHORTCUTS
        .iter()
        .map(|(k, v)| ((*v).to_owned(), (*k).to_owned()))
        .collect();
    table(ui, demo::SHORTCUT_COLUMN, &cells);
}

/// All icons and the drawn transport symbols, tinted text.2.
pub fn icon_examples(ui: &mut Ui) {
    let tint = color32(color::TEXT_2);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(space::SPACE_4, space::SPACE_3);
        for &icon in Icon::ALL {
            icon_cell(ui, icon.name(), |ui, r| icons::paint(ui, r, icon, tint));
        }
        type PaintIn = fn(&Ui, Rect);
        let shapes: [(&str, PaintIn); 5] = [
            ("play", |ui, r| {
                let s = Rect::from_center_size(r.center(), demo::PLAY.into());
                icons::paint_play(ui.painter(), s, color32(color::TEXT_2));
            }),
            ("stop", |ui, r| {
                let s = Rect::from_center_size(r.center(), Vec2::splat(demo::RECORD));
                icons::paint_stop(ui.painter(), s, color32(color::TEXT_2));
            }),
            ("record", |ui, r| {
                let s = Rect::from_center_size(r.center(), Vec2::splat(demo::RECORD));
                icons::paint_record(ui.painter(), s, color32(color::STATE_RECORD));
            }),
            ("caret-down", |ui, r| {
                let s = Rect::from_center_size(r.center(), demo::CARET.into());
                icons::paint_caret(ui.painter(), s, Caret::Down, color32(color::TEXT_2));
            }),
            ("caret-right", |ui, r| {
                let s = Rect::from_center_size(r.center(), vec2(demo::CARET[1], demo::CARET[0]));
                icons::paint_caret(ui.painter(), s, Caret::Right, color32(color::TEXT_2));
            }),
        ];
        for (name, paint) in shapes {
            icon_cell(ui, name, paint);
        }
    });
}

fn icon_cell(ui: &mut Ui, name: &str, paint: impl FnOnce(&Ui, Rect)) {
    // A fixed cell size, so the wrapping row knows when to break.
    let w = text_width(ui, name, &typography::VALUE_S)
        .max(size::CONTROL)
        .ceil();
    let h = size::CONTROL + space::SPACE_1 + typography::VALUE_S.line_height;
    ui.allocate_ui_with_layout(vec2(w, h), Layout::top_down(Align::Min), |ui| {
        ui.set_min_size(vec2(w, h));
        ui.spacing_mut().item_spacing.y = space::SPACE_1;
        let (r, _) = ui.allocate_exact_size(Vec2::splat(size::CONTROL), Sense::hover());
        px::fill(ui.painter(), r, radius::M, color32(color::SURFACE_2));
        paint(ui, r);
        text(ui, name, &typography::VALUE_S);
    });
}

fn icon_note(ui: &mut Ui) {
    text_in(
        ui,
        "Icons: Vorschlag Lucide (ISC-Lizenz) auf 16-px-Raster mit 1,5 px Strich, als SVG \
         lieferbar. Transport-Symbole (Play, Stop, Record) bleiben geometrische Grundformen. Das \
         Icon-Set lege ich mit den Screens fest.",
        &typography::UI,
        color::TEXT_3,
    );
    icon_examples(ui);
}

/// 07 Globale Shortcuts, with the icon set.
pub fn shortcuts_and_icons() -> Section {
    Section::new("07", "Globale Shortcuts")
        .group(Group::full(
            Card::label("Globale Shortcuts").content(shortcuts),
        ))
        .group(Group::full(
            Card::label("Icons · Lucide 16 px · Transport als Grundformen").content(icon_note),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_names_match_the_token_hues() {
        assert_eq!(HUE_NAMES.len(), track::HUES.len());
        assert_eq!(track::HUES[0].degrees, 25);
        assert_eq!(track::HUES[13].degrees, 350);
    }

    #[test]
    fn values_come_from_tokens() {
        assert_eq!(hex(color::ACCENT), "#4ea7e6");
        assert_eq!(swatch_value(Swatch::ShadowFloat), "0 6 16 / 45%");
        assert_eq!(swatch_value(Swatch::ShadowEdge), "#111214");
        assert_eq!(range([56.0, 32.0, 160.0]), "56 · 32–160");
    }
}
