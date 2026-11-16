//! The graph cards of the gallery page „Geräte-Widgets“ (`Geraete.dc.html` section 03): the layers
//! one by one, then every configuration in its states (standard, Griff gewählt, Bypass) and live.
//! The data layer gets synthetic test data (the generator's formulas); live engine data comes
//! later.

use egui::{Id, Ui, Vec2, vec2};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};
use komponist_tokens::TrackPalette;
use komponist_tokens::egui_support::color32;

use super::configs::{self, band};
use super::spec::{Axis, AxisKind, GraphSpec, Layer};
use super::{Graph, GraphOutput, Look, SlotInfo};
use crate::Lang;
use crate::gallery_shell::Card;
use crate::widgets::device::cell_width;
use crate::widgets::device::gallery::{demo, live_caption, live_state, sample};

/// Hue of the generator's `LEAD` and `DRUM` tracks.
const LEAD: u16 = 150;
const DRUM: u16 = 45;

/// A configuration with values, slot descriptions and test data.
struct Demo {
    spec: GraphSpec,
    values: Vec<f64>,
    infos: Vec<SlotInfo<'static>>,
    data: Vec<f32>,
    trace: Vec<f32>,
    size: Vec2,
    hue: u16,
    playhead: Option<f64>,
    level: Option<f64>,
    /// The handle the „Griff gewählt“ sample selects.
    pick: usize,
}

impl Demo {
    fn show(
        &self,
        ui: &mut Ui,
        values: &mut [f64],
        look: Look,
        selected: Option<usize>,
        id: Option<Id>,
    ) -> GraphOutput {
        let palette = TrackPalette::new(self.hue);
        let mut g = Graph::new(&self.spec, values, &self.infos, self.size)
            .track(color32(palette.base))
            .look(look)
            .data(&self.data)
            .trace(&self.trace)
            .playhead(self.playhead)
            .level(self.level);
        if let Some(id) = id {
            g = g.id(id);
        }
        if look != Look::Normal || id.is_none() {
            g = g.selected(selected);
        }
        g.show(ui)
    }

    fn still(&self, ui: &mut Ui, look: Look, selected: Option<usize>) {
        let mut values = self.values.clone();
        self.show(ui, &mut values, look, selected, None);
    }
}

fn param(id: &str, name: &str, kind: ParamKind, default: f64, unit: Unit) -> ParamInfo {
    ParamInfo::new(ParamId::new(id), name, kind, default).unit(unit)
}

fn size_of(cols: u8) -> Vec2 {
    // `CW3 = gw(4)` × 98: four cells wide, 1.5 rows high.
    vec2(cell_width(cols), 98.0)
}

/// The spectrum of the generator (`W_eq`): level over 101 log-spaced points.
fn spectrum() -> Vec<f32> {
    (0..=100)
        .map(|i| {
            let f = 20.0 * 1000f64.powf(f64::from(i) / 100.0);
            let lev = -18.0 - 10.0 * (f / 180.0).log10().abs().powf(1.3)
                + 3.0 * (f64::from(i) * 1.7).sin()
                + 2.0 * (f64::from(i) * 0.53).sin();
            ((lev + 60.0) / 60.0) as f32
        })
        .collect()
}

/// The sample of the generator (`W_wave`, kind `kick`): amplitude every 2 px of 236.
fn kick(points: usize) -> Vec<f32> {
    (0..points)
        .map(|i| {
            let t = i as f64 / (points - 1) as f64;
            let ramp = if t > 0.01 { 0.95 } else { t * 90.0 };
            ((-t * 5.0).exp() * ramp * (0.85 + 0.15 * (t * 90.0).sin())) as f32
        })
        .collect()
}

fn envelope() -> Demo {
    let time = |id: &str, name: &str, max: f64, default: f64| {
        SlotInfo::from_param(
            &param(
                id,
                name,
                ParamKind::scaled(0.001, max, Scale::Log),
                default,
                Unit::S,
            ),
            true,
        )
    };
    Demo {
        spec: configs::envelope(Lang::De).titled("Amplitude"),
        // Attack 8 ms, decay 0.8 s, sustain 55 %, release 4.5 s, curve in the middle.
        values: vec![0.23, 0.85, 0.55, 1.0, 0.5],
        infos: vec![
            time("attack", "Attack", 10.0, 0.004),
            time("decay", "Decay", 10.0, 0.2),
            SlotInfo::from_param(
                &param(
                    "sustain",
                    "Sustain",
                    ParamKind::lin(0.0, 1.0),
                    0.55,
                    Unit::Percent,
                ),
                false,
            ),
            time("release", "Release", 20.0, 1.2),
            SlotInfo::new(0.0, 1.0, 0.5),
        ],
        data: Vec::new(),
        size: size_of(4),
        hue: LEAD,
        playhead: Some(0.32),
        level: None,
        trace: Vec::new(),
        pick: 1,
    }
}

fn eq_demo(size: Vec2) -> Demo {
    // (frequency, gain, Q, type) of the generator's `EQB`.
    let bands = [
        (60.0, 0.0, 1.0, band::LOW_CUT),
        (180.0, 3.0, 0.9, band::LOW_SHELF),
        (420.0, -4.0, 1.4, band::BELL),
        (2400.0, 2.5, 1.0, band::BELL),
        (9000.0, 3.5, 1.0, band::HIGH_SHELF),
    ];
    let mut infos = Vec::new();
    let mut values = Vec::new();
    for (n, (f, g, q, t)) in bands.into_iter().enumerate() {
        let n = n + 1;
        infos.push(SlotInfo::new(0.0, 1.0, 1.0));
        infos.push(SlotInfo::from_param(
            &param(
                &format!("b{n}_freq"),
                "Frequenz",
                ParamKind::scaled(20.0, 20000.0, Scale::Log),
                f,
                Unit::Hz,
            ),
            false,
        ));
        infos.push(SlotInfo::from_param(
            &param(
                &format!("b{n}_gain"),
                "Gain",
                ParamKind::lin(-24.0, 24.0),
                0.0,
                Unit::Db,
            ),
            false,
        ));
        infos.push(SlotInfo::new(0.1, 18.0, 0.71));
        infos.push(SlotInfo::new(0.0, 4.0, band::BELL));
        values.extend([1.0, f, g, q, t]);
    }
    Demo {
        spec: configs::eq(Lang::De, 5, 18.0),
        values,
        infos,
        data: spectrum(),
        size,
        hue: LEAD,
        playhead: None,
        level: None,
        trace: Vec::new(),
        pick: 2,
    }
}

fn filter(kind: f64) -> Demo {
    Demo {
        spec: configs::filter(Lang::De),
        values: vec![900.0, 0.45, kind],
        infos: vec![
            SlotInfo::from_param(
                &param(
                    "cutoff",
                    "Cutoff",
                    ParamKind::scaled(20.0, 20000.0, Scale::Log),
                    900.0,
                    Unit::Hz,
                ),
                false,
            ),
            SlotInfo::from_param(
                &param(
                    "resonance",
                    "Resonanz",
                    ParamKind::lin(0.0, 1.0),
                    0.45,
                    Unit::Percent,
                ),
                false,
            ),
            SlotInfo::new(0.0, 3.0, 1.0),
        ],
        data: Vec::new(),
        size: size_of(4),
        // `BASS` in the generator.
        hue: 250,
        playhead: None,
        level: None,
        trace: Vec::new(),
        pick: 0,
    }
}

fn transfer() -> Demo {
    Demo {
        spec: configs::transfer(Lang::De),
        values: vec![-18.0, 4.0, 0.0],
        infos: vec![
            SlotInfo::from_param(
                &param(
                    "threshold",
                    "Threshold",
                    ParamKind::lin(-60.0, 0.0),
                    -18.0,
                    Unit::Db,
                ),
                false,
            ),
            SlotInfo::from_param(
                &param(
                    "ratio",
                    "Ratio",
                    ParamKind::scaled(1.0, 20.0, Scale::Log),
                    4.0,
                    Unit::Ratio,
                ),
                false,
            ),
            SlotInfo::new(0.0, 1.0, 1.0),
        ],
        data: Vec::new(),
        size: vec2(cell_width(2), 98.0),
        hue: LEAD,
        playhead: None,
        level: Some(-9.0),
        trace: Vec::new(),
        pick: 0,
    }
}

/// The compressor of the device `presse` at its size in the layout (3 columns × 2 rows) with a
/// level point at −9 dB.
fn compressor() -> Demo {
    let mut d = transfer();
    d.spec = configs::compressor(Lang::De);
    d.values = vec![-18.0, 4.0, 6.0];
    d.infos[2] = SlotInfo::from_param(
        &param("knee", "Knie", ParamKind::lin(0.0, 24.0), 6.0, Unit::Db),
        false,
    );
    d.size = vec2(cell_width(3), crate::widgets::device::cell_height(2));
    d.level = Some(-9.0);
    d
}

/// The reduction history of `presse` (2 columns × 1 row): a few seconds of music-like material.
fn gr_history() -> Demo {
    let points = 128;
    let level: Vec<f32> = (0..points)
        .map(|i| {
            let t = i as f32 / points as f32;
            (0.05 + 0.3 * (t * 23.0).sin().abs() * (t * 3.0).cos().abs()).clamp(0.0, 0.4)
        })
        .collect();
    let reduction: Vec<f32> = (0..points)
        .map(|i| {
            let t = i as f32 / points as f32;
            -(1.0 + 13.0 * (t * 9.0).sin().powi(2) * (0.4 + 0.6 * t)).min(18.0)
        })
        .collect();
    Demo {
        spec: configs::gr_history(Lang::De),
        values: Vec::new(),
        infos: Vec::new(),
        data: level,
        trace: reduction,
        size: vec2(cell_width(2), crate::widgets::device::cell_height(1)),
        hue: LEAD,
        playhead: Some(configs::GR_HISTORY_SECONDS),
        level: None,
        pick: 0,
    }
}

fn waveform() -> Demo {
    Demo {
        spec: configs::waveform(Lang::De, 0.5).titled("kick_dry_04.wav"),
        values: vec![20.0, 450.0],
        infos: vec![
            SlotInfo::from_param(
                &param("start", "Start", ParamKind::lin(0.0, 500.0), 0.0, Unit::Ms),
                false,
            ),
            SlotInfo::from_param(
                &param("end", "Ende", ParamKind::lin(0.0, 500.0), 450.0, Unit::Ms),
                false,
            ),
        ],
        data: kick(119),
        size: size_of(4),
        hue: DRUM,
        playhead: Some(0.1),
        level: None,
        trace: Vec::new(),
        pick: 0,
    }
}

fn xy() -> Demo {
    let unit = |lo: f64, hi: f64| Axis::new(AxisKind::Lin, lo, hi);
    Demo {
        spec: configs::xy(Lang::De, unit(0.0, 1.0), unit(0.0, 1.0), "Zeit", "Feedback"),
        values: vec![0.5, 0.42],
        infos: vec![
            SlotInfo::new(0.0, 1.0, 0.5).format(|_| "1/8 P".to_owned()),
            SlotInfo::from_param(
                &param(
                    "feedback",
                    "Feedback",
                    ParamKind::lin(0.0, 1.0),
                    0.42,
                    Unit::Percent,
                ),
                false,
            ),
        ],
        data: Vec::new(),
        size: vec2(cell_width(4), 98.0),
        hue: LEAD,
        playhead: None,
        level: None,
        trace: Vec::new(),
        pick: 0,
    }
}

fn lfo() -> Demo {
    // The generator's points (phase, value).
    let points = [(0.18, 0.85), (0.32, 0.85), (0.55, -0.6), (0.8, -0.2)];
    let mut values = Vec::new();
    let mut infos = Vec::new();
    for (x, y) in points {
        values.extend([x, y]);
        infos.push(SlotInfo::new(0.0, 1.0, 0.0));
        infos.push(SlotInfo::new(-1.0, 1.0, 0.0));
    }
    Demo {
        spec: configs::lfo(Lang::De, 4).titled("LFO 1 · Form"),
        values,
        infos,
        data: Vec::new(),
        size: size_of(4),
        hue: LEAD,
        playhead: Some(0.37),
        level: None,
        trace: Vec::new(),
        pick: 2,
    }
}

/// A sample line: standard, Griff gewählt, Bypass, and a live graph.
fn config_card(ui: &mut Ui, name: &str, demo_of: &Demo, extra: Option<(&str, &Demo)>) {
    demo(ui, |ui| {
        sample(ui, "standard", demo_of.size.x, |ui| {
            demo_of.still(ui, Look::Normal, None)
        });
        sample(ui, "Griff gewählt", demo_of.size.x, |ui| {
            demo_of.still(ui, Look::Normal, Some(demo_of.pick));
        });
        sample(ui, "Bypass", demo_of.size.x, |ui| {
            demo_of.still(ui, Look::Bypass, Some(demo_of.pick))
        });
        sample(ui, "deaktiviert", demo_of.size.x, |ui| {
            demo_of.still(ui, Look::Disabled, None)
        });
        if let Some((caption, other)) = extra {
            sample(ui, caption, other.size.x, |ui| {
                other.still(ui, Look::Normal, None)
            });
        }
    });
    live_caption(
        ui,
        "live · Ziehen, Shift fein, Strg grob, Alt-Klick Standard, Tab und Pfeile, Doppelklick auf die Fläche",
    );
    demo(ui, |ui| {
        let (id, stored): (_, Vec<f64>) = live_state(ui, name);
        let mut values = if stored.len() == demo_of.values.len() {
            stored
        } else {
            demo_of.values.clone()
        };
        let gid = ui.scope_id().with(("komponist_graph_live", name));
        demo_of.show(ui, &mut values, Look::Normal, None, Some(gid));
        ui.data_mut(|d| d.insert_temp(id, values));
    });
}

fn envelope_card(ui: &mut Ui) {
    config_card(ui, "graph_envelope", &envelope(), None);
}

fn eq_card(ui: &mut Ui) {
    config_card(ui, "graph_eq", &eq_demo(size_of(4)), None);
}

fn filter_card(ui: &mut Ui) {
    config_card(
        ui,
        "graph_filter",
        &filter(1.0),
        Some(("Hochpass", &filter(3.0))),
    );
}

fn transfer_card(ui: &mut Ui) {
    config_card(ui, "graph_transfer", &transfer(), None);
}

fn compressor_card(ui: &mut Ui) {
    config_card(ui, "graph_compressor", &compressor(), None);
}

fn gr_history_card(ui: &mut Ui) {
    config_card(ui, "graph_gr_history", &gr_history(), None);
}

fn waveform_card(ui: &mut Ui) {
    config_card(ui, "graph_waveform", &waveform(), None);
}

fn xy_card(ui: &mut Ui) {
    config_card(ui, "graph_xy", &xy(), None);
}

fn lfo_card(ui: &mut Ui) {
    config_card(ui, "graph_lfo", &lfo(), None);
}

fn layers_card(ui: &mut Ui) {
    // `layer_thumb`: the EQ at 3 cells × 112 with some layers left out.
    let thumb = |keep_grid_only: bool, data: bool, curve: bool, handles: bool| {
        let mut d = eq_demo(vec2(cell_width(3), 112.0));
        d.spec.layers.retain(|l| match l {
            Layer::Data(_) => data,
            Layer::Curve(_) => curve,
            _ => !keep_grid_only,
        });
        if !handles {
            d.spec.handles.clear();
        }
        if !data {
            d.data.clear();
        }
        d
    };
    demo(ui, |ui| {
        sample(ui, "1 · Achsen und Raster", cell_width(3), |ui| {
            thumb(true, false, false, false).still(ui, Look::Normal, None)
        });
        sample(ui, "2 · Daten", cell_width(3), |ui| {
            thumb(true, true, false, false).still(ui, Look::Normal, None)
        });
        sample(ui, "3 · Kurve", cell_width(3), |ui| {
            thumb(true, false, true, false).still(ui, Look::Normal, None)
        });
        sample(ui, "4 · Griffe", cell_width(3), |ui| {
            thumb(true, false, false, true).still(ui, Look::Normal, Some(2))
        });
        sample(ui, "= · Zusammen", cell_width(3), |ui| {
            thumb(false, true, true, true).still(ui, Look::Normal, Some(2))
        });
    });
}

/// The cards: the layers, then the seven configurations.
pub fn cards() -> Vec<Card> {
    vec![
        Card::new("Graph · Ebenen")
            .spec("min 2 × 1 · jede Größe im Raster")
            .ix(&[
                ("Achsen", "X und Y je linear, logarithmisch, dB oder Zeit. Raster und Beschriftung folgen der Achse."),
                ("Daten", "Live-Daten aus der Engine: Spektrum, Sample, Pegelverlauf. Nur Anzeige."),
                ("Kurve", "Funktion der Parameter, 1,5 px Spurfarbe, Fläche 14 % bis zur Basislinie."),
                ("Griffe", "Je Griff 1 bis 3 Parameter: ↔, ↕ und Mausrad. Optional nummeriert."),
                ("Zusammen", "Ebenen werden in dieser Reihenfolge gezeichnet. Dazu Marker: Playhead, Pegelpunkt, Grenzlinien."),
                ("Griff ziehen", "Wie Drehregler: Shift fein, Strg grob, Alt-Klick Standardwert, Werte-Tooltip am Griff. Gewählter Griff 2 px akzent."),
                ("Griff wählen", "Wählt den zugehörigen Parameter für Inspector, Automation und MIDI lernen. Bei 2 Achsen der Parameter der zuletzt bewegten Achse."),
                ("Doppelklick Fläche", "Optional je Konfiguration: neuen Punkt oder neues Band anlegen. Sonst Zoom zurücksetzen."),
                ("Tastatur", "Tab wechselt Griffe, Pfeile bewegen um einen Schritt, Shift fein."),
                ("Bypass, deaktiviert", "Kurve und Griffe in text.4, Daten-Ebene ausgeblendet."),
            ])
            .content(layers_card),
        Card::new("Hüllkurve")
            .spec("graph::envelope")
            .ix(&[
                ("X", "Zeit, Segmente proportional"),
                ("Y", "linear 0–1"),
                ("Ebenen", "Kurve aus Segmenten, Marker Playhead"),
                ("Griffe", "A ↔ · D ↔↕ · S ↕ · R ↔; Mausrad = Krümmung"),
            ])
            .content(envelope_card),
        Card::new("Parametrischer EQ")
            .spec("graph::eq")
            .ix(&[
                ("X", "log, 20 Hz – 20 kHz"),
                ("Y", "dB, ±12 oder ±24"),
                ("Ebenen", "Spektrum, Summenkurve"),
                ("Griffe", "je Band Freq ↔, Gain ↕, Q Mausrad; Doppelklick neu"),
            ])
            .content(eq_card),
        Card::new("Filter")
            .spec("graph::filter")
            .ix(&[
                ("X", "log, 20 Hz – 20 kHz"),
                ("Y", "dB, −36 bis +12"),
                ("Ebenen", "Kurve, Modulationsbereich"),
                ("Griffe", "ein Griff: Cutoff ↔, Resonanz ↕"),
            ])
            .content(filter_card),
        Card::new("Kompressor-Kennlinie")
            .spec("graph::transfer")
            .ix(&[
                ("X", "dB, −60 bis 0 (Eingang)"),
                ("Y", "dB, −60 bis 0 (Ausgang)"),
                ("Ebenen", "Diagonale, Kurve, Pegelpunkt live"),
                ("Griffe", "Knie: Threshold ↔, Ratio ↕"),
            ])
            .content(transfer_card),
        Card::new("Kompressor (Presse)")
            .spec("graph::compressor")
            .ix(&[
                ("X", "dB, −60 bis 0 (Eingang), Beschriftung −48, −24, 0"),
                ("Y", "dB, −60 bis 0 (Ausgang)"),
                ("Ebenen", "Diagonale, Kurve mit Knie in dB, Pegelpunkt live"),
                ("Ablesung", "IN, OUT und GR des Pegelpunkts rechts neben der Kurve"),
                ("Griffe", "Knie: Threshold ↔, Ratio ↕"),
            ])
            .content(compressor_card),
        Card::new("Reduktionsverlauf (Presse)")
            .spec("graph::gr_history")
            .ix(&[
                ("X", "Zeit, 4 s, jetzt rechts"),
                ("Y", "dB, 0 bis −24, die Reduktion hängt von oben"),
                ("Ebenen", "Daten Eingangspegel, Verlauf Reduktion, Marker Playhead"),
                ("Griffe", "keine, nur Anzeige"),
            ])
            .content(gr_history_card),
        Card::new("Wellenform")
            .spec("graph::waveform")
            .ix(&[
                ("X", "Zeit, Länge des Samples"),
                ("Y", "bipolar, linear"),
                ("Ebenen", "Daten Sample, Bereich außerhalb S–E abgedunkelt"),
                ("Griffe", "S und E als Linien, nur ↔"),
            ])
            .content(waveform_card),
        Card::new("XY-Feld")
            .spec("graph::xy")
            .ix(&[
                ("X", "Parameter 1, eigene Skala"),
                ("Y", "Parameter 2, eigene Skala"),
                ("Ebenen", "nur Raster und Fadenkreuz"),
                ("Griffe", "ein Griff ↔↕; Shift sperrt eine Achse"),
            ])
            .content(xy_card),
        Card::new("LFO-Form")
            .spec("graph::lfo")
            .ix(&[
                ("X", "Phase 0–1, Raster nach Takt"),
                ("Y", "bipolar −1 bis 1"),
                ("Ebenen", "Kurve aus Punkten, Playhead"),
                ("Griffe", "Punkte ↔↕, Doppelklick neu, Entf löscht"),
            ])
            .content(lfo_card),
    ]
}
