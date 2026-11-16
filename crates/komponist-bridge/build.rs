//! Platform cfgs of this crate (ARCHITECTURE §4.1, "Platform cfgs"), set from the target OS:
//!
//! - `bridge_frame` (macOS, Windows): the bridge owns the plugin's frame window
//!   (`GuiTarget::OwnFrame`, the `frame` module; #52 on macOS, port W6 on Windows) and the app only
//!   sends the header model and receives its actions.
//! - `x11_embed` (Linux): the plugin attaches to an X11 window the app owns
//!   (`GuiTarget::X11Child`); the bridge has no frame of its own.
//!
//! Plain `target_os` stays where the code is about one OS's API (the main loop, scheduling, core
//! dumps, AppKit, Win32).

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(bridge_frame)");
    println!("cargo::rustc-check-cfg=cfg(x11_embed)");
    match std::env::var("CARGO_CFG_TARGET_OS")
        .unwrap_or_default()
        .as_str()
    {
        "linux" => println!("cargo::rustc-cfg=x11_embed"),
        "macos" | "windows" => println!("cargo::rustc-cfg=bridge_frame"),
        _ => {}
    }
}
