//! Generates the token constants from `design/tokens.json` and prepares the icons from
//! `assets/icons/`. The work is done in `build/codegen.rs`, which the tests share.

#[path = "build/codegen.rs"]
mod codegen;

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let root = manifest.join("../..");
    let tokens_path = root.join("design/tokens.json");
    let icons_dir = root.join("assets/icons");

    println!("cargo:rerun-if-changed=build/codegen.rs");
    println!("cargo:rerun-if-changed={}", tokens_path.display());
    println!("cargo:rerun-if-changed={}", icons_dir.display());

    let text = fs::read_to_string(&tokens_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", tokens_path.display()));
    let tokens: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", tokens_path.display()));
    let generated =
        codegen::generate(&tokens).unwrap_or_else(|e| panic!("{}: {e}", tokens_path.display()));
    for (name, source) in &generated.files {
        write(&out.join(name), source.as_bytes());
    }

    let icons_out = out.join("icons");
    fs::create_dir_all(&icons_out).expect("create OUT_DIR/icons");
    let mut entries: Vec<_> = fs::read_dir(&icons_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", icons_dir.display()))
        .map(|entry| entry.expect("read assets/icons entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
        .collect();
    entries.sort();
    for path in entries {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("UTF-8 icon name");
        let svg = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let prepare = |filled| {
            codegen::prepare_icon(&svg, generated.icon_size, codegen::ICON_STROKE_PX, filled)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        };
        write(
            &icons_out.join(format!("{name}.svg")),
            prepare(false).as_bytes(),
        );
        if codegen::FILLED_ICONS.contains(&name) {
            write(
                &icons_out.join(format!("{name}-filled.svg")),
                prepare(true).as_bytes(),
            );
        }
    }
}

/// Writes only when the content changed, so dependants do not rebuild needlessly.
fn write(path: &Path, content: &[u8]) {
    if fs::read(path).is_ok_and(|old| old == content) {
        return;
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}
