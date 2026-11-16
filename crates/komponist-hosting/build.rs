//! Platform cfgs of this crate (ARCHITECTURE §4.1, "Platform cfgs"), set from the target OS:
//!
//! - `x11_embed` (Linux): plugin GUIs are embedded as X11 children of a window the app owns
//!   (`GuiParent::X11`, CLAP `x11`, VST3 `kPlatformTypeX11EmbedWindowID`).
//!
//! Plain `target_os` stays where the code is about one OS's API (AppKit, `dlopen`, bundle layouts).

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(x11_embed)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        println!("cargo::rustc-cfg=x11_embed");
    }
}
