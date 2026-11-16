//! Platform cfgs of this crate (ARCHITECTURE §8.4, "Platform cfgs"), set from the target OS:
//!
//! - `bridge_frame` (macOS): the plugin bridge owns the plugin's frame window (`GuiTarget::OwnFrame`,
//!   #52); the app only sends the header model and handles the actions it gets back. The Windows
//!   port joins here in W6.
//! - `x11_embed` (Linux): the app owns the frame window and the plugin attaches to an X11 child of
//!   it (`GuiTarget::X11Child`, WP-50), including the key grabs.
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
        _ => {}
    }
}
