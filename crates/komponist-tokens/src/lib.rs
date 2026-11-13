//! Design tokens of komponist as Rust constants.
//!
//! `design/tokens.json` is the single source of truth. A build script turns it into colours, sizes,
//! spacing, radii, type styles and the track hue list; this crate also converts oklch to sRGB and
//! bundles the fonts. No other crate hard-codes a design value.
//!
//! Scaffold only: the generated constants arrive with WP-02.
