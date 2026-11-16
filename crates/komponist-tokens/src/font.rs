//! Bundled fonts: IBM Plex Sans (400/500/600) and IBM Plex Mono (400/500/600), SIL OFL 1.1.
//!
//! The family names come from `font.*` in `tokens.json`; [`crate::typography`] picks family and
//! weight per text style. Mono 600 belongs to no text style: it sets the letters M S R I of the
//! track toggles (`tgl` in the design data, `fontWeight: 600`). All faces have tabular digits in
//! their default glyphs (equal advance for `0`–`9`), so numbers line up without OpenType
//! features, which egui does not apply. Source, versions and the check are in
//! `assets/fonts/README.md`.

include!(concat!(env!("OUT_DIR"), "/font.rs"));

/// Font family of a text style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// IBM Plex Sans: UI text.
    Sans,
    /// IBM Plex Mono: values (positions, tempo, dB, …).
    Mono,
}

impl Family {
    /// Family name as in `tokens.json` (`"IBM Plex Sans"`, `"IBM Plex Mono"`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sans => SANS_FAMILY,
            Self::Mono => MONO_FAMILY,
        }
    }
}

/// One embedded font file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    /// Family.
    pub family: Family,
    /// CSS weight: 400 regular, 500 medium, 600 semibold.
    pub weight: u16,
    /// PostScript-style name, unique per face (`"IBMPlexSans-Medium"`); a good key for egui's
    /// `FontDefinitions::font_data`.
    pub name: &'static str,
    /// The TrueType file.
    pub bytes: &'static [u8],
}

macro_rules! face {
    ($family:ident, $weight:literal, $name:literal) => {
        Face {
            family: Family::$family,
            weight: $weight,
            name: $name,
            bytes: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/fonts/",
                $name,
                ".ttf"
            )),
        }
    };
}

/// All embedded faces.
pub const FACES: [Face; 6] = [
    face!(Sans, 400, "IBMPlexSans-Regular"),
    face!(Sans, 500, "IBMPlexSans-Medium"),
    face!(Sans, 600, "IBMPlexSans-SemiBold"),
    face!(Mono, 400, "IBMPlexMono-Regular"),
    face!(Mono, 500, "IBMPlexMono-Medium"),
    face!(Mono, 600, "IBMPlexMono-SemiBold"),
];

/// Name of [`KEYS_BYTES`] in egui's `FontDefinitions::font_data`.
pub const KEYS_NAME: &str = "KomponistKeys";

/// The keyboard glyphs Plex lacks and macOS shortcut labels need (⌘ U+2318, ⇧ U+21E7, ⌥ U+2325,
/// ⌃ U+2303): a four-glyph subset of DejaVu Sans 2.37, renamed "Komponist Keys" as its licence
/// asks (`assets/fonts/README.md`, `LICENSE-DejaVu.txt`). A fallback only: no text style uses it.
pub const KEYS_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/KomponistKeys.ttf"
));

/// The face of `family` with exactly `weight`, if bundled.
pub fn face(family: Family, weight: u16) -> Option<&'static Face> {
    FACES
        .iter()
        .find(|f| f.family == family && f.weight == weight)
}

/// Licence of the fonts (SIL Open Font License 1.1 with IBM's copyright line), for an
/// about or licence view.
pub const LICENSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/OFL.txt"
));

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes `s` as UTF-16BE, the encoding of Windows names in the TrueType `name` table.
    fn utf16be(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_be_bytes).collect()
    }

    #[test]
    fn faces_are_truetype_of_the_token_family() {
        for face in &FACES {
            assert_eq!(
                &face.bytes[..4],
                &[0, 1, 0, 0],
                "{} is not a TrueType file",
                face.name
            );
            let family = utf16be(face.family.name());
            assert!(
                face.bytes.windows(family.len()).any(|w| w == family),
                "{} does not name the family {:?}",
                face.name,
                face.family.name()
            );
        }
    }

    #[test]
    fn lookup() {
        assert_eq!(
            face(Family::Sans, 600).unwrap().name,
            "IBMPlexSans-SemiBold"
        );
        assert_eq!(face(Family::Mono, 500).unwrap().name, "IBMPlexMono-Medium");
        assert_eq!(
            face(Family::Mono, 600).unwrap().name,
            "IBMPlexMono-SemiBold"
        );
        assert!(face(Family::Mono, 700).is_none());
        for family in [Family::Sans, Family::Mono] {
            for weight in [400, 500, 600] {
                assert!(face(family, weight).is_some(), "{family:?} {weight}");
            }
        }
        assert!(LICENSE.contains("SIL OPEN FONT LICENSE Version 1.1"));
        assert!(LICENSE.contains("Reserved Font Name \"Plex\""));
    }
}
