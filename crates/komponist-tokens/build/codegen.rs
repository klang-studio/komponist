//! Code generation for `komponist-tokens`.
//!
//! Turns `design/tokens.json` into Rust source (one file per token module). Used by `build.rs`; `tests/codegen.rs` includes this file too, so the generator
//! can be tested with broken token files.
//!
//! The generator is strict on purpose: a token group, key or value shape it does not know is an
//! error, so a change to `tokens.json` fails the build instead of being dropped silently.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{Map, Value};

pub type Result<T> = std::result::Result<T, String>;

/// What `generate` produces.
#[derive(Debug)]
pub struct Generated {
    /// Rust source per file name in `OUT_DIR`.
    pub files: BTreeMap<&'static str, String>,
}

/// Generates the Rust sources for all token groups.
pub fn generate(tokens: &Value) -> Result<Generated> {
    let root = obj(tokens, "tokens.json")?;
    for key in root.keys() {
        if !matches!(
            key.as_str(),
            "$meta"
                | "color"
                | "track"
                | "font"
                | "type"
                | "space"
                | "radius"
                | "line"
                | "focus"
                | "select"
                | "shadow"
                | "size"
        ) {
            return Err(format!("unknown token group `{key}`"));
        }
    }
    let field = |key: &str| {
        root.get(key)
            .ok_or_else(|| format!("token group `{key}` is missing"))
    };

    check_meta(field("$meta")?)?;
    let colors = obj(field("color")?, "color")?;

    let mut files = BTreeMap::new();
    files.insert("color.rs", gen_color(colors)?);
    files.insert("track.rs", gen_track(field("track")?)?);
    files.insert("font.rs", gen_font(field("font")?)?);
    files.insert("typography.rs", gen_typography(field("type")?, colors)?);
    files.insert("space.rs", gen_space(field("space")?)?);
    files.insert("radius.rs", gen_radius(field("radius")?)?);
    files.insert("line.rs", gen_line(field("line")?)?);
    files.insert("focus.rs", gen_focus(field("focus")?, colors)?);
    files.insert("select.rs", gen_select(field("select")?, colors)?);
    files.insert("shadow.rs", gen_shadow(field("shadow")?, colors)?);
    files.insert("size.rs", gen_size(field("size")?)?);

    Ok(Generated { files })
}

// ---------------------------------------------------------------------------------------------
// Groups

fn check_meta(meta: &Value) -> Result<()> {
    for (key, value) in obj(meta, "$meta")? {
        let path = format!("$meta.{key}");
        let value = string(value, &path)?;
        match key.as_str() {
            "source" => {}
            "theme" if value == "dark" => {}
            "units" if value == "px @1x" => {}
            "theme" | "units" => return Err(format!("`{path}`: unsupported value `{value}`")),
            _ => return Err(format!("unknown key `{path}`")),
        }
    }
    Ok(())
}

fn gen_color(colors: &Map<String, Value>) -> Result<String> {
    let mut out = String::new();
    for (group, entries) in colors {
        let entries = obj(entries, &format!("color.{group}"))?;
        for (key, value) in entries {
            let path = format!("color.{group}.{key}");
            let hex = string(value, &path)?;
            let rgba = parse_hex(hex, &path)?;
            doc(&mut out, &format!("`{path}` = `{hex}`."));
            writeln!(
                out,
                "pub const {}: Srgba = {};",
                color_const(group, key)?,
                rgba_lit(rgba)
            )
            .unwrap();
        }
        // A scale 0..n also becomes an array, for the "+1 / +2 steps" state rules.
        let n = entries.len();
        if n > 1 && (0..n).all(|i| entries.contains_key(&i.to_string())) {
            let name = const_ident(group)?;
            doc(
                &mut out,
                &format!("`color.{group}.*` indexed by step: `{name}[i]` = `{name}_i`."),
            );
            let items: Vec<String> = (0..n).map(|i| format!("{name}_{i}")).collect();
            writeln!(
                out,
                "pub const {name}: [Srgba; {n}] = [{}];",
                items.join(", ")
            )
            .unwrap();
        }
    }
    Ok(out)
}

fn gen_track(track: &Value) -> Result<String> {
    let track = obj(track, "track")?;
    for key in track.keys() {
        if !matches!(
            key.as_str(),
            "$formula" | "auto-assign-hue-order" | "colors"
        ) {
            return Err(format!("unknown key `track.{key}`"));
        }
    }
    let field = |key: &str| {
        track
            .get(key)
            .ok_or_else(|| format!("`track.{key}` is missing"))
    };
    let mut out = String::new();

    // Formulas.
    let formula = obj(field("$formula")?, "track.$formula")?;
    let mut seen = Vec::new();
    out.push_str(
        "/// The oklch formulas of `track.$formula`; `h` is the track hue.\npub mod formula {\n",
    );
    out.push_str("    use super::{LightnessChroma, NoteFormula};\n");
    for (key, value) in formula {
        let path = format!("track.$formula.{key}");
        let text = string(value, &path)?;
        let name = const_ident(key)?;
        match key.as_str() {
            "base" | "body" | "dim" | "ink" | "ink-dim" | "note-playing" | "clip-head-hover" => {
                let (l, c) = parse_oklch_lc(text, &path)?;
                writeln!(out, "    /// `{path}` = `{text}`.").unwrap();
                writeln!(
                    out,
                    "    pub const {name}: LightnessChroma = LightnessChroma {{ l: {}, c: {} }};",
                    f64_lit(l),
                    f64_lit(c)
                )
                .unwrap();
            }
            "note" => {
                let (l_min, l_range, c) = parse_note_formula(text, &path)?;
                writeln!(out, "    /// `{path}` = `{text}`.").unwrap();
                writeln!(
                    out,
                    "    pub const NOTE: NoteFormula = NoteFormula {{ l_min: {}, l_range: {}, c: {} }};",
                    f64_lit(l_min),
                    f64_lit(l_range),
                    f64_lit(c)
                )
                .unwrap();
            }
            "note-hover" => {
                let delta = parse_note_hover(text, &path)?;
                writeln!(
                    out,
                    "    /// `{path}` = `{text}`: lightness added to the note colour."
                )
                .unwrap();
                writeln!(
                    out,
                    "    pub const NOTE_HOVER_L_DELTA: f64 = {};",
                    f64_lit(delta)
                )
                .unwrap();
            }
            _ => return Err(format!("unknown formula `{path}`")),
        }
        seen.push(key.as_str());
    }
    for key in [
        "base",
        "body",
        "dim",
        "ink",
        "ink-dim",
        "note",
        "note-hover",
        "note-playing",
        "clip-head-hover",
    ] {
        if !seen.contains(&key) {
            return Err(format!("`track.$formula.{key}` is missing"));
        }
    }
    out.push_str("}\n");

    // Hues and precomputed colours, in ascending hue (the order of the design system).
    let colors = obj(field("colors")?, "track.colors")?;
    let mut hues = Vec::new();
    for (name, entry) in colors {
        let path = format!("track.colors.{name}");
        let entry = obj(entry, &path)?;
        let hue = uint(
            entry
                .get("hue")
                .ok_or_else(|| format!("`{path}.hue` is missing"))?,
            &format!("{path}.hue"),
        )?;
        if hue >= 360 {
            return Err(format!("`{path}.hue`: {hue} is not below 360"));
        }
        let mut fixed = BTreeMap::new();
        let mut notes = Vec::new();
        for (key, value) in entry {
            let path = format!("{path}.{key}");
            match key.as_str() {
                "hue" => {}
                "base" | "body" | "dim" | "ink" | "ink-dim" | "play" => {
                    fixed.insert(key.as_str(), parse_hex(string(value, &path)?, &path)?);
                }
                "note-vel" => {
                    for (vel, value) in obj(value, &path)? {
                        let path = format!("{path}.{vel}");
                        let vel: u8 = vel
                            .parse()
                            .ok()
                            .filter(|v| *v <= 127)
                            .ok_or_else(|| format!("`{path}`: velocity must be 0..=127"))?;
                        notes.push((vel, parse_hex(string(value, &path)?, &path)?));
                    }
                }
                _ => return Err(format!("unknown key `{path}`")),
            }
        }
        notes.sort_by_key(|(vel, _)| *vel);
        hues.push((hue, name.as_str(), fixed, notes));
    }
    hues.sort_by_key(|(hue, ..)| *hue);
    if hues.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err("`track.colors`: two colours share a hue".into());
    }

    let n = hues.len();
    doc(
        &mut out,
        "The track hues of `track.colors`, in ascending hue.",
    );
    writeln!(out, "pub const HUES: [Hue; {n}] = [").unwrap();
    for (hue, name, ..) in &hues {
        writeln!(out, "    Hue {{ name: {name:?}, degrees: {hue} }},").unwrap();
    }
    out.push_str("];\n");

    let order = arr(
        field("auto-assign-hue-order")?,
        "track.auto-assign-hue-order",
    )?;
    let mut order_lits = Vec::new();
    for (i, value) in order.iter().enumerate() {
        let path = format!("track.auto-assign-hue-order[{i}]");
        let hue = uint(value, &path)?;
        if !hues.iter().any(|(h, ..)| *h == hue) {
            return Err(format!("`{path}`: hue {hue} is not in `track.colors`"));
        }
        order_lits.push(hue.to_string());
    }
    doc(
        &mut out,
        "`track.auto-assign-hue-order`: the hues new tracks get, in this order.",
    );
    writeln!(
        out,
        "pub const AUTO_ASSIGN_HUE_ORDER: [u16; {}] = [{}];",
        order_lits.len(),
        order_lits.join(", ")
    )
    .unwrap();

    doc(
        &mut out,
        "The colours `tokens.json` lists precomputed per hue (`track.colors`), same order as [`HUES`].\n\
         The UI computes its colours with [`TrackPalette`]; these are the reference \
         values the tests check it against.",
    );
    writeln!(out, "pub const PRECOMPUTED: [PrecomputedColors; {n}] = [").unwrap();
    for (hue, name, fixed, notes) in &hues {
        let get = |key: &str| {
            fixed
                .get(key)
                .copied()
                .ok_or_else(|| format!("`track.colors.{name}.{key}` is missing"))
        };
        writeln!(out, "    PrecomputedColors {{").unwrap();
        writeln!(out, "        hue: {hue},").unwrap();
        for key in ["base", "body", "dim", "ink", "ink-dim", "play"] {
            writeln!(
                out,
                "        {}: {},",
                key.replace('-', "_"),
                rgba_lit(get(key)?)
            )
            .unwrap();
        }
        let notes: Vec<String> = notes
            .iter()
            .map(|(v, c)| format!("({v}, {})", rgba_lit(*c)))
            .collect();
        writeln!(out, "        note_velocity: &[{}],", notes.join(", ")).unwrap();
        writeln!(out, "    }},").unwrap();
    }
    out.push_str("];\n");
    Ok(out)
}

fn gen_font(font: &Value) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(font, "font")? {
        let path = format!("font.{key}");
        let text = string(value, &path)?;
        match key.as_str() {
            "sans" | "mono" => {
                doc(
                    &mut out,
                    &format!("`{path}`: family name of the bundled {key} font."),
                );
                writeln!(
                    out,
                    "pub const {}_FAMILY: &str = {text:?};",
                    const_ident(key)?
                )
                .unwrap();
            }
            // Holds by construction: the bundled fonts have tabular digits (assets/fonts/README.md).
            "numbers" if text == "tabular-nums everywhere" => {}
            "numbers" => {
                return Err(format!(
                    "`{path}`: unsupported value `{text}` (only `tabular-nums everywhere`)"
                ));
            }
            _ => return Err(format!("unknown key `{path}`")),
        }
    }
    Ok(out)
}

fn gen_typography(types: &Value, colors: &Map<String, Value>) -> Result<String> {
    let mut out = String::new();
    let mut all = Vec::new();
    for (key, style) in obj(types, "type")? {
        let path = format!("type.{key}");
        let style = obj(style, &path)?;
        let mut family = None;
        let mut size = None;
        let mut line = None;
        let mut weight = None;
        let mut emphasis = None;
        let mut uppercase = false;
        let mut tracking = 0.0;
        let mut color = None;
        for (field, value) in style {
            let path = format!("{path}.{field}");
            match field.as_str() {
                "family" => {
                    family = Some(match string(value, &path)? {
                        "sans" => "Family::Sans",
                        "mono" => "Family::Mono",
                        other => return Err(format!("`{path}`: unknown family `{other}`")),
                    })
                }
                "size" => size = Some(num(value, &path)?),
                "line" => line = Some(num(value, &path)?),
                "weight" => match value {
                    Value::String(text) => {
                        let (a, b) = text
                            .split_once('/')
                            .ok_or_else(|| format!("`{path}`: expected `regular/emphasis`"))?;
                        weight = Some(parse_weight(a, &path)?);
                        emphasis = Some(parse_weight(b, &path)?);
                    }
                    _ => weight = Some(parse_weight(&uint(value, &path)?.to_string(), &path)?),
                },
                "case" => match string(value, &path)? {
                    "upper" => uppercase = true,
                    other => return Err(format!("`{path}`: unknown case `{other}`")),
                },
                "tracking" => {
                    let text = string(value, &path)?;
                    tracking = text
                        .strip_suffix("em")
                        .and_then(|n| n.parse::<f64>().ok())
                        .ok_or_else(|| format!("`{path}`: expected `<n>em`, got `{text}`"))?;
                }
                "color" => color = Some(color_ref(string(value, &path)?, colors, &path)?.1),
                _ => return Err(format!("unknown key `{path}`")),
            }
        }
        let missing = |what: &str| format!("`{path}.{what}` is missing");
        doc(&mut out, &format!("`{path}`."));
        writeln!(
            out,
            "pub const {}: TypeStyle = TypeStyle {{",
            const_ident(key)?
        )
        .unwrap();
        writeln!(
            out,
            "    family: {},",
            family.ok_or_else(|| missing("family"))?
        )
        .unwrap();
        writeln!(
            out,
            "    size: {},",
            f32_lit(size.ok_or_else(|| missing("size"))?)
        )
        .unwrap();
        writeln!(
            out,
            "    line_height: {},",
            f32_lit(line.ok_or_else(|| missing("line"))?)
        )
        .unwrap();
        writeln!(
            out,
            "    weight: {},",
            weight.ok_or_else(|| missing("weight"))?
        )
        .unwrap();
        writeln!(
            out,
            "    emphasis_weight: {},",
            opt(emphasis.map(|w| w.to_string()))
        )
        .unwrap();
        writeln!(out, "    uppercase: {uppercase},").unwrap();
        writeln!(out, "    tracking_em: {},", f32_lit(tracking)).unwrap();
        writeln!(out, "    color: {},", opt(color)).unwrap();
        out.push_str("};\n");
        all.push(format!("({key:?}, {})", const_ident(key)?));
    }
    doc(
        &mut out,
        "All text styles with their token names, for galleries and tests.",
    );
    writeln!(
        out,
        "pub const ALL: [(&str, TypeStyle); {}] = [{}];",
        all.len(),
        all.join(", ")
    )
    .unwrap();
    Ok(out)
}

fn gen_space(space: &Value) -> Result<String> {
    let mut out = String::new();
    let mut entries: Vec<(f64, &String, f64)> = Vec::new();
    for (key, value) in obj(space, "space")? {
        let path = format!("space.{key}");
        let step: f64 = key
            .parse()
            .map_err(|_| format!("`{path}`: key must be a number"))?;
        entries.push((step, key, num(value, &path)?));
    }
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, key, px) in entries {
        doc(&mut out, &format!("`space.{key}` = {px} px."));
        writeln!(
            out,
            "pub const SPACE_{}: f32 = {};",
            key.replace('.', "_"),
            f32_lit(px)
        )
        .unwrap();
    }
    Ok(out)
}

fn gen_radius(radius: &Value) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(radius, "radius")? {
        let path = format!("radius.{key}");
        let px = num(value, &path)?;
        doc(&mut out, &format!("`{path}` = {px} px."));
        writeln!(
            out,
            "pub const {}: f32 = {};",
            const_ident(key)?,
            f32_lit(px)
        )
        .unwrap();
    }
    Ok(out)
}

fn gen_line(line: &Value) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(line, "line")? {
        let path = format!("line.{key}");
        match key.as_str() {
            "width" => {
                let px = num(value, &path)?;
                doc(
                    &mut out,
                    &format!("`{path}` = {px} px: every line is 1 px."),
                );
                writeln!(out, "pub const WIDTH: f32 = {};", f32_lit(px)).unwrap();
            }
            _ => return Err(format!("unknown key `{path}`")),
        }
    }
    Ok(out)
}

fn gen_focus(focus: &Value, colors: &Map<String, Value>) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(focus, "focus")? {
        let path = format!("focus.{key}");
        match key.as_str() {
            "outline" => {
                let text = string(value, &path)?;
                let (width, color) = text
                    .split_once(' ')
                    .ok_or_else(|| format!("`{path}`: expected `<width>px <colour>`"))?;
                let width = parse_px(width, &path)?;
                let color = css_color(color.trim(), colors, &path)?;
                doc(
                    &mut out,
                    &format!("`{path}` = `{text}`: width of the focus ring."),
                );
                writeln!(out, "pub const OUTLINE_WIDTH: f32 = {};", f32_lit(width)).unwrap();
                doc(
                    &mut out,
                    &format!("`{path}` = `{text}`: colour of the focus ring."),
                );
                writeln!(out, "pub const OUTLINE_COLOR: Srgba = {color};").unwrap();
            }
            "offset" => {
                let px = num(value, &path)?;
                doc(
                    &mut out,
                    &format!("`{path}` = {px} px: gap between object and focus ring."),
                );
                writeln!(out, "pub const OFFSET: f32 = {};", f32_lit(px)).unwrap();
            }
            _ => return Err(format!("unknown key `{path}`")),
        }
    }
    Ok(out)
}

fn gen_select(select: &Value, colors: &Map<String, Value>) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(select, "select")? {
        let path = format!("select.{key}");
        match key.as_str() {
            "ring" => {
                let text = string(value, &path)?;
                doc(
                    &mut out,
                    &format!("`{path}` = `{text}` (CSS box-shadow): selection ring."),
                );
                writeln!(
                    out,
                    "pub const RING: BoxShadow = {};",
                    box_shadow(text, colors, &path)?
                )
                .unwrap();
            }
            _ => return Err(format!("unknown key `{path}`")),
        }
    }
    Ok(out)
}

fn gen_shadow(shadow: &Value, colors: &Map<String, Value>) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(shadow, "shadow")? {
        let path = format!("shadow.{key}");
        let text = string(value, &path)?;
        doc(&mut out, &format!("`{path}` = `{text}` (CSS box-shadow)."));
        writeln!(
            out,
            "pub const {}: BoxShadow = {};",
            const_ident(key)?,
            box_shadow(text, colors, &path)?
        )
        .unwrap();
    }
    Ok(out)
}

fn gen_size(size: &Value) -> Result<String> {
    let mut out = String::new();
    for (key, value) in obj(size, "size")? {
        let path = format!("size.{key}");
        match value {
            Value::Number(_) => {
                let px = num(value, &path)?;
                doc(&mut out, &format!("`{path}` = {px} px."));
                writeln!(
                    out,
                    "pub const {}: f32 = {};",
                    const_ident(key)?,
                    f32_lit(px)
                )
                .unwrap();
            }
            Value::Array(items) => {
                let [w, h] = items.as_slice() else {
                    return Err(format!("`{path}`: expected [width, height]"));
                };
                let (w, h) = (num(w, &path)?, num(h, &path)?);
                doc(
                    &mut out,
                    &format!("`{path}` = {w} × {h} px, as `[width, height]`."),
                );
                writeln!(
                    out,
                    "pub const {}: [f32; 2] = [{}, {}];",
                    const_ident(key)?,
                    f32_lit(w),
                    f32_lit(h)
                )
                .unwrap();
            }
            Value::Object(entries) => {
                writeln!(out, "/// `{path}.*`.\npub mod {} {{", mod_ident(key)?).unwrap();
                for (sub, value) in entries {
                    let path = format!("{path}.{sub}");
                    let name = const_ident(sub)?;
                    match value {
                        Value::String(text) if text.ends_with("vh") => {
                            let percent: f64 = text
                                .trim_end_matches("vh")
                                .parse()
                                .map_err(|_| format!("`{path}`: expected `<n>vh`, got `{text}`"))?;
                            writeln!(
                                out,
                                "    /// `{path}` = `{text}`: fraction of the window height."
                            )
                            .unwrap();
                            writeln!(
                                out,
                                "    pub const {name}_VIEWPORT_HEIGHT_FRACTION: f32 = {};",
                                f32_lit(percent / 100.0)
                            )
                            .unwrap();
                        }
                        _ => {
                            let px = num(value, &path)?;
                            writeln!(out, "    /// `{path}` = {px} px.").unwrap();
                            writeln!(out, "    pub const {name}: f32 = {};", f32_lit(px)).unwrap();
                        }
                    }
                }
                out.push_str("}\n");
            }
            _ => return Err(format!("`{path}`: unknown size value `{value}`")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Value parsing

fn obj<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| format!("`{path}`: expected an object"))
}

fn arr<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| format!("`{path}`: expected an array"))
}

fn string<'a>(value: &'a Value, path: &str) -> Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| format!("`{path}`: expected a string, got `{value}`"))
}

fn num(value: &Value, path: &str) -> Result<f64> {
    value
        .as_f64()
        .ok_or_else(|| format!("`{path}`: expected a number, got `{value}`"))
}

fn uint(value: &Value, path: &str) -> Result<u16> {
    value
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or_else(|| format!("`{path}`: expected an unsigned integer, got `{value}`"))
}

fn parse_weight(text: &str, path: &str) -> Result<u16> {
    let weight: u16 = text
        .trim()
        .parse()
        .map_err(|_| format!("`{path}`: weight `{text}` is not a number"))?;
    if !(100..=900).contains(&weight) || !weight.is_multiple_of(100) {
        return Err(format!("`{path}`: weight {weight} is not 100, 200, …, 900"));
    }
    Ok(weight)
}

fn parse_px(text: &str, path: &str) -> Result<f64> {
    if text == "0" {
        return Ok(0.0);
    }
    text.strip_suffix("px")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("`{path}`: expected a length like `6px`, got `{text}`"))
}

/// Parses `#rrggbb` or `#rrggbbaa`.
pub fn parse_hex(text: &str, path: &str) -> Result<[u8; 4]> {
    let bad = || format!("`{path}`: expected `#rrggbb` or `#rrggbbaa`, got `{text}`");
    let hex = text.strip_prefix('#').ok_or_else(bad)?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
    let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
    Ok([byte(0)?, byte(2)?, byte(4)?, alpha])
}

/// Resolves a colour token reference like `text.2`, `surface.0` or `accent` (= `accent.default`).
/// Returns the value and the Rust path of the generated constant.
fn color_ref(text: &str, colors: &Map<String, Value>, path: &str) -> Result<([u8; 4], String)> {
    let (group, key) = text.split_once('.').unwrap_or((text, "default"));
    let value = colors
        .get(group)
        .and_then(|g| g.get(key))
        .ok_or_else(|| format!("`{path}`: unknown colour token `{text}`"))?;
    let rgba = parse_hex(string(value, path)?, path)?;
    Ok((rgba, format!("crate::color::{}", color_const(group, key)?)))
}

/// A CSS colour (`#hex`, `rgb(…)`, `rgba(…)`) or a colour token reference, as a Rust expression.
fn css_color(text: &str, colors: &Map<String, Value>, path: &str) -> Result<String> {
    if text.starts_with('#') {
        return Ok(rgba_lit(parse_hex(text, path)?));
    }
    if let Some(args) = text
        .strip_prefix("rgba(")
        .or_else(|| text.strip_prefix("rgb("))
        .and_then(|t| t.strip_suffix(')'))
    {
        let parts: Vec<&str> = args.split(',').map(str::trim).collect();
        let bad = || format!("`{path}`: cannot parse colour `{text}`");
        let channel = |s: &str| s.parse::<u8>().map_err(|_| bad());
        let rgba = match parts.as_slice() {
            [r, g, b] if text.starts_with("rgb(") => [channel(r)?, channel(g)?, channel(b)?, 255],
            [r, g, b, a] if text.starts_with("rgba(") => {
                let a: f64 = a.parse().map_err(|_| bad())?;
                if !(0.0..=1.0).contains(&a) {
                    return Err(bad());
                }
                [
                    channel(r)?,
                    channel(g)?,
                    channel(b)?,
                    (a * 255.0).round() as u8,
                ]
            }
            _ => return Err(bad()),
        };
        return Ok(rgba_lit(rgba));
    }
    Ok(color_ref(text, colors, path)?.1)
}

/// Parses a CSS box-shadow `<x> <y> [<blur> [<spread>]] <colour>` into a `BoxShadow` expression.
fn box_shadow(text: &str, colors: &Map<String, Value>, path: &str) -> Result<String> {
    if (text.contains(',') && !text.contains('(')) || text.contains("inset") {
        return Err(format!(
            "`{path}`: only a single outer shadow is supported, got `{text}`"
        ));
    }
    // The colour is the last part; rgb()/rgba() may contain spaces.
    let (lengths, color) = match text.find("rgb") {
        Some(i) => (&text[..i], &text[i..]),
        None => text
            .rsplit_once(' ')
            .ok_or_else(|| format!("`{path}`: expected `<x> <y> [<blur> [<spread>]] <colour>`"))?,
    };
    let lengths: Vec<f64> = lengths
        .split_whitespace()
        .map(|l| parse_px(l, path))
        .collect::<Result<_>>()?;
    let (x, y, blur, spread) = match lengths.as_slice() {
        [x, y] => (*x, *y, 0.0, 0.0),
        [x, y, blur] => (*x, *y, *blur, 0.0),
        [x, y, blur, spread] => (*x, *y, *blur, *spread),
        _ => return Err(format!("`{path}`: expected 2 to 4 lengths, got `{text}`")),
    };
    Ok(format!(
        "BoxShadow {{ offset: [{}, {}], blur: {}, spread: {}, color: {} }}",
        f32_lit(x),
        f32_lit(y),
        f32_lit(blur),
        f32_lit(spread),
        css_color(color.trim(), colors, path)?
    ))
}

/// `oklch(<l> <c> h)` → (l, c).
fn parse_oklch_lc(text: &str, path: &str) -> Result<(f64, f64)> {
    let bad = || format!("`{path}`: expected `oklch(<l> <c> h)`, got `{text}`");
    let inner = text
        .strip_prefix("oklch(")
        .and_then(|t| t.strip_suffix(')'))
        .ok_or_else(bad)?;
    match inner.split_whitespace().collect::<Vec<_>>().as_slice() {
        [l, c, "h"] => Ok((l.parse().map_err(|_| bad())?, c.parse().map_err(|_| bad())?)),
        _ => Err(bad()),
    }
}

/// `oklch(<a> + <b>*vel/127, <c>, h)` → (a, b, c).
fn parse_note_formula(text: &str, path: &str) -> Result<(f64, f64, f64)> {
    let bad = || format!("`{path}`: expected `oklch(<a> + <b>*vel/127, <c>, h)`, got `{text}`");
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let inner = compact
        .strip_prefix("oklch(")
        .and_then(|t| t.strip_suffix(",h)"))
        .ok_or_else(bad)?;
    let (l, c) = inner.split_once(',').ok_or_else(bad)?;
    let (a, rest) = l.split_once('+').ok_or_else(bad)?;
    let b = rest.strip_suffix("*vel/127").ok_or_else(bad)?;
    let parse = |s: &str| s.parse::<f64>().map_err(|_| bad());
    Ok((parse(a)?, parse(b)?, parse(c)?))
}

/// `note L + <d>` → d.
fn parse_note_hover(text: &str, path: &str) -> Result<f64> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    compact
        .strip_prefix("noteL+")
        .and_then(|d| d.parse().ok())
        .ok_or_else(|| format!("`{path}`: expected `note L + <d>`, got `{text}`"))
}

// ---------------------------------------------------------------------------------------------
// Output helpers

fn doc(out: &mut String, text: &str) {
    for line in text.lines() {
        writeln!(out, "/// {line}").unwrap();
    }
}

/// `SCREAMING_SNAKE` identifier from a token key (`ink-dim` → `INK_DIM`, `value.l` → `VALUE_L`).
fn const_ident(key: &str) -> Result<String> {
    let ident: String = key
        .chars()
        .map(|c| {
            if c == '-' || c == '.' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    if ident.is_empty()
        || ident.starts_with(|c: char| c.is_ascii_digit())
        || !ident
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(format!("token key `{key}` does not make a Rust identifier"));
    }
    Ok(ident)
}

fn mod_ident(key: &str) -> Result<String> {
    Ok(const_ident(key)?.to_ascii_lowercase())
}

/// Constant name of colour `color.<group>.<key>`; `default` names the group itself.
fn color_const(group: &str, key: &str) -> Result<String> {
    let group = const_ident(group)?;
    Ok(match key {
        "default" => group,
        _ => format!(
            "{group}_{}",
            key.replace(['-', '.'], "_").to_ascii_uppercase()
        ),
    })
}

fn rgba_lit([r, g, b, a]: [u8; 4]) -> String {
    format!("[0x{r:02x}, 0x{g:02x}, 0x{b:02x}, 0x{a:02x}]")
}

fn f64_lit(x: f64) -> String {
    format!("{x:?}")
}

fn f32_lit(x: f64) -> String {
    format!("{:?}", x as f32)
}

fn opt(value: Option<String>) -> String {
    value.map_or_else(|| "None".to_string(), |v| format!("Some({v})"))
}
