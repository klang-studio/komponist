//! Text styles (`type.*` in `tokens.json`): `type.value.l` → [`VALUE_L`].

use crate::Srgba;
use crate::font::{self, Face, Family};

include!(concat!(env!("OUT_DIR"), "/typography.rs"));

/// A text style of the design system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeStyle {
    /// Font family.
    pub family: Family,
    /// Font size in px.
    pub size: f32,
    /// Line height in px.
    pub line_height: f32,
    /// CSS weight (400, 500, 600).
    pub weight: u16,
    /// Second weight the style allows, for emphasis: `type.ui` is "400/500", buttons use 500.
    pub emphasis_weight: Option<u16>,
    /// Set in capitals (`case: upper`).
    pub uppercase: bool,
    /// Extra letter spacing in em (`tracking: 0.08em` → 0.08); multiply by `size` for px.
    pub tracking_em: f32,
    /// Default text colour of the style, if it has one (`type.small` → `text.2`).
    pub color: Option<Srgba>,
}

impl TypeStyle {
    /// The bundled face for [`weight`](Self::weight).
    pub fn face(&self) -> &'static Face {
        font::face(self.family, self.weight).expect("every type style has a bundled face (tested)")
    }

    /// The bundled face for [`emphasis_weight`](Self::emphasis_weight), if the style has one.
    pub fn emphasis_face(&self) -> Option<&'static Face> {
        self.emphasis_weight.map(|w| {
            font::face(self.family, w).expect("every type style has a bundled face (tested)")
        })
    }

    /// Letter spacing in px.
    pub fn tracking_px(&self) -> f32 {
        self.tracking_em * self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color;

    #[test]
    fn styles_from_tokens() {
        assert_eq!(TITLE.family, Family::Sans);
        assert_eq!(
            (TITLE.size, TITLE.line_height, TITLE.weight),
            (16.0, 20.0, 600)
        );
        assert_eq!((UI.weight, UI.emphasis_weight), (400, Some(500)));
        assert_eq!(SMALL.color, Some(color::TEXT_2));
        assert_eq!((LABEL.uppercase, TITLE.uppercase), (true, false));
        assert_eq!(LABEL.tracking_em, 0.08);
        assert_eq!(LABEL.color, Some(color::TEXT_3));
        assert!((LABEL.tracking_px() - 0.8).abs() < 1e-6);
        assert_eq!(VALUE_L.family, Family::Mono);
        assert_eq!(
            (VALUE_L.size, VALUE_L.line_height, VALUE_L.weight),
            (20.0, 24.0, 500)
        );
        assert_eq!(VALUE_S.color, Some(color::TEXT_3));
        assert_eq!((VALUE_M.tracking_em, VALUE_M.color), (0.0, None));
    }

    /// `type.body` is a komponist addition (`design/README.md`): the 13 px regular body text of
    /// S06 and S10 (`font-size:13px; line-height:1.55` → 20.15, rounded to whole px).
    #[test]
    fn body_is_the_13_px_regular_text() {
        assert_eq!(BODY.family, Family::Sans);
        assert_eq!(
            (BODY.size, BODY.line_height, BODY.weight),
            (13.0, 20.0, 400)
        );
        assert_eq!((BODY.size * 1.55).round(), BODY.line_height);
        assert_eq!(BODY.size, TRACK.size, "the size of type.track, regular");
        assert_eq!((BODY.emphasis_weight, BODY.color), (None, None));
        assert!(ALL.iter().any(|(name, _)| *name == "body"));
    }

    /// The four device styles are komponist additions (`design/README.md`, owner decision
    /// 2026-11-14: 8 and 9 px text accepted): the device handoff's `lbl`, graph title and the two
    /// small mono sizes of axis labels, handle numbers and units.
    #[test]
    fn device_styles_are_the_handoffs_small_texts() {
        assert_eq!(
            (DEVICE_LABEL.family, DEVICE_LABEL.weight),
            (Family::Sans, 400)
        );
        assert_eq!((DEVICE_LABEL.size, DEVICE_LABEL.line_height), (10.0, 12.0));
        assert_eq!(DEVICE_LABEL.color, Some(color::TEXT_2));
        assert_eq!((GRAPH_TITLE.size, GRAPH_TITLE.weight), (9.0, 600));
        assert!(std::hint::black_box(GRAPH_TITLE).uppercase);
        assert_eq!(GRAPH_TITLE.tracking_em, 0.08);
        assert_eq!(GRAPH_TITLE.color, Some(color::TEXT_2));
        assert_eq!((MONO_XS.family, MONO_XS.size), (Family::Mono, 8.0));
        assert_eq!((MONO_XXS.family, MONO_XXS.size), (Family::Mono, 9.0));
        for style in [MONO_XS, MONO_XXS] {
            assert_eq!((style.weight, style.color), (400, Some(color::TEXT_2)));
        }
        for name in ["device-label", "graph-title", "mono-xs", "mono-xxs"] {
            assert!(ALL.iter().any(|(n, _)| *n == name), "{name}");
        }
    }

    #[test]
    fn every_style_has_a_bundled_face() {
        for (name, style) in ALL {
            assert_eq!(style.face().family, style.family, "{name}");
            assert_eq!(style.face().weight, style.weight, "{name}");
            if let Some(w) = style.emphasis_weight {
                assert_eq!(style.emphasis_face().unwrap().weight, w, "{name}");
            }
        }
    }
}
