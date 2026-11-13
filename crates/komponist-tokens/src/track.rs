//! Track colours (`track.*`): hue list, auto-assign order, oklch formulas and [`TrackPalette`].
//!
//! All 14 hues share lightness and chroma, so every track colour is computed from its hue with the
//! formulas of `track.$formula` ([`formula`]). Clip head = `base`, clip body = `body`, content =
//! `ink`, muted = `dim`; note lightness follows velocity.

use crate::{Srgba, oklch_to_srgb};

include!(concat!(env!("OUT_DIR"), "/track.rs"));

/// One track hue of the design system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hue {
    /// Key in `track.colors` (`"red"`, `"vermilion"`, …).
    pub name: &'static str,
    /// oklch hue angle in degrees.
    pub degrees: u16,
}

/// Lightness and chroma of an oklch colour whose hue is the track hue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightnessChroma {
    /// Oklab lightness, 0–1.
    pub l: f64,
    /// Chroma.
    pub c: f64,
}

/// Note colour as a function of velocity: `oklch(l_min + l_range · vel / 127, c, h)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteFormula {
    /// Lightness at velocity 0.
    pub l_min: f64,
    /// Lightness added at velocity 127.
    pub l_range: f64,
    /// Chroma.
    pub c: f64,
}

impl NoteFormula {
    /// Lightness of a note with `velocity` (0–127).
    pub fn lightness(&self, velocity: u8) -> f64 {
        self.l_min + self.l_range * f64::from(velocity) / 127.0
    }
}

/// The colours `tokens.json` lists precomputed for one hue (see [`PRECOMPUTED`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrecomputedColors {
    /// Hue in degrees.
    pub hue: u16,
    /// Clip head, track colour bar.
    pub base: Srgba,
    /// Clip body.
    pub body: Srgba,
    /// Muted clip.
    pub dim: Srgba,
    /// Clip content.
    pub ink: Srgba,
    /// Secondary clip content.
    pub ink_dim: Srgba,
    /// Playing note (`track.$formula.note-playing`).
    pub play: Srgba,
    /// Note colours for some velocities, `(velocity, colour)`, ascending.
    pub note_velocity: &'static [(u8, Srgba)],
}

/// All colours of one track hue, computed with [`oklch_to_srgb`] from [`formula`].
///
/// Build one per hue at startup (`HUES.iter().map(|h| TrackPalette::new(h.degrees))`); `new`
/// computes 2 × 128 note colours.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackPalette {
    /// Hue in degrees.
    pub hue: u16,
    /// Clip head and track colour bar (`base`).
    pub base: Srgba,
    /// Clip body (`body`).
    pub body: Srgba,
    /// Muted clip (`dim`).
    pub dim: Srgba,
    /// Clip content: notes and steps drawn inside a clip (`ink`).
    pub ink: Srgba,
    /// Secondary clip content (`ink-dim`).
    pub ink_dim: Srgba,
    /// Note that is playing right now (`note-playing`, precomputed as `play`).
    pub play: Srgba,
    /// Clip head under the mouse (`clip-head-hover`).
    pub clip_head_hover: Srgba,
    notes: [Srgba; 128],
    notes_hover: [Srgba; 128],
}

impl TrackPalette {
    /// Computes the palette of `hue` (degrees).
    pub fn new(hue: u16) -> Self {
        let h = f64::from(hue);
        let lc = |f: LightnessChroma| oklch_to_srgb(f.l, f.c, h);
        let note = formula::NOTE;
        let notes = std::array::from_fn(|v| oklch_to_srgb(note.lightness(v as u8), note.c, h));
        let notes_hover = std::array::from_fn(|v| {
            oklch_to_srgb(
                note.lightness(v as u8) + formula::NOTE_HOVER_L_DELTA,
                note.c,
                h,
            )
        });
        Self {
            hue,
            base: lc(formula::BASE),
            body: lc(formula::BODY),
            dim: lc(formula::DIM),
            ink: lc(formula::INK),
            ink_dim: lc(formula::INK_DIM),
            play: lc(formula::NOTE_PLAYING),
            clip_head_hover: lc(formula::CLIP_HEAD_HOVER),
            notes,
            notes_hover,
        }
    }

    /// Note colour for `velocity`; velocities above 127 count as 127.
    pub fn note(&self, velocity: u8) -> Srgba {
        self.notes[usize::from(velocity.min(127))]
    }

    /// Note colour under the mouse (`note-hover`: note lightness + 0.08).
    pub fn note_hover(&self, velocity: u8) -> Srgba {
        self.notes_hover[usize::from(velocity.min(127))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest per-channel difference.
    fn diff(a: Srgba, b: Srgba) -> u8 {
        (0..4).map(|i| a[i].abs_diff(b[i])).max().unwrap()
    }

    #[test]
    fn palette_matches_every_precomputed_colour() {
        let mut checked = 0;
        for pre in &PRECOMPUTED {
            let p = TrackPalette::new(pre.hue);
            let pairs = [
                ("base", p.base, pre.base),
                ("body", p.body, pre.body),
                ("dim", p.dim, pre.dim),
                ("ink", p.ink, pre.ink),
                ("ink-dim", p.ink_dim, pre.ink_dim),
                ("play", p.play, pre.play),
            ];
            for (what, got, want) in pairs {
                assert!(
                    diff(got, want) <= 1,
                    "hue {} {what}: {got:?} vs {want:?}",
                    pre.hue
                );
                checked += 1;
            }
            assert_eq!(pre.note_velocity.len(), 5, "hue {}", pre.hue);
            for &(vel, want) in pre.note_velocity {
                let got = p.note(vel);
                assert!(
                    diff(got, want) <= 1,
                    "hue {} note {vel}: {got:?} vs {want:?}",
                    pre.hue
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 14 * 11);
    }

    #[test]
    fn hues_and_auto_assign_order() {
        assert_eq!(HUES.len(), 14);
        assert_eq!(
            HUES[0],
            Hue {
                name: "red",
                degrees: 25
            }
        );
        assert_eq!(
            HUES[13],
            Hue {
                name: "pink",
                degrees: 350
            }
        );
        assert!(HUES.windows(2).all(|w| w[0].degrees < w[1].degrees));
        for (hue, pre) in HUES.iter().zip(&PRECOMPUTED) {
            assert_eq!(hue.degrees, pre.hue);
        }
        assert_eq!(
            AUTO_ASSIGN_HUE_ORDER,
            [150, 250, 300, 95, 200, 25, 325, 175, 70, 225, 125, 275, 350]
        );
        // ARCHITECTURE §13.4: vermilion (45) is manual-only.
        assert!(!AUTO_ASSIGN_HUE_ORDER.contains(&45));
    }

    #[test]
    fn formulas_from_tokens() {
        assert_eq!(formula::BASE, LightnessChroma { l: 0.70, c: 0.12 });
        assert_eq!(formula::INK, LightnessChroma { l: 0.88, c: 0.084 });
        assert_eq!(
            formula::NOTE,
            NoteFormula {
                l_min: 0.40,
                l_range: 0.44,
                c: 0.12
            }
        );
        assert_eq!(formula::NOTE_HOVER_L_DELTA, 0.08);
        assert_eq!(formula::NOTE_PLAYING, LightnessChroma { l: 0.92, c: 0.06 });
        assert_eq!(
            formula::CLIP_HEAD_HOVER,
            LightnessChroma { l: 0.77, c: 0.12 }
        );
    }

    #[test]
    fn note_lightness_grows_with_velocity() {
        let p = TrackPalette::new(150);
        let luma = |c: Srgba| u32::from(c[0]) * 2 + u32::from(c[1]) * 7 + u32::from(c[2]);
        for v in 1..=127u8 {
            assert!(luma(p.note(v)) >= luma(p.note(v - 1)), "velocity {v}");
            assert!(luma(p.note_hover(v)) > luma(p.note(v)), "velocity {v}");
        }
        assert_eq!(p.note(200), p.note(127));
        assert_eq!(p.note_hover(255), p.note_hover(127));
    }

    #[test]
    fn note_hover_is_note_lightness_plus_delta() {
        let p = TrackPalette::new(250);
        let want = oklch_to_srgb(0.40 + 0.44 * 64.0 / 127.0 + 0.08, 0.12, 250.0);
        assert_eq!(p.note_hover(64), want);
    }
}
