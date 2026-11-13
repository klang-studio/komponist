//! Transport (ARCHITECTURE §6.4 step 3): play state, song position, loop, count-in.
//!
//! The position is a song sample: sample 0 is tick 0, and an event at tick `t` happens at sample
//! `TempoMap::tick_to_sample(t)`. A block is cut into segments at the loop end (where the position
//! jumps back to the loop start, sample-exact) and at the end of the count-in (where capture
//! starts). Every segment is processed as one call per processor, with the transport valid for its
//! first frame.
//!
//! Rules:
//! - The loop wraps when playback reaches the loop end from before it. Starting at or behind the
//!   end plays on. An empty loop never wraps.
//! - No wrap during the count-in, so the count-in always reaches the capture start.

use komponist_model::{Meter, Tempo, TempoMap, Tick, TickRange};

use crate::api::TransportState;
use crate::processor::TransportInfo;
use crate::program::Program;

/// Song settings the transport needs, copied from the program (defaults without one).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Song {
    pub(crate) map: TempoMap,
    pub(crate) loop_range: TickRange,
    pub(crate) loop_on: bool,
    pub(crate) metronome: bool,
    pub(crate) count_in_bars: u8,
}

impl Song {
    /// The settings of a program.
    pub(crate) fn of(program: &Program) -> Song {
        Song {
            map: program.tempo_map,
            loop_range: program.loop_range,
            loop_on: program.loop_on,
            metronome: program.metronome,
            count_in_bars: program.count_in_bars,
        }
    }

    /// The loop in song samples `(start, end)`, if it is on and not empty.
    pub(crate) fn loop_samples(&self, sample_rate: f64) -> Option<(i64, i64)> {
        if !self.loop_on {
            return None;
        }
        let start = self.map.tick_to_sample(self.loop_range.start, sample_rate);
        let end = self.map.tick_to_sample(self.loop_range.end, sample_rate);
        (end > start).then_some((start, end))
    }
}

impl Default for Song {
    /// 120 bpm, 4/4, loop off, no metronome, one bar count-in.
    fn default() -> Song {
        Song {
            map: TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR),
            loop_range: TickRange::default(),
            loop_on: false,
            metronome: false,
            count_in_bars: 1,
        }
    }
}

/// What happens at the end of a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentEnd {
    /// Nothing; playback (or standstill) goes on.
    Continue,
    /// The segment ends at the loop end: jump to the loop start.
    Wrap,
}

/// Play state and position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Transport {
    pub(crate) state: TransportState,
    /// Song sample of the next frame.
    pub(crate) pos: i64,
    /// Capture start of the running recording (meaningful in `CountIn`).
    pub(crate) record_from: Tick,
    /// The loop wrapped since playback started (recording latency correction).
    pub(crate) wrapped: bool,
}

impl Default for Transport {
    fn default() -> Transport {
        Transport {
            state: TransportState::Stopped,
            pos: 0,
            record_from: Tick::ZERO,
            wrapped: false,
        }
    }
}

impl Transport {
    /// Whether the transport runs (count-in included).
    pub(crate) fn playing(&self) -> bool {
        self.state != TransportState::Stopped
    }

    /// The playhead tick (floored).
    pub(crate) fn tick(&self, song: &Song, sample_rate: f64) -> Tick {
        song.map.sample_to_tick(self.pos, sample_rate)
    }

    /// The count-in ends at the position: switches to `Recording` and returns the capture start.
    /// Called at the start of every segment.
    pub(crate) fn count_in_done(&mut self, song: &Song, sample_rate: f64) -> Option<Tick> {
        let end = song.map.tick_to_sample(self.record_from, sample_rate);
        if self.state == TransportState::CountIn && self.pos >= end {
            self.state = TransportState::Recording;
            Some(self.record_from)
        } else {
            None
        }
    }

    /// Length (1 ..= `max`, `max` ≥ 1) of the next segment and what happens at its end.
    pub(crate) fn next_segment(
        &self,
        max: usize,
        song: &Song,
        sample_rate: f64,
    ) -> (usize, SegmentEnd) {
        let max = max as i64;
        match self.state {
            TransportState::Stopped => (max as usize, SegmentEnd::Continue),
            TransportState::CountIn => {
                let end = song.map.tick_to_sample(self.record_from, sample_rate);
                (
                    (end - self.pos).clamp(1, max) as usize,
                    SegmentEnd::Continue,
                )
            }
            TransportState::Playing | TransportState::Recording => {
                match song.loop_samples(sample_rate) {
                    Some((_, end)) if self.pos < end && end - self.pos <= max => {
                        ((end - self.pos) as usize, SegmentEnd::Wrap)
                    }
                    _ => (max as usize, SegmentEnd::Continue),
                }
            }
        }
    }

    /// Advances by a segment of `frames` that ended with `end`. Returns the loop start tick after
    /// a wrap.
    pub(crate) fn advance(
        &mut self,
        frames: usize,
        end: SegmentEnd,
        song: &Song,
        sample_rate: f64,
    ) -> Option<Tick> {
        if !self.playing() {
            return None;
        }
        self.pos += frames as i64;
        match (end, song.loop_samples(sample_rate)) {
            (SegmentEnd::Wrap, Some((start, _))) => {
                self.pos = start;
                self.wrapped = true;
                Some(song.loop_range.start)
            }
            _ => None,
        }
    }

    /// The transport snapshot for processors at the current position.
    pub(crate) fn info(&self, song: &Song, sample_rate: f64) -> TransportInfo {
        TransportInfo {
            playing: self.playing(),
            recording: self.state == TransportState::Recording,
            looping: song.loop_on,
            tempo_bpm: song.map.tempo.bpm,
            meter: song.map.meter,
            position_ticks: song.map.samples_to_ticks(self.pos as f64, sample_rate),
            loop_range: song.loop_range,
        }
    }

    /// Keeps the musical position when the tempo map or the sample rate changes.
    pub(crate) fn rescale(&mut self, from: (&TempoMap, f64), to: (&TempoMap, f64)) {
        let ticks = from.0.samples_to_ticks(self.pos as f64, from.1);
        self.pos = (ticks * to.0.samples_per_tick(to.1) + 0.5).floor() as i64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn looping(start: i64, end: i64) -> Song {
        Song {
            loop_range: TickRange::new(Tick(start), Tick(end)),
            loop_on: true,
            ..Song::default()
        }
    }

    /// Runs the transport over `total` frames in blocks of `block`: the segments as
    /// `(start sample, frames)` and the wraps.
    fn run(t: &mut Transport, song: &Song, total: usize, block: usize) -> Vec<(i64, usize)> {
        let mut segments = Vec::new();
        let mut done = 0;
        while done < total {
            let max = block.min(total - done);
            let mut off = 0;
            while off < max {
                t.count_in_done(song, RATE);
                let (n, end) = t.next_segment(max - off, song, RATE);
                segments.push((t.pos, n));
                t.advance(n, end, song, RATE);
                off += n;
            }
            done += max;
        }
        segments
    }

    #[test]
    fn stopped_stands_still() {
        let mut t = Transport::default();
        let segs = run(&mut t, &Song::default(), 1000, 300);
        assert_eq!(segs, [(0, 300), (0, 300), (0, 300), (0, 100)]);
    }

    #[test]
    fn loop_wraps_mid_block_at_the_exact_sample() {
        // One 4/4 bar at 120 bpm and 48 kHz: 96 000 samples.
        let song = looping(0, 3840);
        let mut t = Transport {
            state: TransportState::Playing,
            pos: 95_000,
            ..Transport::default()
        };
        let segs = run(&mut t, &song, 2048, 2048);
        assert_eq!(segs, [(95_000, 1000), (0, 1048)]);
        assert_eq!(t.pos, 1048);
        assert!(t.wrapped);
    }

    #[test]
    fn loop_shorter_than_a_block_wraps_repeatedly() {
        // 1/16 at 120 bpm: 240 ticks = 6000 samples.
        let song = looping(960, 1200);
        let mut t = Transport {
            state: TransportState::Playing,
            pos: 24_000,
            ..Transport::default()
        };
        let segs = run(&mut t, &song, 15_000, 15_000);
        assert_eq!(segs, [(24_000, 6000), (24_000, 6000), (24_000, 3000)]);
    }

    #[test]
    fn no_wrap_behind_the_loop_end_or_with_an_empty_loop() {
        let mut t = Transport {
            state: TransportState::Playing,
            pos: 96_000,
            ..Transport::default()
        };
        let segs = run(&mut t, &looping(0, 3840), 1000, 1000);
        assert_eq!(segs, [(96_000, 1000)]);
        let mut t = Transport {
            state: TransportState::Playing,
            pos: 0,
            ..Transport::default()
        };
        let segs = run(&mut t, &looping(3840, 3840), 1000, 1000);
        assert_eq!(segs, [(0, 1000)]);
    }

    #[test]
    fn count_in_splits_at_the_capture_start_and_ignores_the_loop() {
        let song = looping(0, 1920);
        let mut t = Transport {
            state: TransportState::CountIn,
            pos: -96_000,
            record_from: Tick::ZERO,
            ..Transport::default()
        };
        let segs = run(&mut t, &song, 96_500, 1024);
        // 93 full blocks, then the count-in ends 768 frames into block 94.
        assert_eq!(segs[93], (-96_000 + 93 * 1024, 768));
        assert_eq!(segs[94], (0, 256));
        assert_eq!(t.state, TransportState::Recording);
        assert_eq!(t.pos, 500);
    }

    #[test]
    fn info_and_rescale() {
        let song = looping(0, 3840);
        let mut t = Transport {
            state: TransportState::Playing,
            pos: 48_000,
            ..Transport::default()
        };
        let info = t.info(&song, RATE);
        assert!(info.playing && !info.recording && info.looping);
        assert_eq!(info.position_ticks, 1920.0);
        assert_eq!(t.tick(&song, RATE), Tick(1920));
        // Twice the tempo: the same tick is half the samples.
        let fast = TempoMap::constant(Tempo::new(240.0), Meter::FOUR_FOUR);
        t.rescale((&song.map, RATE), (&fast, RATE));
        assert_eq!(t.pos, 24_000);
        t.rescale((&fast, RATE), (&fast, 44_100.0));
        assert_eq!(t.pos, 22_050);
    }
}
