//! Tests of the build-time generator (`build/codegen.rs`): the real token file generates, and a
//! token the generator does not know fails instead of being dropped.

#[allow(dead_code)]
#[path = "../build/codegen.rs"]
mod codegen;

use serde_json::{Value, json};

fn tokens() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../design/tokens.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Applies `edit` to the real tokens and returns the generator's error.
fn error_after(edit: impl FnOnce(&mut Value)) -> String {
    let mut t = tokens();
    edit(&mut t);
    codegen::generate(&t).expect_err("generator accepted a broken token file")
}

#[test]
fn real_tokens_generate() {
    let generated = codegen::generate(&tokens()).unwrap();
    assert_eq!(generated.icon_size, 16.0);
    let names: Vec<_> = generated.files.keys().copied().collect();
    assert_eq!(
        names,
        [
            "color.rs",
            "focus.rs",
            "font.rs",
            "line.rs",
            "radius.rs",
            "select.rs",
            "shadow.rs",
            "size.rs",
            "space.rs",
            "track.rs",
            "typography.rs"
        ]
    );
}

#[test]
fn unknown_group_fails() {
    let e = error_after(|t| t["opacity"] = json!({ "disabled": 0.4 }));
    assert!(e.contains("unknown token group `opacity`"), "{e}");
}

#[test]
fn missing_group_fails() {
    let e = error_after(|t| {
        t.as_object_mut().unwrap().remove("radius");
    });
    assert!(e.contains("`radius` is missing"), "{e}");
}

#[test]
fn unknown_size_value_fails() {
    let e = error_after(|t| t["size"]["panel-header"] = json!("28pt"));
    assert!(e.contains("size.panel-header"), "{e}");
    let e = error_after(|t| t["size"]["toggle"] = json!([18, 16, 2]));
    assert!(e.contains("size.toggle"), "{e}");
    let e = error_after(|t| t["size"]["dock"]["max"] = json!("70%"));
    assert!(e.contains("size.dock.max"), "{e}");
}

#[test]
fn bad_colour_fails() {
    let e = error_after(|t| t["color"]["text"]["1"] = json!("rgb(1,2,3)"));
    assert!(e.contains("color.text.1"), "{e}");
    let e = error_after(|t| t["color"]["surface"]["0"] = json!("#11121"));
    assert!(e.contains("color.surface.0"), "{e}");
}

#[test]
fn unknown_colour_reference_fails() {
    let e = error_after(|t| t["type"]["small"]["color"] = json!("text.9"));
    assert!(e.contains("unknown colour token `text.9`"), "{e}");
    let e = error_after(|t| t["focus"]["outline"] = json!("1px accnt"));
    assert!(e.contains("accnt"), "{e}");
}

#[test]
fn unknown_type_field_fails() {
    let e = error_after(|t| t["type"]["ui"]["italic"] = json!(true));
    assert!(e.contains("unknown key `type.ui.italic`"), "{e}");
    let e = error_after(|t| t["type"]["ui"]["family"] = json!("serif"));
    assert!(e.contains("unknown family `serif`"), "{e}");
    let e = error_after(|t| t["type"]["label"]["case"] = json!("lower"));
    assert!(e.contains("unknown case"), "{e}");
}

#[test]
fn unknown_or_changed_formula_fails() {
    let e = error_after(|t| t["track"]["$formula"]["key"] = json!("oklch(0.82 0.1 h)"));
    assert!(e.contains("unknown formula `track.$formula.key`"), "{e}");
    let e = error_after(|t| t["track"]["$formula"]["base"] = json!("oklch(0.70 0.12 h / 0.5)"));
    assert!(e.contains("track.$formula.base"), "{e}");
    let e = error_after(|t| {
        t["track"]["$formula"]
            .as_object_mut()
            .unwrap()
            .remove("note-hover");
    });
    assert!(e.contains("note-hover` is missing"), "{e}");
}

#[test]
fn komponist_additions_generate() {
    let generated = codegen::generate(&tokens()).unwrap();
    let track = &generated.files["track.rs"];
    for line in [
        "pub const KEY_PLAYING: LightnessChroma = LightnessChroma { l: 0.82, c: 0.1 };",
        "pub const NOTE_MUTED: LightnessChroma = LightnessChroma { l: 0.55, c: 0.08 };",
        "pub const CLIP_HANDLE: LightnessChroma = LightnessChroma { l: 0.4, c: 0.07 };",
    ] {
        assert!(track.contains(line), "{line} missing in {track}");
    }
    let color = &generated.files["color.rs"];
    for line in [
        "pub const LINE_0: Srgba = [0x23, 0x24, 0x27, 0xff];",
        "pub const LINE: [Srgba; 3] = [LINE_0, LINE_1, LINE_2];",
        "pub const STATE_RECORD_INK: Srgba = [0xf2, 0xa6, 0xa8, 0xff];",
        "pub const CANVAS_LANE_SELECTED: Srgba = [0x1a, 0x1c, 0x1f, 0xff];",
        "pub const CANVAS_BLACK_KEY_ROW: Srgba = [0x1a, 0x1b, 0x1d, 0xff];",
        // rgba(17,18,20,0.6): alpha 0.6 × 255 = 153.
        "pub const CANVAS_OUTSIDE_CLIP: Srgba = [0x11, 0x12, 0x14, 0x99];",
        "pub const CANVAS_STEP_GROUP_ALT: Srgba = [0x22, 0x23, 0x26, 0xff];",
    ] {
        assert!(color.contains(line), "{line} missing in {color}");
    }
}

#[test]
fn komponist_formulas_are_required() {
    for key in ["key-playing", "note-muted", "clip-handle"] {
        let e = error_after(|t| {
            t["track"]["$formula"].as_object_mut().unwrap().remove(key);
        });
        assert!(
            e.contains(&format!("`track.$formula.{key}` is missing")),
            "{e}"
        );
        let e = error_after(|t| t["track"]["$formula"][key] = json!("oklch(0.5 0.1 250)"));
        assert!(e.contains(&format!("track.$formula.{key}")), "{e}");
    }
}

#[test]
fn auto_assign_hue_must_exist() {
    let e = error_after(|t| t["track"]["auto-assign-hue-order"][0] = json!(151));
    assert!(e.contains("hue 151 is not in `track.colors`"), "{e}");
}

#[test]
fn font_numbers_rule_is_checked() {
    let e = error_after(|t| t["font"]["numbers"] = json!("proportional"));
    assert!(e.contains("font.numbers"), "{e}");
}

#[test]
fn meta_units_are_checked() {
    let e = error_after(|t| t["$meta"]["units"] = json!("px @2x"));
    assert!(e.contains("$meta.units"), "{e}");
}

#[test]
fn shadow_parsing() {
    let mut t = tokens();
    t["shadow"]["test"] = json!("1px -2px 3px 4px #102030");
    let generated = codegen::generate(&t).unwrap();
    let shadow = &generated.files["shadow.rs"];
    assert!(
        shadow.contains(
            "TEST: BoxShadow = BoxShadow { offset: [1.0, -2.0], blur: 3.0, spread: 4.0, \
             color: [0x10, 0x20, 0x30, 0xff] }"
        ),
        "{shadow}"
    );
    assert!(
        shadow.contains("color: [0x00, 0x00, 0x00, 0x73]"),
        "rgba alpha 0.45 → 115: {shadow}"
    );
    let e = error_after(|t| t["shadow"]["float"] = json!("inset 0 1px #000000"));
    assert!(e.contains("shadow.float"), "{e}");
}

#[test]
fn prepare_icon_rewrites_the_root_attributes() {
    let svg = "<!-- c -->\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" \
               viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\">\
               <path d=\"M1 1h2\" /></svg>";
    let out = codegen::prepare_icon(svg, 16.0, 1.5, false).unwrap();
    assert_eq!(
        out,
        "<!-- c -->\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" \
         viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"2.25\">\
         <path d=\"M1 1h2\" /></svg>"
    );
    let filled = codegen::prepare_icon(svg, 16.0, 1.5, true).unwrap();
    assert!(filled.contains("fill=\"#ffffff\""), "{filled}");
}

#[test]
fn prepare_icon_rejects_unexpected_files() {
    let base = "<svg width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" \
                stroke=\"currentColor\" stroke-width=\"2\"><path d=\"M1 1\" /></svg>";
    assert!(codegen::prepare_icon(base, 16.0, 1.5, false).is_ok());
    for broken in [
        base.replace("viewBox=\"0 0 24 24\"", "viewBox=\"0 0 16 16\""),
        base.replace("stroke-width=\"2\"", "stroke-width=\"1.5\""),
        base.replace(" stroke=\"currentColor\"", ""),
        base.replace("<path", "<path stroke=\"currentColor\""),
        "<g />".to_string(),
    ] {
        assert!(
            codegen::prepare_icon(&broken, 16.0, 1.5, false).is_err(),
            "{broken}"
        );
    }
}
