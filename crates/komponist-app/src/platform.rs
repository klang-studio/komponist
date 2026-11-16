//! The desktop platform as far as the UI text and the shortcut modifier differ (macOS issue #47).
//! The type lives in `komponist-ui` (widget tooltips pick per platform too); this is its path in
//! the app.

pub use komponist_ui::platform::Platform;
