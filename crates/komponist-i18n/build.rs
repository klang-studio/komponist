//! Generates the typed text catalog from `locales/<lang>/*.ftl` (`generator.rs`, `docs/I18N.md`).
//!
//! `locales/` becomes `$OUT_DIR/catalog.rs`; `tests/fixtures/locales/` becomes
//! `$OUT_DIR/fixture_catalog.rs`, which the tests compile to try plurals, variables and terms.
//! On errors the build stops and prints every one as `path:line: message`.

#[path = "generator.rs"]
mod generator;

use std::path::{Path, PathBuf};

use generator::{Locale, SourceFile};

fn read_locales(dir: &Path) -> std::io::Result<Vec<Locale>> {
    let mut locales = Vec::new();
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    folders.sort();
    for folder in folders.into_iter().filter(|p| p.is_dir()) {
        let code = folder.file_name().unwrap().to_string_lossy().into_owned();
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&folder)?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()?;
        paths.sort();
        let mut files = Vec::new();
        for path in paths
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "ftl"))
        {
            let name = path.file_name().unwrap().to_string_lossy();
            files.push(SourceFile {
                path: format!("{}/{code}/{name}", dir.display()),
                text: std::fs::read_to_string(&path)?,
            });
        }
        locales.push(Locale { code, files });
    }
    Ok(locales)
}

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=generator.rs");
    let mut failed = false;
    for (dir, file) in [
        ("locales", "catalog.rs"),
        ("tests/fixtures/locales", "fixture_catalog.rs"),
    ] {
        println!("cargo:rerun-if-changed={dir}");
        let locales = read_locales(Path::new(dir)).unwrap_or_else(|e| panic!("{dir}: {e}"));
        match generator::generate(&locales) {
            Ok(code) => std::fs::write(out.join(file), code).expect("write catalog"),
            Err(errors) => {
                failed = true;
                eprintln!("komponist-i18n: {} error(s) in {dir}:", errors.len());
                for e in errors {
                    eprintln!("error: {e}");
                }
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}
