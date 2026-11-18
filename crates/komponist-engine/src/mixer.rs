//! Mixer building blocks (ARCHITECTURE §6.4 steps 4–5, §6.6): fader with gain ramp and balance,
//! solo logic, latency compensation delay.

use klang_dsp::buffer::gain_ramp;
use klang_dsp::pan::balance;

use crate::automation::MixerValues;

/// Highest linear gain the engine applies (+24 dB); larger and non-finite values are clamped.
const MAX_GAIN: f32 = 16.0;

/// Whether a strip is heard: `!mute && (!any_solo || solo)` ("Stumm durch Solo" when a solo
/// elsewhere silences it).
pub(crate) fn audible(mute: bool, solo: bool, any_solo: bool) -> bool {
    !mute && (!any_solo || solo)
}

/// Per-side target gains of a strip: fader gain × balance, or silence when not audible.
pub(crate) fn targets(gain: f32, pan: f32, audible: bool) -> (f32, f32) {
    if !audible {
        return (0.0, 0.0);
    }
    let gain = if gain.is_nan() {
        0.0
    } else {
        gain.clamp(0.0, MAX_GAIN)
    };
    let (l, r) = balance(if pan.is_nan() { 0.0 } else { pan });
    (gain * l, gain * r)
}

/// Fader state of one strip: the per-side gains reached at the end of the last segment. Every
/// change ramps linearly over the next segment (`klang_dsp::buffer::gain_ramp`), so fader moves,
/// pans, mutes and solos never click.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Fader {
    l: f32,
    r: f32,
}

impl Fader {
    /// A fader standing at `gains`.
    pub(crate) fn at(gains: (f32, f32)) -> Fader {
        Fader {
            l: gains.0,
            r: gains.1,
        }
    }

    /// RT. Applies the ramp from the current gains to `target` to the segment.
    pub(crate) fn apply(&mut self, l: &mut [f32], r: &mut [f32], target: (f32, f32)) {
        gain_ramp(l, self.l, target.0);
        gain_ramp(r, self.r, target.1);
        self.l = target.0;
        self.r = target.1;
    }

    /// RT. Apply the absolute automation ramp to a running strip. Muting and manual
    /// changes still take the established fader path when no active ramp drives it.
    pub(crate) fn apply_automation(
        &mut self,
        l: &mut [f32],
        r: &mut [f32],
        (gain, pan): (f32, f32),
        open: bool,
        mut volume: Option<MixerValues<'_>>,
        mut balance: Option<MixerValues<'_>>,
    ) {
        if !open || (volume.is_none() && balance.is_none()) {
            self.apply(l, r, targets(gain, pan, open));
            return;
        }
        for (l, r) in l.iter_mut().zip(r) {
            let gain = volume.as_mut().map_or(gain, |value| value.value(gain));
            let pan = balance.as_mut().map_or(pan, |value| value.value(pan));
            (self.l, self.r) = targets(gain, pan, true);
            *l *= self.l;
            *r *= self.r;
        }
    }
}

/// Frames a delay line keeps: the longest delay plus one block.
const DELAY_LEN: usize = crate::MAX_PDC_FRAMES + crate::MAX_BLOCK;

/// Latency compensation delay of one track (ARCHITECTURE §6.6), preallocated for
/// [`crate::MAX_PDC_FRAMES`]. Untouched while the delay is 0; cleared when it starts being used
/// again, so no stale audio comes out.
pub(crate) struct DelayLine {
    buf: Box<[f32]>,
    write: usize,
    in_use: bool,
}

impl DelayLine {
    /// Non-RT. Allocates the line (zeroed lazily by the OS until used).
    pub(crate) fn new() -> DelayLine {
        DelayLine {
            buf: vec![0.0; 2 * DELAY_LEN].into_boxed_slice(),
            write: 0,
            in_use: false,
        }
    }

    /// RT. Forgets the line's contents: its next use with a delay starts from silence.
    pub(crate) fn forget(&mut self) {
        self.in_use = false;
    }

    /// RT. Delays the segment in place by `delay` frames (clamped to `MAX_PDC_FRAMES`).
    pub(crate) fn process(&mut self, l: &mut [f32], r: &mut [f32], delay: u32) {
        let delay = (delay as usize).min(crate::MAX_PDC_FRAMES);
        if delay == 0 {
            self.in_use = false;
            return;
        }
        if !self.in_use {
            self.buf.fill(0.0);
            self.write = 0;
            self.in_use = true;
        }
        let (line_l, line_r) = self.buf.split_at_mut(DELAY_LEN);
        let mut w = self.write;
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            line_l[w] = *x;
            line_r[w] = *y;
            let read = (w + DELAY_LEN - delay) % DELAY_LEN;
            *x = line_l[read];
            *y = line_r[read];
            w = (w + 1) % DELAY_LEN;
        }
        self.write = w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_logic() {
        // (mute, solo, any_solo) → audible
        let table = [
            ((false, false, false), true),
            ((true, false, false), false),
            ((false, true, true), true),
            ((false, false, true), false),
            ((true, true, true), false),
        ];
        for ((m, s, any), expected) in table {
            assert_eq!(audible(m, s, any), expected, "{m} {s} {any}");
        }
    }

    #[test]
    fn targets_combine_gain_and_balance() {
        assert_eq!(targets(0.5, 0.0, true), (0.5, 0.5));
        assert_eq!(targets(0.5, 0.5, true), (0.25, 0.5));
        assert_eq!(targets(1.0, -1.0, true), (1.0, 0.0));
        assert_eq!(targets(1.0, 0.0, false), (0.0, 0.0));
        assert_eq!(targets(f32::NAN, f32::NAN, true), (0.0, 0.0));
        assert_eq!(targets(f32::INFINITY, 0.0, true), (MAX_GAIN, MAX_GAIN));
        assert_eq!(targets(-1.0, 0.0, true), (0.0, 0.0));
    }

    #[test]
    fn fader_ramps_once_then_holds() {
        let mut f = Fader::at((1.0, 1.0));
        let (mut l, mut r) = ([1.0_f32; 4], [1.0_f32; 4]);
        f.apply(&mut l, &mut r, (0.0, 0.5));
        assert_eq!(l, [0.75, 0.5, 0.25, 0.0]);
        assert_eq!(r, [0.875, 0.75, 0.625, 0.5]);
        let (mut l, mut r) = ([1.0_f32; 2], [1.0_f32; 2]);
        f.apply(&mut l, &mut r, (0.0, 0.5));
        assert_eq!((l, r), ([0.0; 2], [0.5; 2]));
    }

    #[test]
    fn delay_line_delays_across_segments() {
        let mut d = DelayLine::new();
        let input: Vec<f32> = (1..=40).map(|i| i as f32).collect();
        let mut out = Vec::new();
        for chunk in input.chunks(7) {
            let (mut l, mut r) = (chunk.to_vec(), chunk.iter().map(|x| -x).collect::<Vec<_>>());
            d.process(&mut l, &mut r, 5);
            assert!(l.iter().zip(&r).all(|(a, b)| *a == -*b));
            out.extend(l);
        }
        let mut expected = vec![0.0; 5];
        expected.extend_from_slice(&input[..35]);
        assert_eq!(out, expected);
        // Delay 0 passes through; using the line again starts from silence.
        let (mut l, mut r) = ([7.0_f32; 3], [7.0_f32; 3]);
        d.process(&mut l, &mut r, 0);
        assert_eq!(l, [7.0; 3]);
        d.process(&mut l, &mut r, 2);
        assert_eq!(l, [0.0, 0.0, 7.0]);
        // The maximum delay works.
        let mut d = DelayLine::new();
        let mut first = None;
        for block in 0..10 {
            let (mut l, mut r) = ([1.0_f32; crate::MAX_BLOCK], [1.0_f32; crate::MAX_BLOCK]);
            d.process(&mut l, &mut r, crate::MAX_PDC_FRAMES as u32 + 100);
            if first.is_none()
                && let Some(i) = l.iter().position(|&x| x == 1.0)
            {
                first = Some(block * crate::MAX_BLOCK + i);
            }
        }
        assert_eq!(first, Some(crate::MAX_PDC_FRAMES));
    }
}
