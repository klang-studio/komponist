//! Generates the token constants from `design/tokens.json`. The work is done in
//! `build/codegen.rs`, which the tests share.

#[path = "build/codegen.rs"]
mod codegen;

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let root = manifest.join("../..");
    let tokens_path = root.join("design/tokens.json");

    println!("cargo:rerun-if-changed=build/codegen.rs");
    println!("cargo:rerun-if-changed={}", tokens_path.display());

    let text = fs::read_to_string(&tokens_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", tokens_path.display()));
    let tokens: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", tokens_path.display()));
    let generated =
        codegen::generate(&tokens).unwrap_or_else(|e| panic!("{}: {e}", tokens_path.display()));
    for (name, source) in &generated.files {
        write(&out.join(name), source.as_bytes());
    }
}

/// Writes only when the content changed, so dependants do not rebuild needlessly.
fn write(path: &Path, content: &[u8]) {
    if fs::read(path).is_ok_and(|old| old == content) {
        return;
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
}
