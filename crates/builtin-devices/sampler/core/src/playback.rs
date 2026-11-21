//! Playback-region sampling for pitched notes, loops and slices.

use crate::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackDirection {
    #[default]
    Forward,
    Reverse,
    /// Reflects at both loop points. Plays forward without a loop.
    PingPong,
}

/// Loop bounds in playback sample frames, with an exclusive end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackLoop {
    pub start: f64,
    pub end: f64,
    /// Total width in frames of the overlap centered on a forward or reverse wrap.
    pub crossfade: f64,
}

/// A playback region in the frames of the sample supplied to the engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackRegion {
    pub start: f64,
    /// Exclusive end, clamped to the sample length.
    pub end: f64,
    /// Trim fade lengths measured in frames, relative to the playback direction.
    pub fade_in: f64,
    pub fade_out: f64,
    pub looping: Option<PlaybackLoop>,
    pub direction: PlaybackDirection,
}

impl Default for PlaybackRegion {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: f64::INFINITY,
            fade_in: 0.0,
            fade_out: 0.0,
            looping: None,
            direction: PlaybackDirection::Forward,
        }
    }
}

/// Plays one sample slot with its own tuning and region, independently of pad layers.
#[derive(Debug, Clone, Copy)]
pub struct SampleTrigger {
    pub pad: usize,
    pub slot: usize,
    pub key: u8,
    /// Exact note identity when supplied by the sequencer.
    pub note_id: Option<u32>,
    /// Channel associated with this note, if the source supplied one.
    pub channel: Option<u8>,
    pub velocity: f32,
    /// Tuning offset in semitones, in addition to the pad's tune.
    pub tune: f32,
    /// Changes pitch without changing the speed of the playback timeline.
    pub independent_pitch: bool,
    pub region: PlaybackRegion,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Playback {
    region: PlaybackRegion,
    first: isize,
    last: isize,
    backward: bool,
    wrapped: bool,
    pub gate: bool,
    pub key: u8,
    pub released: bool,
    pub independent_pitch: bool,
}

impl Playback {
    pub fn new(trigger: SampleTrigger, len: usize, gate: bool) -> Option<Self> {
        let mut region = trigger.region;
        if region.start.is_nan() || region.end.is_nan() {
            return None;
        }
        region.start = region.start.clamp(0.0, len as f64);
        region.end = region.end.clamp(0.0, len as f64);
        let duration = region.end - region.start;
        if duration < 1.0 {
            return None;
        }
        region.fade_in = finite_length(region.fade_in, duration);
        region.fade_out = finite_length(region.fade_out, duration);
        region.looping = region.looping.and_then(|mut window| {
            if !window.start.is_finite() || !window.end.is_finite() {
                return None;
            }
            window.start = window.start.clamp(region.start, region.end);
            window.end = window.end.clamp(region.start, region.end);
            let length = window.end - window.start;
            let minimum = if region.direction == PlaybackDirection::PingPong {
                2.0
            } else {
                1.0
            };
            if length < minimum {
                return None;
            }
            window.crossfade = finite_length(window.crossfade, length);
            Some(window)
        });
        Some(Self {
            region,
            first: region.start.ceil() as isize,
            last: region.end.ceil() as isize - 1,
            backward: region.direction == PlaybackDirection::Reverse,
            wrapped: false,
            gate,
            key: trigger.key,
            released: false,
            independent_pitch: trigger.independent_pitch,
        })
    }

    pub fn start(&self, offset: f32) -> f64 {
        let region = &self.region;
        let shift = f64::from(offset.max(0.0)) * (region.end - region.start);
        if self.backward {
            let first = region.looping.map_or(region.start, |window| window.start);
            (region.end - 1.0 - shift).max(first)
        } else {
            let end = region.looping.map_or(region.end, |window| window.end);
            (region.start + shift).min(end - 1.0)
        }
    }

    /// Output frames remaining at the tempo clock, for a finite region.
    pub fn remaining_frames(&self, pos: f64, clock_rate: f64) -> Option<u32> {
        self.region.looping.is_none().then(|| {
            if self.backward {
                (((pos - self.region.start) / clock_rate).floor() as u32).saturating_add(1)
            } else {
                ((self.region.end - pos) / clock_rate).ceil() as u32
            }
        })
    }

    pub fn offset_in_region(&self, pos: f64, distance: f64) -> f64 {
        let offset = if self.backward { -distance } else { distance };
        (pos + offset).clamp(
            self.region.start,
            (self.region.end - 1.0).max(self.region.start),
        )
    }

    pub fn contains(&self, pos: f64) -> bool {
        pos >= self.region.start && pos < self.region.end
    }

    pub fn read(&self, sample: &Sample, pos: f64, fades: bool) -> (f32, f32) {
        let region = &self.region;
        let (left, right) = self.read_head(sample, pos, fades);
        let Some(window) = region.looping else {
            return (left, right);
        };
        if region.direction == PlaybackDirection::PingPong || window.crossfade == 0.0 {
            return (left, right);
        }

        // Read through the wrap with two heads. Their positions differ by exactly one
        // loop period, so blending never shortens the loop. Sample taps stay within trim.
        let half = window.crossfade * 0.5;
        let period = window.end - window.start;
        let (other_pos, weight) =
            if (!self.backward || self.wrapped) && pos >= window.end - half && pos < window.end {
                (pos - period, (pos - (window.end - half)) / window.crossfade)
            } else if (self.backward || self.wrapped)
                && pos >= window.start
                && pos < window.start + half
            {
                (
                    pos + period,
                    ((window.start + half) - pos) / window.crossfade,
                )
            } else {
                return (left, right);
            };
        let (other_l, other_r) = self.read_head(sample, other_pos, fades);
        let weight = weight as f32;
        (
            left + (other_l - left) * weight,
            right + (other_r - right) * weight,
        )
    }

    pub fn fade_gain(&self, pos: f64) -> f32 {
        let region = &self.region;
        let (from_start, from_end) = if region.direction == PlaybackDirection::Reverse {
            (region.end - 1.0 - pos, pos - region.start)
        } else {
            (pos - region.start, region.end - 1.0 - pos)
        };
        fade(from_start, region.fade_in) * fade(from_end, region.fade_out)
    }

    fn read_head(&self, sample: &Sample, pos: f64, fades: bool) -> (f32, f32) {
        let bounded = pos.clamp(self.first as f64, self.last as f64);
        let (l, r) = sample.read_between(bounded, self.first, self.last);
        if fades {
            let gain = self.fade_gain(pos);
            (l * gain, r * gain)
        } else {
            (l, r)
        }
    }

    pub fn advance(&mut self, pos: f64, rate: f64) -> f64 {
        let next = if self.backward {
            pos - rate
        } else {
            pos + rate
        };
        let Some(window) = self.region.looping else {
            return next;
        };
        if self.region.direction == PlaybackDirection::PingPong {
            // Reflect at the final sample frame so a unit increment never repeats an edge.
            self.reflect(next, window.start, window.end - 1.0)
        } else if (!self.backward && next >= window.end) || (self.backward && next < window.start) {
            self.wrapped = true;
            window.start + (next - window.start).rem_euclid(window.end - window.start)
        } else {
            next
        }
    }

    pub fn advance_grain(&mut self, pos: f64, rate: f64) -> f64 {
        if self.region.looping.is_some() {
            return self.advance(pos, rate);
        }
        // A grain can finish after its read head reaches trim. Reflecting it gives
        // the overlap a continuous signal without reading neighboring slices.
        let next = if self.backward {
            pos - rate
        } else {
            pos + rate
        };
        self.reflect(next, self.region.start, self.region.end - 1.0)
    }

    fn reflect(&mut self, next: f64, first: f64, last: f64) -> f64 {
        let span = last - first;
        if span <= 0.0 {
            return first;
        }
        if (!self.backward && next < last) || (self.backward && next > first) {
            return next;
        }
        let phase = if self.backward {
            span + (last - next)
        } else {
            next - first
        }
        .rem_euclid(span * 2.0);
        self.backward = phase >= span;
        if self.backward {
            last - (phase - span)
        } else {
            first + phase
        }
    }
}

fn finite_length(value: f64, limit: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, limit)
    } else {
        0.0
    }
}

fn fade(distance: f64, length: f64) -> f32 {
    if length > 0.0 {
        (distance / length).clamp(0.0, 1.0) as f32
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playback(region: PlaybackRegion, len: usize) -> Playback {
        Playback::new(
            SampleTrigger {
                pad: 0,
                slot: 0,
                key: 60,
                note_id: None,
                channel: None,
                velocity: 1.0,
                tune: 0.0,
                independent_pitch: false,
                region,
            },
            len,
            true,
        )
        .unwrap()
    }

    #[test]
    fn slice_interpolation_respects_trim_edges() {
        let slice = playback(
            PlaybackRegion {
                start: 2.0,
                end: 5.0,
                ..PlaybackRegion::default()
            },
            6,
        );
        let sample = Sample::mono(48_000.0, vec![0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        for pos in [2.25, 3.75, 4.75] {
            assert_eq!(slice.read(&sample, pos, true), (0.0, 0.0));
        }
        assert!(!slice.contains(5.0));
    }

    #[test]
    fn trim_fades_follow_the_playback_direction() {
        let backward = playback(
            PlaybackRegion {
                start: 1.0,
                end: 9.0,
                fade_in: 2.0,
                fade_out: 4.0,
                direction: PlaybackDirection::Reverse,
                ..PlaybackRegion::default()
            },
            10,
        );
        let sample = Sample::mono(48_000.0, vec![1.0; 10]);
        for (pos, gain) in [(8.0, 0.0), (7.0, 0.5), (6.0, 1.0), (2.0, 0.25), (1.0, 0.0)] {
            assert_eq!(backward.read(&sample, pos, true), (gain, gain));
        }
    }

    #[test]
    fn loops_follow_direction_and_wrap_large_steps() {
        let region = PlaybackRegion {
            looping: Some(PlaybackLoop {
                start: 2.0,
                end: 6.0,
                crossfade: 0.0,
            }),
            ..PlaybackRegion::default()
        };
        for (direction, expected) in [
            (
                PlaybackDirection::Forward,
                [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 2.0, 3.0],
            ),
            (
                PlaybackDirection::Reverse,
                [7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 5.0, 4.0],
            ),
            (
                PlaybackDirection::PingPong,
                [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0],
            ),
        ] {
            let mut state = playback(
                PlaybackRegion {
                    direction,
                    ..region
                },
                8,
            );
            let mut pos = state.start(0.0);
            for frame in expected {
                assert_eq!(pos, frame, "{direction:?}");
                pos = state.advance(pos, 1.0);
            }
            let pos = state.advance(pos, 102.5);
            assert!((2.0..6.0).contains(&pos), "{direction:?}");
        }
    }

    #[test]
    fn crossfade_softens_wrap_without_shortening_period() {
        let mut state = playback(
            PlaybackRegion {
                looping: Some(PlaybackLoop {
                    start: 0.0,
                    end: 8.0,
                    crossfade: 4.0,
                }),
                ..PlaybackRegion::default()
            },
            8,
        );
        let sample = Sample::mono(48_000.0, vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(state.read(&sample, 0.0, true), (0.0, 0.0));
        assert_eq!(state.read(&sample, 7.0, true), (0.75, 0.75));
        assert_eq!(state.advance(7.0, 1.0), 0.0);
        assert_eq!(state.read(&sample, 0.0, true), (0.5, 0.5));
        assert_eq!(state.read(&sample, 1.0, true), (0.25, 0.25));
        let mut pos = 0.0;
        for frame in 1..=24 {
            pos = state.advance(pos, 1.0);
            assert_eq!(pos, (frame % 8) as f64);
        }
    }

    #[test]
    fn short_ping_pong_window_uses_unlooped_playback() {
        let mut state = playback(
            PlaybackRegion {
                looping: Some(PlaybackLoop {
                    start: 3.0,
                    end: 4.0,
                    crossfade: 0.0,
                }),
                direction: PlaybackDirection::PingPong,
                ..PlaybackRegion::default()
            },
            8,
        );
        assert_eq!(state.advance(3.0, 1.0), 4.0);
    }
}
