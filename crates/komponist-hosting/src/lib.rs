//! The plugin format layer of komponist: CLAP and VST3 hosting and scanning.
//!
//! Everything behind the `HostedPlugin` trait. Only `komponist-bridge` links this crate, so plugin
//! code never runs inside the app process.
//!
//! Scaffold only: the contracts arrive with WP-01, the formats with WP-26 (CLAP) and WP-32 (VST3).
