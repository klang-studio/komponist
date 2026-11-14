//! The gallery page „Geräte-Widgets“: every device widget in every state, laid out like the
//! widget catalogue of the device handoff (`Geraete.dc.html` section 02): on a `surface.2` ground
//! in original size, captions under the samples, then a live row per card.

use egui::{Align, Frame, Id, Layout, Margin, Ui, vec2};
use komponist_model::format::MINUS;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, radius, space};

use super::choice::{DropdownCell, Glyph, Segment, SegmentOption, ToggleButton, Wave};
use super::fm_algorithm::{FmAlgorithm, Topology};
use super::knob_cell::KnobCell;
use super::meter_gr::{self, MeterGr};
use super::mini_fader::{self, Fill, MiniFader};
use super::number::{self, Number};
use super::pad_grid::{Pad, PadGrid};
use super::{KnobCellOptions, LABEL, cell_width};
use crate::gallery_shell::{Card, Group, Section, page};
use crate::state::{self, State};
use crate::theme::corner_radius;
use crate::widgets::dropdown::Item;
use crate::widgets::knob::ModArc;
use crate::widgets::number_field::column;
use crate::{fonts, widgets};

/// Hues of the handoff's sample tracks (`LEAD`, `DRUM`, `BASS`).
const LEAD: u16 = 150;
const DRUM: u16 = 45;

fn palette(hue: u16) -> TrackPalette {
    TrackPalette::new(hue)
}

/// The section of the gallery.
pub fn section() -> Section {
    Section::new("07", "Geräte-Widgets").group(Group::grid(
        page::CARDS_COMPONENTS,
        vec![
            Card::new("Drehregler")
                .spec("1 × 1 · Ø 28")
                .ix(&[
                    ("Ziehen ↕", "200 px = voller Bereich, Shift fein, Strg grob, Alt-Klick Standard."),
                    ("Modulation", "Äußerer 1,5-px-Bogen in Spurfarbe zeigt den Modulationsbereich."),
                    ("Automation", "Punkt in Spurfarbe vor dem Namen. Wert folgt der Kurve."),
                ])
                .content(knob_card),
            Card::new("Drehregler groß")
                .spec("2 × 1 · Ø 44")
                .ix(&[
                    ("Einsatz", "Ein bis zwei Hauptparameter je Gerät: Cutoff, Threshold, Größe, Mix."),
                    ("Wert", "value.m rechts neben dem Regler, immer sichtbar."),
                ])
                .content(knob_large_card),
            Card::new("Zahlenfeld kompakt")
                .spec("½ Zelle · h 18")
                .ix(&[
                    ("Bedienung", "Wie Zahlenfeld im Designsystem: 1 px = 1 Schritt, Doppelklick tippt."),
                    ("Für", "Ganzzahlen und exakte Werte: Stimmen, Choke-Gruppe, Start, Ende."),
                ])
                .content(number_card),
            Card::new("Umschalter")
                .spec("½ Zelle × n · h 18")
                .ix(&[
                    ("Klick / Mausrad", "Wählt Segment / blättert. ← → bei Fokus."),
                    ("Für", "Diskrete Auswahl mit 2 bis 6 Werten. Mehr Werte: Dropdown."),
                ])
                .content(segment_card),
            Card::new("Schalt-Button")
                .spec("½ Zelle · h 18")
                .ix(&[
                    ("An", "Fläche text.1, Schrift surface.0. Bewusst nicht akzent, da kein Auswahlzustand."),
                    ("Für", "Binäre Optionen im Gerät: Mono, Legato, Analyse, Einfrieren."),
                ])
                .content(toggle_card),
            Card::new("Dropdown")
                .spec("½ Zelle × 2 · h 18")
                .ix(&[
                    ("Liste", "Menü des Designsystems, öffnet unterhalb. Tippen springt."),
                    ("Für", "Kits, Sidechain-Quelle, lange Auswahllisten."),
                ])
                .content(dropdown_card),
            Card::new("Mini-Fader")
                .spec("1 × 2 oder 1 × 3")
                .ix(&[
                    ("Knopf", "Kompaktform 24 × 28 aus dem Designsystem."),
                    ("Füllung", "Ab Knopf abwärts. Spurfarbe markiert Träger, text.2 Modulatoren."),
                ])
                .content(mini_fader_card),
            Card::new("Pegel und Reduktion")
                .spec("1 × 3")
                .ix(&[
                    ("GR", "Läuft von oben nach unten in meter.loud."),
                    ("Wert", "Größte Reduktion der letzten 1,5 s, Klick setzt zurück."),
                ])
                .content(meter_card),
            Card::new("Pad-Raster")
                .spec("n × m Zellen")
                .ix(&[
                    ("Klick", "Wählt das Pad und spielt es an. Rechts im Gerät stehen dann seine Parameter."),
                    ("Spielt", "Pad hellt kurz auf (note.play). Auswahl: Kontur akzent."),
                    ("Drop", "Sample auf ein Pad legt es dort ab."),
                ])
                .content(pad_card),
            Card::new("FM-Algorithmus")
                .spec("Sonderanzeige · pisynth")
                .ix(&[
                    ("Anzeige", "Operatoren als Knoten, Modulation als Linien, Träger in Spurfarbe."),
                    ("Klick", "Wählt den Operator."),
                ])
                .content(fm_card),
        ],
    ))
}

// ---------------------------------------------------------------------------------------------
// Layout helpers

/// The `surface.2` demo ground of a card (`widget_card`: padding 24 / 16 / 16, radius.m).
fn demo(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(color32(color::SURFACE_2))
        .corner_radius(corner_radius(radius::M))
        .inner_margin(Margin {
            left: space::SPACE_4 as i8,
            right: space::SPACE_4 as i8,
            top: (space::SPACE_6) as i8,
            bottom: space::SPACE_4 as i8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let layout = Layout::left_to_right(Align::Min).with_main_wrap(true);
            ui.with_layout(layout, |ui| {
                // `gap:20px 18px`.
                ui.spacing_mut().item_spacing = vec2(18.0, 20.0);
                add(ui);
            });
        });
}

/// One sample: the widget `width` wide, its caption under it in `type.device-label` (`state()`:
/// `gap:8px`, centred).
fn sample(ui: &mut Ui, caption: &str, width: f32, add: impl FnOnce(&mut Ui)) {
    let cap = widgets::number_field::text_width(ui, caption, &LABEL);
    column(ui, width.max(cap), Align::Center, space::SPACE_2, |ui| {
        add(ui);
        ui.label(fonts::rich_text(caption, &LABEL).color(color32(color::TEXT_2)));
    });
}

fn live_caption(ui: &mut Ui, text: &str) {
    ui.add_space(space::SPACE_2);
    ui.label(fonts::rich_text(
        text,
        &crate::widgets::knob_caption_style(),
    ));
}

fn live_state<T: Clone + Default + Send + Sync + 'static>(ui: &mut Ui, name: &str) -> (Id, T) {
    let id = ui.scope_id().with(("komponist_device_gallery", name));
    let v = ui.data(|d| d.get_temp::<T>(id)).unwrap_or_default();
    (id, v)
}

// ---------------------------------------------------------------------------------------------
// Cards

fn knob_card(ui: &mut Ui) {
    let lead = color32(palette(LEAD).base);
    type Sample = (
        &'static str,
        f64,
        &'static str,
        State,
        bool,
        Option<ModArc>,
        Option<egui::Color32>,
        bool,
        bool,
    );
    let samples: [Sample; 8] = [
        (
            "standard",
            0.35,
            "1.2 kHz",
            State::REST,
            false,
            None,
            None,
            false,
            false,
        ),
        (
            "hover",
            0.35,
            "1.2 kHz",
            State::HOVERED,
            false,
            None,
            None,
            false,
            false,
        ),
        (
            "ziehen",
            0.58,
            "2.4 kHz",
            State::PRESSED,
            false,
            None,
            None,
            false,
            false,
        ),
        (
            "bipolar",
            0.36,
            "L 14",
            State::REST,
            true,
            None,
            None,
            false,
            false,
        ),
        (
            "moduliert",
            0.45,
            "1.6 kHz",
            State::REST,
            false,
            Some(ModArc {
                low: -0.12,
                high: 0.2,
                color: lead,
            }),
            None,
            false,
            false,
        ),
        (
            "automatisiert",
            0.45,
            "1.6 kHz",
            State::REST,
            false,
            None,
            Some(lead),
            false,
            false,
        ),
        (
            "MIDI lernen",
            0.45,
            "1.6 kHz",
            State::REST,
            false,
            None,
            None,
            true,
            false,
        ),
        (
            "deaktiviert",
            0.35,
            "1.2 kHz",
            State::DISABLED,
            false,
            None,
            None,
            false,
            true,
        ),
    ];
    demo(ui, |ui| {
        for (caption, v, text, st, bipolar, modulation, dot, learn, disabled) in samples {
            sample(ui, caption, cell_width(1), |ui| {
                let mut value = v;
                let r = state::force(ui, st, |ui| {
                    ui.add(
                        KnobCell::new(&mut value, "Cutoff")
                            .bipolar(bipolar)
                            .format(move |_| text.to_owned())
                            .options(KnobCellOptions {
                                automation_dot: dot,
                                learn,
                                modulation,
                                disabled,
                            }),
                    )
                });
                if st.pressed {
                    mini_fader::still_tooltip(ui, r.rect, text);
                }
            });
        }
    });
    live_caption(
        ui,
        "live · Ziehen, Shift fein, Strg grob, Alt-Klick Standard, Doppelklick tippt",
    );
    demo(ui, |ui| {
        let (id, mut v): (_, [f64; 2]) = live_state(ui, "knobs");
        if v == [0.0, 0.0] {
            v = [0.35, 0.5];
        }
        ui.add(
            KnobCell::new(&mut v[0], "Cutoff")
                .default_value(0.35)
                .format(|v| {
                    let hz = 20.0 * 1000f64.powf(v);
                    if hz >= 1000.0 {
                        format!("{:.1} kHz", hz / 1000.0)
                    } else {
                        format!("{hz:.0} Hz")
                    }
                }),
        );
        ui.add(
            KnobCell::new(&mut v[1], "Pan")
                .range(-1.0..=1.0)
                .bipolar(true)
                .format(|v| komponist_model::format::fmt_pan(v as f32)),
        );
        ui.data_mut(|d| d.insert_temp(id, v));
    });
}

fn knob_large_card(ui: &mut Ui) {
    demo(ui, |ui| {
        for (caption, v, text, st) in [
            ("standard", 0.55, "900 Hz", State::REST),
            ("ziehen", 0.66, "1.4 kHz", State::PRESSED),
        ] {
            sample(ui, caption, cell_width(2), |ui| {
                let mut value = v;
                let r = state::force(ui, st, |ui| {
                    ui.add(
                        KnobCell::new(&mut value, "Cutoff")
                            .large()
                            .format(move |_| text.to_owned()),
                    )
                });
                if st.pressed {
                    let d = komponist_tokens::widget::knob_large::DIAMETER;
                    let knob = egui::Rect::from_center_size(
                        egui::pos2(r.rect.left() + 2.0 + d / 2.0, r.rect.center().y),
                        vec2(d, d),
                    );
                    mini_fader::still_tooltip(ui, knob, text);
                }
            });
        }
    });
    live_caption(ui, "live");
    demo(ui, |ui| {
        let (id, mut v): (_, f64) = live_state(ui, "knob_large");
        let mut shown = if v == 0.0 { 0.55 } else { v };
        ui.add(
            KnobCell::new(&mut shown, "Cutoff")
                .large()
                .format(|v| format!("{:.0} Hz", 20.0 * 1000f64.powf(v))),
        );
        v = shown;
        ui.data_mut(|d| d.insert_temp(id, v));
    });
}

fn number_card(ui: &mut Ui) {
    demo(ui, |ui| {
        sample(ui, "standard", cell_width(1), |ui| {
            number::still(ui, "Stimmen", "8", "", State::REST, false, 1);
        });
        sample(ui, "hover", cell_width(1), |ui| {
            number::still(ui, "Stimmen", "8", "", State::HOVERED, false, 1);
        });
        sample(ui, "texteingabe", cell_width(1), |ui| {
            number::still(ui, "Start", "12", "ms", State::REST, true, 1);
        });
        sample(ui, "2 Spalten", cell_width(2), |ui| {
            number::still(ui, "Länge", "1.250", "s", State::REST, false, 2);
        });
        sample(ui, "deaktiviert", cell_width(1), |ui| {
            number::still(ui, "Stimmen", "8", "", State::DISABLED, false, 1);
        });
    });
    live_caption(ui, "live · Ziehen, Doppelklick tippt, ↑ ↓ und Mausrad");
    demo(ui, |ui| {
        let (id, mut v): (_, [i32; 2]) = live_state(ui, "numbers");
        if v == [0, 0] {
            v = [8, 12];
        }
        ui.add(
            Number::new(&mut v[0], "Stimmen")
                .range(1.0..=16.0)
                .step(1.0)
                .default_value(8.0),
        );
        ui.add(
            Number::new(&mut v[1], "Start")
                .range(0.0..=500.0)
                .step(1.0)
                .unit("ms"),
        );
        ui.data_mut(|d| d.insert_temp(id, v));
    });
}

fn segment_card(ui: &mut Ui) {
    let typ = [
        SegmentOption::text("LC"),
        SegmentOption::text("LS"),
        SegmentOption::text("Gl."),
        SegmentOption::text("HS"),
        SegmentOption::text("HC"),
    ];
    let waves = [
        SegmentOption::glyph("Sinus", Glyph::Wave(Wave::Sine)),
        SegmentOption::glyph("Dreieck", Glyph::Wave(Wave::Triangle)),
        SegmentOption::glyph("Säge", Glyph::Wave(Wave::Saw)),
        SegmentOption::glyph("Rechteck", Glyph::Wave(Wave::Square)),
    ];
    let ops: Vec<SegmentOption> = ["1", "2", "3", "4", "5", "6"]
        .into_iter()
        .map(SegmentOption::text)
        .collect();
    demo(ui, |ui| {
        sample(ui, "Text", cell_width(2), |ui| {
            let mut s = 2;
            state::force(ui, State::REST, |ui| {
                ui.add(Segment::new("Typ", &typ, &mut s).cols(2))
            });
        });
        sample(ui, "Icons", cell_width(2), |ui| {
            let mut s = 2;
            state::force(ui, State::REST, |ui| {
                ui.add(Segment::new("Osz A", &waves, &mut s).cols(2))
            });
        });
        sample(ui, "Zahlen", cell_width(3), |ui| {
            let mut s = 0;
            state::force(ui, State::REST, |ui| {
                ui.add(Segment::new("Operator", &ops, &mut s).cols(3))
            });
        });
        sample(ui, "deaktiviert", cell_width(2), |ui| {
            let mut s = 2;
            state::force(ui, State::DISABLED, |ui| {
                ui.add(Segment::new("Typ", &typ, &mut s).cols(2))
            });
        });
    });
    live_caption(ui, "live · Klick, Mausrad, ← →");
    demo(ui, |ui| {
        let (id, mut s): (_, usize) = live_state(ui, "segment");
        ui.add(Segment::new("Typ", &typ, &mut s).cols(2));
        ui.data_mut(|d| d.insert_temp(id, s));
    });
}

fn toggle_card(ui: &mut Ui) {
    demo(ui, |ui| {
        for (caption, label, on, cols, st) in [
            ("aus", "Loop", false, 1, State::REST),
            ("hover", "Loop", false, 1, State::HOVERED),
            ("an", "Loop", true, 1, State::REST),
            ("breit, an", "Ping-Pong", true, 2, State::REST),
            ("deaktiviert", "Loop", false, 1, State::DISABLED),
        ] {
            sample(ui, caption, cell_width(cols), |ui| {
                let mut on = on;
                state::force(ui, st, |ui| {
                    ui.add(ToggleButton::new(label, &mut on).cols(cols))
                });
            });
        }
    });
    live_caption(ui, "live");
    demo(ui, |ui| {
        let (id, mut on): (_, bool) = live_state(ui, "toggle");
        ui.add(ToggleButton::new("Loop", &mut on));
        ui.data_mut(|d| d.insert_temp(id, on));
    });
}

const KITS: [Item<'static>; 3] = [
    Item::new("808 Rostock"),
    Item::new("Studio Kit"),
    Item::new("Lo-Fi Kit"),
];
const SIDECHAIN: [Item<'static>; 4] = [
    Item::new("aus"),
    Item::new("Drums"),
    Item::new("Bass"),
    Item::new("Lead"),
];

fn dropdown_card(ui: &mut Ui) {
    demo(ui, |ui| {
        sample(ui, "standard", cell_width(2), |ui| {
            let mut s = 0;
            state::force(ui, State::REST, |ui| {
                ui.add(DropdownCell::new("Kit", &KITS, &mut s))
            });
        });
        sample(ui, "Sidechain", cell_width(2), |ui| {
            let mut s = 1;
            state::force(ui, State::REST, |ui| {
                ui.add(DropdownCell::new("Sidechain", &SIDECHAIN, &mut s))
            });
        });
        sample(ui, "hover", cell_width(2), |ui| {
            let mut s = 0;
            state::force(ui, State::HOVERED, |ui| {
                ui.add(DropdownCell::new("Kit", &KITS, &mut s))
            });
        });
        sample(ui, "offen", cell_width(2), |ui| {
            let mut s = 0;
            state::force(ui, State::REST, |ui| {
                ui.add(DropdownCell::new("Kit", &KITS, &mut s).open_still(Some(1)))
            });
        });
        sample(ui, "deaktiviert", cell_width(2), |ui| {
            let mut s = 0;
            state::force(ui, State::DISABLED, |ui| {
                ui.add(DropdownCell::new("Kit", &KITS, &mut s))
            });
        });
    });
    live_caption(ui, "live · Klick öffnet, Tippen springt, Mausrad blättert");
    demo(ui, |ui| {
        let (id, mut s): (_, usize) = live_state(ui, "dropdown");
        ui.add(DropdownCell::new("Kit", &KITS, &mut s));
        ui.data_mut(|d| d.insert_temp(id, s));
    });
}

fn mini_fader_card(ui: &mut Ui) {
    let lead = color32(palette(LEAD).base);
    let drum = color32(palette(DRUM).base);
    demo(ui, |ui| {
        for (caption, name, f, text, fill, st, rows) in [
            ("standard", "Op 1", 0.8, "92", Fill::Neutral, State::REST, 2),
            (
                "Spurfarbe",
                "Op 2",
                0.65,
                "78",
                Fill::Track(lead),
                State::REST,
                2,
            ),
            (
                "ziehen",
                "Op 3",
                0.55,
                "64",
                Fill::Neutral,
                State::PRESSED,
                2,
            ),
            (
                "1 × 3",
                "Kick",
                0.82,
                &*format!("{MINUS}2.0"),
                Fill::Track(drum),
                State::REST,
                3,
            ),
            (
                "deaktiviert",
                "Op 4",
                0.65,
                "78",
                Fill::Neutral,
                State::DISABLED,
                2,
            ),
        ] {
            sample(ui, caption, cell_width(1), |ui| {
                mini_fader::still(ui, name, text, f, fill, st, rows);
            });
        }
    });
    live_caption(
        ui,
        "live · Ziehen, Shift fein, Strg grob, Alt-Klick Standard, Doppelklick tippt",
    );
    demo(ui, |ui| {
        let (id, mut v): (_, f64) = live_state(ui, "mini_fader");
        let mut shown = if v == 0.0 { 0.8 } else { v };
        ui.add(
            MiniFader::new(&mut shown, "Op 1")
                .default_value(0.8)
                .format(|v| format!("{:.0}", v * 100.0)),
        );
        v = shown;
        ui.data_mut(|d| d.insert_temp(id, v));
    });
}

fn meter_card(ui: &mut Ui) {
    demo(ui, |ui| {
        sample(ui, "In · GR · Out", cell_width(1), |ui| {
            ui.add(MeterGr::new([0.72, 0.18, 0.64], &meter_gr::readout(4.1)));
        });
        sample(ui, "Pegel über 75 %", cell_width(1), |ui| {
            ui.add(MeterGr::new([0.92, 0.5, 0.88], &meter_gr::readout(9.8)));
        });
        sample(ui, "still", cell_width(1), |ui| {
            ui.add(MeterGr::new([0.0, 0.0, 0.0], &meter_gr::readout(0.0)));
        });
    });
}

fn pad_card(ui: &mut Ui) {
    let pads = [
        Pad::new("Kick", "C1"),
        Pad::new("Snare", "D1"),
        Pad::new("Clap", "D#1"),
        Pad::new("Hat zu", "F#1"),
    ];
    let pal = palette(DRUM);
    demo(ui, |ui| {
        sample(ui, "Auswahl · spielt", cell_width(4), |ui| {
            PadGrid::new(&pads, 4, &pal)
                .selected(Some(0))
                .playing(&[3])
                .show(ui);
        });
    });
    live_caption(ui, "live · Klick wählt und spielt an");
    demo(ui, |ui| {
        let (id, mut sel): (_, usize) = live_state(ui, "pads");
        let out = PadGrid::new(&pads, 4, &pal).selected(Some(sel)).show(ui);
        if let Some(c) = out.clicked {
            sel = c;
        }
        ui.data_mut(|d| d.insert_temp(id, sel));
    });
}

fn fm_card(ui: &mut Ui) {
    let pal = palette(LEAD);
    let algo = Topology {
        modulates: &[0, 0b1, 0, 0b100, 0, 0b10000],
        feedback: Some(5),
    };
    demo(ui, |ui| {
        sample(ui, "Algorithmus 5", cell_width(2), |ui| {
            FmAlgorithm::new(5, algo, &pal).selected(Some(0)).show(ui);
        });
    });
    live_caption(ui, "live · Klick wählt den Operator");
    demo(ui, |ui| {
        let (id, mut sel): (_, usize) = live_state(ui, "fm");
        let out = FmAlgorithm::new(5, algo, &pal).selected(Some(sel)).show(ui);
        if let Some(c) = out.clicked {
            sel = c;
        }
        ui.data_mut(|d| d.insert_temp(id, sel));
    });
}
