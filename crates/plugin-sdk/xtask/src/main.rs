//! Bundle and independently scan the native CLAP SDK example.

mod bundle;
mod scan;

use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const PLUGIN_ID: &str = "org.klang-studio.komponist.native-clap-example";
const PLUGIN_PACKAGE: &str = "native-clap-example";
const PLUGIN_BINARY: &str = "native_clap_example";
const BUNDLE_NAME: &str = "native_clap_example.clap";
const EXTENSION_ID: &std::ffi::CStr = c"org.klang-studio.komponist.device-layout/1";
const USAGE: &str = "usage: native-clap-xtask bundle [--release] [--target TRIPLE] [--no-default-features] [--out-dir DIR]\n\
       native-clap-xtask validate BUNDLE [--plain]\n\
       native-clap-xtask metadata BUNDLE ARCHIVE TARGET SOURCE_REV OUTPUT";

fn main() {
    if let Err(error) = run() {
        eprintln!("native-clap-xtask: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match arguments.as_slice() {
        [command, options @ ..] if command == "bundle" => {
            let bundle = bundle::build(bundle::Options::parse(options)?)?;
            println!("{}", bundle.display());
        }
        [command, path, rest @ ..] if command == "validate" => {
            let plain = match rest {
                [] => false,
                [flag] if flag == "--plain" => true,
                _ => return Err(USAGE.into()),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&scan::example(Path::new(path), plain)?)?
            );
        }
        [command, bundle, archive, target, source_rev, output] if command == "metadata" => {
            let metadata = metadata(Path::new(bundle), Path::new(archive), target, source_rev)?;
            fs::write(
                output,
                format!("{}\n", serde_json::to_string_pretty(&metadata)?),
            )?;
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn metadata(bundle: &Path, archive: &Path, target: &str, source_rev: &str) -> Result<Value> {
    if target != host_target()? {
        return Err(format!("target {target} does not match the native scan host").into());
    }
    if source_rev.len() != 40 || !source_rev.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("source revision must be a full 40-character Git commit ID".into());
    }
    let scan = scan::example(bundle, false)?;
    let mut file = File::open(archive)?;
    if !file.metadata()?.is_file() {
        return Err(format!("archive is not a regular file: {}", archive.display()).into());
    }
    let mut digest = Sha256::new();
    let mut bytes = [0_u8; 32 * 1024];
    let mut size_bytes = 0_u64;
    loop {
        let size = file.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        size_bytes += size as u64;
        digest.update(&bytes[..size]);
    }
    if size_bytes == 0 {
        return Err("package archive is empty".into());
    }
    let filename = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("archive filename must be UTF-8")?;
    let sha256: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(json!({
        "schema_version": 1,
        "plugin": { "id": scan["plugin"]["id"], "version": scan["plugin"]["version"] },
        "target": target,
        "layout_compatibility": {
            "extension_id": EXTENSION_ID.to_str()?,
            "schema_version": scan["layout"]["schema_version"],
        },
        "source_revision": source_rev,
        "license": env!("CARGO_PKG_LICENSE"),
        "package": { "file": filename, "size_bytes": size_bytes, "sha256": sha256 },
    }))
}

fn host_target() -> Result<String> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output()?;
    if !output.status.success() {
        return Err("rustc -vV failed".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc did not report its host target")?
        .to_owned())
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is under the SDK workspace")
        .to_owned()
}

fn cargo() -> Command {
    let executable = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(executable);
    command.current_dir(workspace());
    command
}
