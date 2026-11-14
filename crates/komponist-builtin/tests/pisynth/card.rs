//! The device card's parameters (WP-86): every parameter views one patch field, events reach the
//! running patch like the document-side `Field::set`, old macro states load, and the layout
//! validates for every operator.

use komponist_builtin::device::{self, Field, OpField, layout, params};
use komponist_builtin::{
    Macro, PRESET_COUNT, decode_state, default_patch, encode_state, preset_patch,
};
use komponist_engine::EventKind;
use komponist_model::params::layout::validate;
use komponist_model::params::layout::ENV_OP;
use komponist_model::params::{ParamContext, ParamId};

use crate::common::{activated, note_on, play};

#[test]
fn ids_are_unique_wire_ids_round_trip_and_defaults_are_in_range() {
    let params = params();
    let mut keys: Vec<&str> = params.iter().map(|p| p.info.id.as_str()).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), params.len());
    let mut wires: Vec<u32> = params.iter().map(|p| p.wire).collect();
    wires.sort_unstable();
    wires.dedup();
    assert_eq!(wires.len(), params.len());
    for p in params {
        assert_eq!(Field::from_wire(p.wire), Some(p.field), "{}", p.info.id);
        assert_eq!(p.field.wire(), p.wire);
        let (min, max) = p.info.range();
        assert!((min..=max).contains(&p.info.default), "{}", p.info.id);
        assert_eq!(p.info.snap(p.info.default), p.info.default, "{}", p.info.id);
        assert!(!p.info.format(p.info.default).is_empty());
    }
    // 8 macros stay, the card's own start right after them.
    assert_eq!(params[0].wire, Macro::Algorithm.id());
    assert_eq!(params[1].wire, Macro::Feedback.id());
    assert_eq!(Field::from_wire(device::FIRST_WIRE - 1), Some(Field::Macro(Macro::Release)));
    assert_eq!(Field::from_wire(device::GLOBAL_BASE + 6), None);
    assert_eq!(params.len(), 2 + 6 + 6 * OpField::COUNT);
}

#[test]
fn set_then_get_gives_the_value_and_changes_nothing_else() {
    for p in params() {
        let (min, max) = p.info.range();
        let mid = p.info.snap(min + (max - min) * 0.37);
        let base = preset_patch(1).unwrap();
        let mut patch = base;
        p.field.set(&mut patch, mid);
        let got = p.field.get(&patch);
        // Times at the bottom of the knob are 0 in the patch; floats go through f32.
        assert!((got - mid).abs() <= 1e-5 * max.abs().max(1.0), "{}: {mid} → {got}", p.info.id);
        assert_eq!(patch, patch.sanitized(), "{} keeps the patch sanitized", p.info.id);
        // Non-finite values are ignored.
        let mut same = patch;
        p.field.set(&mut same, f64::NAN);
        assert_eq!(same, patch);
    }
}

#[test]
fn a_parameter_writes_the_patch_field_the_table_names() {
    let mut p = preset_patch(0).unwrap();
    let set = |patch: &mut komponist_builtin::Patch, key: &str, v: f64| {
        let field = device::param(&ParamId::new(key)).unwrap().field;
        field.set(patch, v);
    };
    set(&mut p, "op3_ratio", 3.5);
    assert_eq!(p.operators[2].ratio, 3.5);
    assert_eq!(p.operators[2].pitch_mode, synth_core::PitchMode::Ratio);
    set(&mut p, "op4_level", 0.4);
    set(&mut p, "op5_attack", 2.0);
    set(&mut p, "op5_sustain", 0.25);
    set(&mut p, "op6_key_scale", -12.0);
    set(&mut p, "op2_output", 1.0);
    set(&mut p, "op2_loop", 1.0);
    set(&mut p, "op1_hold", 0.0005);
    set(&mut p, "volume", 0.5);
    set(&mut p, "glide", 0.2);
    set(&mut p, "voices", 4.0);
    set(&mut p, "mono", 1.0);
    set(&mut p, "spread", 0.8);
    set(&mut p, "tune", -2.0);
    assert_eq!(p.operators[3].level, 0.4);
    assert_eq!(p.operators[4].envelope.attack, 2.0);
    assert_eq!(p.operators[4].envelope.sustain, 0.25);
    assert_eq!(p.operators[5].key_scaling.right, -12.0);
    assert!(p.operators[1].output && p.operators[1].envelope.looping);
    assert_eq!(p.operators[0].envelope.hold, 0.0);
    assert_eq!(
        (p.volume, p.portamento, p.unison, p.unison_spread, p.tune),
        (0.5, 0.2, 4, 0.8, -2.0)
    );
    assert_eq!(p.voice_mode, synth_core::VoiceMode::Mono);
    // Mono off is poly; legato stays legato while mono is on.
    set(&mut p, "mono", 0.0);
    assert_eq!(p.voice_mode, synth_core::VoiceMode::Poly);
    p.voice_mode = synth_core::VoiceMode::Legato;
    set(&mut p, "mono", 1.0);
    assert_eq!(p.voice_mode, synth_core::VoiceMode::Legato);
}

#[test]
fn old_macro_states_load_and_the_shared_parameters_agree() {
    // A state saved with the 8 macros is a patch like any other; the card reads it.
    for index in 0..PRESET_COUNT {
        let mut patch = preset_patch(index).unwrap();
        Macro::Algorithm.set(&mut patch, 5.0);
        Macro::Ratio2.set(&mut patch, 3.5);
        Macro::Index.set(&mut patch, 0.6);
        Macro::Sustain.set(&mut patch, 0.25);
        let loaded = decode_state(Some(&encode_state(&patch))).unwrap();
        let get = |key: &str| device::param(&ParamId::new(key)).unwrap().field.get(&loaded);
        assert_eq!(get("algorithm"), Macro::Algorithm.get(&loaded));
        assert_eq!(get("feedback"), Macro::Feedback.get(&loaded));
        assert_eq!(get("op2_ratio"), 3.5);
        assert!((get("op2_level") - 0.6).abs() < 1e-6);
        let first = loaded.operators.iter().position(|o| o.output).unwrap_or(0);
        let carrier = format!("op{}_sustain", first + 1);
        assert!((get(&carrier) - 0.25).abs() < 1e-6, "preset {index}");
    }
}

#[test]
fn a_card_event_reaches_the_running_patch_like_the_document_side_set() {
    let patch = preset_patch(1).unwrap();
    let changes: Vec<(&str, f64)> = vec![
        ("op1_ratio", 2.0),
        ("op2_level", 0.3),
        ("op1_decay", 1.5),
        ("op3_key_scale", 6.0),
        ("volume", 0.4),
        ("voices", 3.0),
        ("mono", 1.0),
        ("algorithm", 12.0),
    ];
    let mut expected = patch;
    let mut events = Vec::new();
    for (i, (key, v)) in changes.iter().enumerate() {
        let p = device::param(&ParamId::new(*key)).unwrap();
        p.field.set(&mut expected, *v);
        events.push((i * 10, EventKind::Param { id: p.wire, value: *v }));
    }
    events.push((100, note_on(60)));
    let mut proc = activated(patch);
    let _ = play(&mut proc, 512, 256, &events);
    assert_eq!(*proc.patch(), expected);
}

#[test]
fn the_layout_validates_for_every_operator_and_shows_every_card_parameter() {
    let known = |id: &ParamId| device::param(id).is_some();
    let layout = layout();
    assert_eq!(layout.pages.len(), 4);
    assert_eq!(
        layout.pages.iter().map(|p| (p.name.as_str(), p.cols)).collect::<Vec<_>>(),
        [("Operatoren", 10), ("Hüllkurven", 8), ("LFO", 8), ("Global", 8)]
    );
    for op in 1..=6 {
        let ctx = ParamContext::new().with(ENV_OP, op);
        assert_eq!(validate(&layout, &ctx, &known), vec![], "env_op {op}");
    }
    // Every parameter is reachable from some cell for some operator.
    let mut shown = std::collections::BTreeSet::new();
    for op in 1..=6 {
        let ctx = ParamContext::new().with(ENV_OP, op);
        for cell in layout.pages.iter().flat_map(|p| &p.cells) {
            if let Ok(Some(id)) = cell.resolved(&ctx) {
                shown.insert(id.as_str().to_owned());
            }
            if let Some(id) = &cell.carrier {
                shown.insert(id.as_str().to_owned());
            }
            if let komponist_model::params::layout::CellParam::Graph(b) = &cell.param {
                for (slot, _) in &b.bind {
                    if let Ok(Some(id)) = b.resolved(slot, &ctx) {
                        shown.insert(id.as_str().to_owned());
                    }
                }
            }
        }
    }
    for p in params() {
        assert!(shown.contains(p.info.id.as_str()), "{} is not on the card", p.info.id);
    }
}

#[test]
fn carriers_follow_the_algorithm() {
    // After the Algorithmus macro every operator's `output` flag says what the algorithm says.
    for number in 1..=32 {
        let mut patch = default_patch();
        Macro::Algorithm.set(&mut patch, f64::from(number));
        for op in 0..6 {
            assert_eq!(
                patch.operators[op].output,
                device::is_carrier(number as usize, op),
                "algorithm {number}, operator {}",
                op + 1
            );
        }
    }
}
