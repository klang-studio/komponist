//! `komponist-bridge`: one process per hosted plugin instance.
//!
//! Instance mode (`--instance`) runs the plugin with an RT thread, a poll loop for the control
//! socket, plugin fds and timers, and the plugin GUI. Scan mode (`--scan <bundle>`) prints the
//! bundle's descriptors as JSON. A crashing plugin takes only its own bridge down.
//!
//! Scaffold only: the implementation arrives with WP-31.

fn main() {
    println!("komponist-bridge {} (scaffold)", env!("CARGO_PKG_VERSION"));
}
