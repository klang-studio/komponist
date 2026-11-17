//! Layout metrics of the mixer (`Screens.dc.html` `mixer()`, `mxScale` and the `isMixer` markup;
//! `ks-view.js` `mixView`). Sums of spacing tokens are written that way; the rest are the design's
//! numbers.
//!
//! A strip, top to bottom: colour bar, name, instrument, "Inserts" with the insert slots, the
//! reserved "Sends", the pan knob with its value, M S R, the fader with the meter next to it,
//! and the level and peak values. The fader takes the height that is left, so the strip fills
//! the maximised dock (S04). In the normal dock there is less height than the design's strip
//! needs; [`Tier`] switches to the lean strip.
//!
//! Deviation from S04 (owner decision 2026-11-14, ARCHITECTURE §13): faders must be tall. The
//! paddings are tighter, the pan value stands right of the knob, the send list and the output button
//! always show on a full strip (WP-103; before the first bus the „+“ offers a new effect bus), and the normal dock's lean strip shows only name, M S R, fader and
//! values (no instrument line, no inserts; the instrument is the name's tooltip and AccessKit
//! description). Pan is a small knob at the right end of the lean strip's name row (owner
//! decision Q8, 2026-11-14), so it takes no fader height.

use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{line, size, space};
use komponist_ui::widgets::fader::FaderSize;
use komponist_ui::widgets::meter::Style as MeterStyle;
use komponist_ui::widgets::metrics::meter as meter_metrics;

/// The design's 10 px texts of a strip (instrument line `instS`, insert slots: `fontSize:10`)
/// have no type token; this is `type.label` (Sans 10/12) without its case and tracking, at
/// regular weight (insert slots of instruments use semibold).
pub const TEXT_10: TypeStyle = TypeStyle {
    weight: 400,
    emphasis_weight: None,
    uppercase: false,
    tracking_em: 0.0,
    color: None,
    ..typography::LABEL
};

/// Width of a track strip (`size.mixer-strip`, `width:80`).
pub const STRIP_WIDTH: f32 = size::MIXER_STRIP;
/// Width of the master strip (`width:isM?96:80`).
pub const MASTER_WIDTH: f32 = 96.0;
/// Gap between the strips, in which `surface.0` shows (`gap:1px; background:#111214`).
pub const STRIP_GAP: f32 = line::WIDTH;
/// Height of the colour bar at the top (`bar:{height:3}`).
pub const COLOR_BAR: f32 = 3.0;
/// Horizontal padding of the strip's rows (`padding:'… 6px'`).
pub const PAD_X: f32 = space::SPACE_1_5;
/// Padding above the name (design `padding:'8px 6px 0'`; tightened to 4 px, the bar already
/// separates the name from the strip's edge).
pub const NAME_TOP: f32 = space::SPACE_1;
/// Padding below the name (`nameS` `paddingBottom:2`).
pub const NAME_BOTTOM: f32 = space::SPACE_0_5;
/// Padding above the instrument line (`instS` `padding:'2px 6px 0'`).
pub const INSTRUMENT_TOP: f32 = space::SPACE_0_5;
/// Padding above "Inserts" (design `padding:8px 6px 4px`; tightened to 4 px).
pub const INSERTS_LABEL_TOP: f32 = space::SPACE_1;
/// Padding below a section label ("Inserts", "Sends": `… 4px`).
pub const LABEL_BOTTOM: f32 = space::SPACE_1;
/// Height of the insert box (`height:88px; overflow:hidden`): three slots and "+".
pub const INSERTS_HEIGHT: f32 = 88.0;
/// Height of an insert slot and of a send placeholder (`height:20`).
pub const SLOT_HEIGHT: f32 = size::CONTROL_COMPACT;
/// Gap between the slots (`gap:2px`).
pub const SLOT_GAP: f32 = space::SPACE_0_5;
/// Horizontal padding of the text in a slot (`padding:'0 6px'`).
pub const SLOT_PAD_X: f32 = space::SPACE_1_5;
/// Thickness of the insert line while a slot or a plugin is dragged over the inserts (S05 notes
/// "blaue 2-px-Linie zeigt die Einfügestelle").
pub const INSERT_LINE: f32 = 2.0;
/// Dashes of the "+" slot's and the sends' border (`1px dashed`): dash and gap.
pub const DASH: [f32; 2] = [3.0, 2.0];
/// Padding above "Sends" (`padding:10px 6px 4px`).
pub const SENDS_LABEL_TOP: f32 = space::SPACE_2 + space::SPACE_0_5;
/// The two reserved send rows (`height:20px` each): the least a strip's send list shows.
pub const SEND_ROWS: usize = 2;

/// Rows of a send list that needs `needed` rows (sends and the "+" row): at least
/// [`SEND_ROWS`], at most [`crate::views::routing::metrics::MAX_STRIP_SEND_ROWS`] (the list
/// scrolls from there on).
pub fn send_rows_for(needed: usize) -> usize {
    needed.clamp(
        SEND_ROWS,
        crate::views::routing::metrics::MAX_STRIP_SEND_ROWS,
    )
}

/// The label band over the buses ("Busse", `04-mixer-dock`, `02-mixer-voll`): one `type.label` line
/// and the gap below it. Only a project with buses has it.
pub const BUS_LABEL_BAND: f32 = typography::LABEL.line_height + space::SPACE_1;
/// The free space between the scrolling tracks and the buses (`02-mixer-voll`: the buses do not
/// move when tracks are added).
pub const BUS_GAP: f32 = space::SPACE_2;

/// Height of the output button's row above the values row: the button and the gap above it.
pub const OUTPUT_ROW: f32 = SLOT_HEIGHT + space::SPACE_1;
/// Padding above the pan knob (design `padding:12px 0 6px`; tightened to 4 px).
pub const PAN_TOP: f32 = space::SPACE_1;
/// Gap between the knob and its value, which now stands right of the knob (design: below it,
/// `gap:2px`).
pub const PAN_GAP: f32 = space::SPACE_1;
/// Padding below the pan row.
pub const PAN_BOTTOM: f32 = space::SPACE_1;
/// The M S R toggles of a strip (`tgl(k, …, 22, 18)`).
pub const TOGGLE: [f32; 2] = [22.0, 18.0];
/// Gap between the toggles (`gap:2px`).
pub const TOGGLE_GAP: f32 = space::SPACE_0_5;
/// Padding below the toggles (design `padding-bottom:8px`; tightened to 4 px).
pub const TOGGLES_BOTTOM: f32 = space::SPACE_1;
/// Gap between the fader column and the meter (`gap:6px`).
pub const FADER_METER_GAP: f32 = space::SPACE_1_5;
/// Padding above the values row (design `padding:8px 6px 10px`; tightened to 4 px).
pub const VALUES_TOP: f32 = space::SPACE_1;
/// Padding below the values row (tightened to 6 px).
pub const VALUES_BOTTOM: f32 = space::SPACE_1_5;
/// The least height of the fader area before the strip drops secondary parts: the thumb (44)
/// and a track of 156 px, about 2 px per 0.5 dB between 0 and −6 dB. Not in the design, which
/// only shows the maximised mixer (was 96 until 2026-11-14).
pub const FADER_AREA_MIN: f32 = 200.0;

/// The fewest insert rows a strip shows when it shows any (instrument and one more).
pub const MIN_INSERT_ROWS: usize = 2;

/// The fader of a strip: normal thumb (32 × 44) with the scale on the left.
pub const FADER: FaderSize = FaderSize::Normal;
/// The meter of a strip: the mixer meter (2 × 4, LED).
pub const METER: MeterStyle = MeterStyle::MIXER;

/// What a strip shows at a given height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    /// The "Inserts" label.
    pub inserts_label: bool,
    /// Rows of the insert box (slots and "+").
    pub insert_rows: usize,
    /// The send list ("Sends" or, on a bus, "Eingänge"): shown on the full strip, also before the first bus.
    pub sends: bool,
    /// Rows of the send list (with the "+" row); 0 without it.
    pub send_rows: usize,
    /// The output button above the values row (WP-103).
    pub output: bool,
    /// Name (with a small pan knob), M S R, fader and values only. No instrument line.
    pub lean: bool,
}

impl Tier {
    /// The design's strip: everything, insert box 88 high.
    pub const FULL: Tier = Tier {
        inserts_label: true,
        insert_rows: 4,
        // The send area comes with `Tier::routed`.
        sends: false,
        send_rows: 0,
        output: false,
        lean: false,
    };
    /// The normal dock's strip: the secondary parts give way to the fader.
    pub const LEAN: Tier = Tier {
        inserts_label: false,
        insert_rows: 0,
        sends: false,
        send_rows: 0,
        output: false,
        lean: true,
    };

    /// What fits into a strip `height` high: the full strip if its fader area keeps
    /// [`FADER_AREA_MIN`]; else the lean strip, in which the fader takes what the inserts, the
    /// instrument line and the pan knob leave.
    pub fn for_height(height: f32) -> Tier {
        Tier::for_routed_height(height, 0)
    }

    /// [`Tier::for_height`] with the send area: `send_rows` (> 0) rows of sends and the
    /// output button come with the full strip, and the strip must still keep its fader area.
    pub fn for_routed_height(height: f32, send_rows: usize) -> Tier {
        let full = Tier::FULL.routed(send_rows);
        if height - fixed_height(full) >= FADER_AREA_MIN {
            full
        } else {
            Tier::LEAN
        }
    }

    /// The full strip with the send list of `send_rows` rows and the output button.
    pub fn routed(self, send_rows: usize) -> Tier {
        Tier {
            sends: send_rows > 0 && !self.lean,
            send_rows,
            output: send_rows > 0 && !self.lean,
            ..self
        }
    }

    /// Height of the insert box.
    pub fn inserts_height(self) -> f32 {
        if self.insert_rows >= Tier::FULL.insert_rows {
            INSERTS_HEIGHT
        } else {
            rows_height(self.insert_rows)
        }
    }
}

/// Height of `n` slot rows with their gaps.
pub fn rows_height(n: usize) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * SLOT_HEIGHT + (n - 1) as f32 * SLOT_GAP
    }
}

/// Height of a strip without its fader area, given the line heights of the text styles: name
/// `type.ui` (16), instrument and labels `type.small` / `type.label`, pan value and values.
pub fn fixed_height(tier: Tier) -> f32 {
    use komponist_tokens::typography::{LABEL, UI, VALUE_M};
    let mut h = COLOR_BAR + NAME_TOP + UI.line_height + NAME_BOTTOM;
    if tier.lean {
        return h
            + INSERTS_LABEL_TOP
            + TOGGLE[1]
            + TOGGLES_BOTTOM
            + VALUES_TOP
            + VALUE_M.line_height
            + VALUES_BOTTOM;
    }
    h += INSTRUMENT_TOP + TEXT_10.line_height;
    if tier.inserts_label {
        h += INSERTS_LABEL_TOP + LABEL.line_height + LABEL_BOTTOM;
    } else {
        h += INSERTS_LABEL_TOP;
    }
    h += tier.inserts_height();
    if tier.sends {
        h += SENDS_LABEL_TOP + LABEL.line_height + LABEL_BOTTOM + rows_height(tier.send_rows);
    }
    if tier.output {
        h += OUTPUT_ROW;
    }
    h += PAN_TOP + size::KNOB + PAN_BOTTOM;
    h += TOGGLE[1] + TOGGLES_BOTTOM;
    h += VALUES_TOP + VALUE_M.line_height + VALUES_BOTTOM;
    h
}

/// Width of the fader column (with or without the scale) and the meter next to it.
pub fn fader_group_width(scale: bool) -> f32 {
    komponist_ui::widgets::fader::Layout::desired_size(FADER, scale, 0.0).x
        + FADER_METER_GAP
        + METER.thickness()
}

/// Whether the fader's scale labels fit a track `length` long without overlapping: the closest
/// taper points (`DBT`) are a label's line height apart at least. A short fader in the normal
/// dock goes without its scale.
pub fn scale_fits(length: f32) -> bool {
    use komponist_ui::widgets::fader::DBT;
    let step = DBT
        .windows(2)
        .map(|w| w[1].1 - w[0].1)
        .fold(f32::MAX, f32::min);
    length * step >= komponist_ui::widgets::fader::SCALE_TEXT.line_height
}

/// Where the meter's LED sits above the fader's track: the bars span the track, so the meter
/// reads on the fader's scale.
pub const METER_LED: f32 = meter_metrics::LED_HEIGHT + meter_metrics::LED_GAP;

/// Four-button audio strip controls (audio proposal D8A).
pub const AUDIO_TOGGLE: [f32; 2] = [18.0, 16.0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_maximised_dock_shows_everything() {
        // S04: 1080 − transport 44 − tab 28 − status bar 22 − frame lines.
        assert_eq!(Tier::for_height(980.0), Tier::FULL);
        assert_eq!(Tier::FULL.inserts_height(), INSERTS_HEIGHT);
        // The fader area takes the rest.
        assert!(980.0 - fixed_height(Tier::FULL) > 500.0);
    }

    #[test]
    fn the_normal_dock_gives_way_to_the_fader() {
        // No reserved sends any more; the normal dock (320 − the 28 px tab strip)
        // gets the lean strip, which keeps FADER_AREA_MIN and more.
        let t = Tier::for_height(292.0);
        assert_eq!(t, Tier::LEAN);
        assert!(292.0 - fixed_height(t) >= FADER_AREA_MIN);
        let full = fixed_height(Tier::FULL) + FADER_AREA_MIN;
        assert_eq!(Tier::for_height(full), Tier::FULL);
        assert_eq!(Tier::for_height(full - 1.0), Tier::LEAN);
    }

    #[test]
    fn the_fader_group_fits_the_strip() {
        // Fader 54 (scale 20, gap 2, thumb 32), gap 6, meter 9.
        assert_eq!(fader_group_width(true), 69.0);
        assert!(fader_group_width(true) <= STRIP_WIDTH);
        assert_eq!(fader_group_width(false), 47.0);
        // The taper's closest points are 0.14 apart, the labels 12 high.
        assert!(scale_fits(200.0) && scale_fits(86.0));
        assert!(!scale_fits(73.0));
        assert_eq!(rows_height(4), 86.0);
    }
}
