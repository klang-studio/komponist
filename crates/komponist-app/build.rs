//! Platform cfgs of this crate (ARCHITECTURE §4.1, "Platform cfgs"), set from the target OS:
//!
//! - `bridge_frame` (macOS): the plugin bridge owns the plugin's frame window (`GuiTarget::OwnFrame`,
//!   #52); the app only sends the header model and handles the actions it gets back. The Windows
//!   port joins here in W6.
//! - `x11_embed` (Linux): the app owns the frame window and the plugin attaches to an X11 child of
//!   it (`GuiTarget::X11Child`, WP-50), including the key grabs.
//!
//! - Windows: icon, version information and manifest of `komponist.exe` (`windows_resources`, ARCHITECTURE §8.8).
//!
//! Plain `target_os` stays where the code is about one OS's API (AppKit menu and delegate, the
//! activation hand-over).

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(bridge_frame)");
    println!("cargo::rustc-check-cfg=cfg(x11_embed)");
    match std::env::var("CARGO_CFG_TARGET_OS")
        .unwrap_or_default()
        .as_str()
    {
        "linux" => println!("cargo::rustc-cfg=x11_embed"),
        "macos" => println!("cargo::rustc-cfg=bridge_frame"),
        "windows" => windows_resources(),
        _ => {}
    }
}

/// Compiles `assets/windows/komponist.rc.in` (icon, version information, manifest) with the MSVC `rc.exe` and
/// links it into the `komponist` binary only (not `komponist-play`). `embed-resource` is a build dependency of
/// Windows hosts only, so this is a no-op everywhere else.
#[cfg(windows)]
fn windows_resources() {
    use std::path::PathBuf;

    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let assets = manifest_dir.join("../../assets/windows");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    // "0.2.0-rc.1" -> "0,2,0,0": the numeric FILEVERSION has four numbers, a pre-release suffix is dropped.
    let mut numbers: Vec<&str> = version
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .split('.')
        .collect();
    numbers.resize(4, "0");
    let path = |name: &str| {
        // rc.exe takes forward slashes; backslashes would need escaping inside the string.
        assets
            .join(name)
            .canonicalize()
            .unwrap_or_else(|e| panic!("assets/windows/{name}: {e}"))
            .display()
            .to_string()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    };
    println!("cargo::rerun-if-changed=../../assets/windows");
    let template =
        std::fs::read_to_string(assets.join("komponist.rc.in")).expect("komponist.rc.in");
    let rc = template
        .replace("@VERSION_NUM@", &numbers.join(","))
        .replace("@VERSION@", &version)
        .replace("@ICON@", &path("komponist.ico"))
        .replace("@MANIFEST@", &path("komponist.manifest"));
    let rc_file = out.join("komponist.rc");
    std::fs::write(&rc_file, rc).expect("write komponist.rc");
    embed_resource::compile_for(&rc_file, ["komponist"], embed_resource::NONE)
        .manifest_required()
        .expect("compile the Windows resources");
}

#[cfg(not(windows))]
fn windows_resources() {}
