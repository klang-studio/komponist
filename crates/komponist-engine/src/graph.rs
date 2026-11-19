//! The routing graph at run time (WP-102, ARCHITECTURE §6.8.4–6.8.5): the engine's table of edge
//! states (delay line and gain of every send, sidechain and head delay) and the summing of sends.
//!
//! Edge states are engine-owned and preallocated ([`crate::route::MAX_LINES`] of them); programs
//! refer to them by [`LineId`](crate::route::LineId). The compiler keeps an edge's line from one
//! program to the next, so a recompile keeps the delayed audio and the gain; when a line changes
//! hands (another [`LineKey`]), installing the program resets it.

use klang_dsp::buffer::add_ramped;

use crate::mixer::{DelayLine, opening_gain};
use crate::route::LineKey;

/// State of one edge: its delay line and, for a send, the gain reached at the end of the last
/// segment (every change ramps over the next segment, so send levels, mutes and solos never
/// click).
pub(crate) struct EdgeState {
    key: Option<LineKey>,
    pub(crate) line: DelayLine,
    gain: f32,
    open: bool,
}

impl EdgeState {
    /// Non-RT.
    pub(crate) fn new() -> EdgeState {
        EdgeState {
            key: None,
            line: DelayLine::new(),
            gain: 0.0,
            open: false,
        }
    }

    /// RT. The line serves `key` from now on; a line that served another key forgets its audio
    /// and its gain (a new send fades in from silence).
    pub(crate) fn assign(&mut self, key: LineKey) {
        if self.key != Some(key) {
            self.key = Some(key);
            self.line.forget();
            self.gain = 0.0;
            self.open = false;
        }
    }

    /// RT. Adds the tapped signal `tap`, delayed by `delay` frames and scaled by a ramp from the
    /// last gain to `gain`, to `sum`. `scratch` holds the delayed copy.
    pub(crate) fn add_send(
        &mut self,
        sum: [&mut [f32]; 2],
        tap: [&[f32]; 2],
        scratch: [&mut [f32]; 2],
        delay: u32,
        (gain, open): (f32, bool),
    ) {
        self.open = open;
        let [sum_l, sum_r] = sum;
        let n = sum_l.len();
        let source: [&[f32]; 2] = if delay > 0 {
            let [l, r] = scratch;
            l[..n].copy_from_slice(&tap[0][..n]);
            r[..n].copy_from_slice(&tap[1][..n]);
            self.line.process(&mut l[..n], &mut r[..n], delay);
            [&l[..n], &r[..n]]
        } else {
            self.line.forget();
            [&tap[0][..n], &tap[1][..n]]
        };
        add_ramped(sum_l, source[0], self.gain, gain);
        add_ramped(sum_r, source[1], self.gain, gain);
        self.gain = gain;
    }

    /// An automated send, after its alignment delay. The gain curve is timed to the
    /// source heard at this tap.
    pub(crate) fn add_automated_send(
        &mut self,
        sum: [&mut [f32]; 2],
        tap: [&[f32]; 2],
        scratch: [&mut [f32]; 2],
        delay: u32,
        fallback: f32,
        gain: &mut crate::automation::MixerValues<'_>,
    ) {
        let n = sum[0].len();
        let source = if delay > 0 {
            let [l, r] = scratch;
            self.delayed([&mut l[..n], &mut r[..n]], tap, delay);
            [&l[..n], &r[..n]]
        } else {
            self.line.forget();
            tap
        };
        let [out_l, out_r] = sum;
        let was_open = std::mem::replace(&mut self.open, true);
        for i in 0..n {
            self.gain = gain.value(fallback);
            let level = self.gain * opening_gain(was_open, i, n as f32);
            out_l[i] += source[0][i] * level;
            out_r[i] += source[1][i] * level;
        }
    }

    /// RT. Copies `tap` into `out`, delayed by `delay` frames (a sidechain input).
    pub(crate) fn delayed(&mut self, out: [&mut [f32]; 2], tap: [&[f32]; 2], delay: u32) {
        let [l, r] = out;
        l.copy_from_slice(&tap[0][..l.len()]);
        r.copy_from_slice(&tap[1][..r.len()]);
        self.line.process(l, r, delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{SendId, TrackId};

    #[test]
    fn sends_ramp_their_gain_and_delay() {
        let mut edge = EdgeState::new();
        edge.assign(LineKey::Send(SendId(1)));
        let tap = [1.0_f32; 4];
        let (mut sl, mut sr) = ([0.0_f32; 4], [0.0_f32; 4]);
        let (mut cl, mut cr) = ([0.0_f32; 4], [0.0_f32; 4]);
        // Fades in from 0 to 0.5 over the segment.
        edge.add_send(
            [&mut sl, &mut sr],
            [&tap, &tap],
            [&mut cl, &mut cr],
            0,
            (0.5, true),
        );
        assert_eq!(sl, [0.125, 0.25, 0.375, 0.5]);
        // Holds, delayed by 2 frames (the line starts from silence).
        let (mut sl, mut sr) = ([0.0_f32; 4], [0.0_f32; 4]);
        edge.add_send(
            [&mut sl, &mut sr],
            [&tap, &tap],
            [&mut cl, &mut cr],
            2,
            (0.5, true),
        );
        assert_eq!(sl, [0.0, 0.0, 0.5, 0.5]);
        assert_eq!(sr, sl);
        // The same key keeps the state; another key resets it.
        edge.assign(LineKey::Send(SendId(1)));
        assert_eq!(edge.gain, 0.5);
        edge.assign(LineKey::Head(TrackId(1)));
        assert_eq!(edge.gain, 0.0);
    }
}
