//! WP-72: the texts of the UI crate in English (`Lang::En`): widget names and values, the
//! timeline's AccessKit names and the graph configurations. German is covered by the tests next
//! to the code and by the snapshots (zero diffs).

use egui::accesskit::Role;
use egui::{Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_model::{Meter, Tick};
use komponist_tokens::TrackPalette;
use komponist_ui::snapshot;
use komponist_ui::timeline::TimeAxis;
use komponist_ui::timeline::{
    ClipModel, ClipState, HeaderModel, NoteState, RulerKind, RulerModel, bar_span, clip_access,
    header_access, note_access, ruler_access,
};
use komponist_ui::widgets::device::fm_algorithm::{FmAlgorithm, Topology};
use komponist_ui::widgets::device::graph::{self, Which, configs};
use komponist_ui::widgets::device::meter_gr::MeterGr;
use komponist_ui::widgets::device::pad_grid::{Pad, PadGrid};
use komponist_ui::widgets::meter::{MeterState, value_text};
use komponist_ui::widgets::toggle::{ToggleKind, TrackToggle};
use komponist_ui::{Lang, State};

fn harness(lang: Lang, mut app: impl FnMut(&mut Ui) + 'static) -> Harness<'static> {
    let mut h = snapshot::builder::<()>(1.0)
        .with_size(vec2(320.0, 240.0))
        .build_ui(move |ui| {
            komponist_ui::set_lang(ui.ctx(), lang);
            app(ui);
        });
    h.run();
    h
}

fn widgets(ui: &mut Ui) {
    let pal = TrackPalette::new(45);
    let algo = Topology {
        modulates: &[0, 1],
        outputs: None,
        feedback: None,
    };
    let pads = [Pad::new("Kick", "C1")];
    ui.add(MeterGr::new([0.5, 0.2, 0.4], "\u{2212}4.1"));
    TrackToggle::new(ToggleKind::Monitor, false)
        .implied(true)
        .track("Lead")
        .show(ui);
    PadGrid::new(&pads, 1, &pal).show(ui);
    FmAlgorithm::new(1, algo, &pal).show(ui);
}

#[test]
fn widget_names_and_values_follow_the_language() {
    let en = harness(Lang::En, widgets);
    let meter = en.get_by_role_and_label(Role::Meter, "Level and gain reduction");
    assert_eq!(
        meter.accesskit_node().value().as_deref(),
        Some("maximum reduction \u{2212}4.1 dB")
    );
    let toggle = en.get_by_role_and_label(Role::Button, "Monitoring · Lead");
    assert_eq!(
        toggle.accesskit_node().value().as_deref(),
        Some("monitoring through Record-Arm")
    );
    en.get_by_role_and_label(Role::Button, "Pad Kick");
    en.get_by_role_and_label(Role::Button, "Operator 1, carrier");
    en.get_by_role_and_label(Role::Button, "Operator 2, modulator");

    let de = harness(Lang::De, widgets);
    de.get_by_role_and_label(Role::Meter, komponist_i18n::de::UI_METER_GR_NAME);
    de.get_by_role_and_label(Role::Button, "Operator 1, Träger");
}

#[test]
fn meter_value_says_clip() {
    let clipped = MeterState::frozen([0.8, 0.5], [0.8, 0.6]);
    assert_eq!(value_text(Lang::En, &clipped), "+0.8 dB, clip");
    assert_eq!(value_text(Lang::De, &clipped), "+0.8 dB, Clip");
}

#[test]
fn timeline_names_in_english() {
    let p = TrackPalette::new(45);
    let m = Meter::FOUR_FOUR;
    assert_eq!(
        bar_span(Lang::En, Tick(0), Tick(4 * 3840), m),
        "bars 1\u{2013}4"
    );
    assert_eq!(
        bar_span(Lang::En, Tick(8 * 3840), Tick(9 * 3840), m),
        "bar 9"
    );
    let mut clip = ClipModel::new("Beat A", &p);
    let a = clip_access(Lang::En, &clip, Tick(0), Tick(4 * 3840), m, ClipState::REST);
    assert_eq!(a.name, "Clip Beat A, bars 1\u{2013}4");
    clip.muted = true;
    let a = clip_access(Lang::En, &clip, Tick(0), Tick(4 * 3840), m, ClipState::REST);
    assert_eq!(a.name, "Clip Beat A, bars 1\u{2013}4, muted");
    let a = note_access(Lang::En, 60, Tick(8 * 3840), m, NoteState::REST);
    assert_eq!(a.name, "Note C4, 9.1.000");
    let head = HeaderModel::new("Lead", "pisynth", &p);
    assert_eq!(
        header_access(Lang::En, &head, State::REST).name,
        "Track Lead, pisynth"
    );
    let ruler = RulerModel {
        axis: TimeAxis::per_bar(0.0, 72.0, m),
        meter: m,
        kind: RulerKind::Arrangement { loop_range: None },
        playhead: None,
        recording: false,
    };
    assert_eq!(ruler_access(Lang::En, &ruler).name, "Ruler");
}

#[test]
fn graph_names_in_english() {
    let spec = configs::eq(Lang::En, 2, 18.0);
    assert_eq!(spec.name, "Parametric EQ");
    assert_eq!(
        graph::accessible_name(&spec.handles[1], Which::X),
        "Band 2 Frequency"
    );
    let env = configs::envelope(Lang::En);
    assert_eq!(env.handles[1].name, "Decay");
    assert_eq!(
        graph::accessible_name(&env.handles[1], Which::Wheel),
        "Decay Curve"
    );
}
