//! Overlapped source reads that change pitch without advancing the note timeline.

use crate::{Sample, playback::Playback};

const HOP_SECONDS: f32 = 0.032;
const OVERLAP_SECONDS: f32 = 0.006;
const LOOKAHEAD_SECONDS: f32 = 0.012;
const PROBE_SECONDS: f32 = 0.003;
const PROBES: usize = 8;
const SEARCH_STEPS: usize = 16;
const START_PHASES: u64 = 16;

#[derive(Debug, Clone, Copy)]
struct Grain {
    playback: Playback,
    pos: f64,
}

impl Grain {
    fn step(&mut self, amount: f64) {
        self.pos = self.playback.advance_grain(self.pos, amount);
    }

    fn read(&self, sample: &Sample) -> (f32, f32) {
        self.playback.read(sample, self.pos, false)
    }

    fn probe(mut self, sample: &Sample, step: f64) -> [(f32, f32); PROBES] {
        std::array::from_fn(|_| {
            let value = self.read(sample);
            self.step(step);
            value
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PitchReader {
    current: Grain,
    outgoing: Option<Grain>,
    elapsed: u32,
    hop: u32,
    overlap: u32,
    probe_step: f64,
}

impl PitchReader {
    pub fn new(playback: &Playback, pos: f64, sample_rate: f32, voice_id: u64) -> Self {
        let hop = ((sample_rate * HOP_SECONDS) as u32).max(2);
        // Spread the first search across half a hop when a chord starts together.
        let initial_phase =
            (u64::from(hop) * (voice_id % START_PHASES) / (2 * START_PHASES)) as u32;
        Self {
            current: Grain {
                playback: *playback,
                pos,
            },
            outgoing: None,
            elapsed: initial_phase,
            hop,
            overlap: ((sample_rate * OVERLAP_SECONDS) as u32).clamp(1, hop - 1),
            probe_step: f64::from(sample_rate * PROBE_SECONDS) / (PROBES - 1) as f64,
        }
    }

    pub fn render(
        &mut self,
        sample: &Sample,
        playback: &Playback,
        pos: f64,
        rate: f64,
    ) -> (f32, f32) {
        if self.elapsed == self.hop {
            self.outgoing = Some(self.current);
            self.current = self.align(
                sample,
                Grain {
                    playback: *playback,
                    pos,
                },
                rate,
            );
            self.elapsed = 0;
        }
        let (left, right) = self.current.read(sample);
        self.current.step(rate);
        let output = if let Some(outgoing) = &mut self.outgoing {
            let (a, b) = outgoing.read(sample);
            outgoing.step(rate);
            let w = self.elapsed as f32 / self.overlap as f32;
            (a + (left - a) * w, b + (right - b) * w)
        } else {
            (left, right)
        };
        self.elapsed += 1;
        if self.elapsed == self.overlap {
            self.outgoing = None;
        }
        output
    }

    /// Match the next grain's phase to the old read head in a short source-time
    /// window. Using both channels also handles material with opposite polarity.
    fn align(&self, sample: &Sample, anchor: Grain, rate: f64) -> Grain {
        let reference = self.current.probe(sample, rate * self.probe_step);
        let energy: f32 = reference.iter().map(|&(l, r)| l * l + r * r).sum();
        if energy < 1.0e-10 {
            return anchor;
        }
        let window = f64::from(sample.sample_rate * LOOKAHEAD_SECONDS);
        let spacing = window / SEARCH_STEPS as f64;
        let score = |offset: f64| {
            let mut grain = anchor;
            grain.step(offset);
            let candidate = grain.probe(sample, rate * self.probe_step);
            let (dot, power) = reference
                .iter()
                .zip(candidate)
                .fold((0.0, 0.0), |(dot, power), (&(a, b), (l, r))| {
                    (dot + a * l + b * r, power + l * l + r * r)
                });
            let correlation = dot / (energy * power).max(1.0e-20).sqrt();
            // Prefer the anchor when two offsets match about equally well.
            (correlation - 0.02 * (offset / window) as f32, grain)
        };
        let (mut best_score, mut best) = score(0.0);
        let mut best_offset = 0.0;
        for i in 1..=SEARCH_STEPS {
            let offset = i as f64 * spacing;
            let (candidate, grain) = score(offset);
            if candidate > best_score {
                best_score = candidate;
                best = grain;
                best_offset = offset;
            }
        }
        let center = best_offset;
        for i in [-3, -2, -1, 1, 2, 3] {
            let offset = center + f64::from(i) * spacing * 0.25;
            if !(0.0..=window).contains(&offset) {
                continue;
            }
            let (candidate, grain) = score(offset);
            if candidate > best_score {
                best_score = candidate;
                best = grain;
            }
        }
        best
    }
}
