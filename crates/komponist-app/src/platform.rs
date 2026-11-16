//! The desktop platform as far as the UI text and the shortcut modifier differ (macOS issue #47).
//!
//! Shortcut labels and the hints that name a modifier exist once per platform in the catalogs
//! (`key-*-mac`, `*-hint-mac`); this module only says which one applies. It is a value, not a
//! `cfg`, so tests can render both.

/// Where the app runs, as far as shortcut labels care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Linux (and anything that is not macOS): shortcuts use Ctrl, labels are written out
    /// ("Strg S", "Ctrl+S").
    Pc,
    /// macOS: shortcuts use ⌘, labels are glyphs ("⌘S", "⇧⌘Z").
    Mac,
}

impl Platform {
    /// The platform this build runs on.
    pub const fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Pc
        }
    }

    /// Whether this is macOS.
    pub const fn is_mac(self) -> bool {
        matches!(self, Platform::Mac)
    }

    /// `pc` on Linux, `mac` on macOS: the text of the platform's modifier.
    pub fn pick<T>(self, pc: T, mac: T) -> T {
        match self {
            Platform::Pc => pc,
            Platform::Mac => mac,
        }
    }
}
