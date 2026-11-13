//! Musical time (ARCHITECTURE §5.1, decision D9).
//!
//! - Positions and lengths are [`Tick`]s, `i64` at [`PPQ`] = 960 ticks per quarter note. One 4/4 bar
//!   is 3840 ticks, one 1/16 step 240 ticks.
//! - The display format `Takt.Schlag.Tick` uses [`DISPLAY_TICKS_PER_BEAT`] = 240 ticks per beat,
//!   so `display_tick = tick_in_beat × 240 / ticks_per_beat` (= `tick_in_beat / 4` in x/4 meters).
//! - The prototype (`ks-engine.js`) stores positions in 1/16 steps (16 steps = one 4/4 bar) and clip
//!   positions in bars. [`Tick::from_steps`] converts the step unit: `Tick::from_steps(s)` =
//!   `s × 240`.
//! - Tempo and meter are constant per project in v0.1. [`TempoMap`] hides this, so tempo changes
//!   can come later without touching callers.
//!
//! Rounding rules (tested by WP-12, relied upon by the engine):
//! - Tick ↔ seconds and tick ↔ samples conversions are exact `f64` math.
//! - [`TempoMap::tick_to_sample`] rounds to the nearest sample, ties up (`floor(x + 0.5)`). A note at
//!   tick `t` starts exactly at sample `tick_to_sample(t, rate)`.
//! - [`TempoMap::sample_to_tick`] floors, so the displayed playhead never runs ahead of the audio.

use core::ops::{Add, AddAssign, Neg, Sub, SubAssign};

use serde::{Deserialize, Serialize};

/// Ticks per quarter note.
pub const PPQ: i64 = 960;

/// Ticks per beat in the display format `Takt.Schlag.Tick` (`10.2.120`): 0 ..= 239.
pub const DISPLAY_TICKS_PER_BEAT: i64 = 240;

/// Ticks per 1/16 step, the position unit of the prototype and of the step sequencer at 1/16.
pub const TICKS_PER_STEP: i64 = PPQ / 4;

/// A musical position or length in ticks at [`PPQ`] = 960.
///
/// Negative positions occur only during the count-in (display "Vorzähler"). Document objects never
/// start before tick 0.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
    /// Tick 0, the start of bar 1.
    pub const ZERO: Tick = Tick(0);

    /// Converts the prototype's 1/16 step unit (`ks-engine.js` positions): `steps × 240`.
    pub const fn from_steps(steps: i64) -> Tick {
        Tick(steps * TICKS_PER_STEP)
    }

    /// Converts quarter notes: `quarters × 960`.
    pub const fn from_quarters(quarters: i64) -> Tick {
        Tick(quarters * PPQ)
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, rhs: Tick) -> Tick {
        Tick(self.0 + rhs.0)
    }
}

impl Sub for Tick {
    type Output = Tick;
    fn sub(self, rhs: Tick) -> Tick {
        Tick(self.0 - rhs.0)
    }
}

impl Neg for Tick {
    type Output = Tick;
    fn neg(self) -> Tick {
        Tick(-self.0)
    }
}

impl AddAssign for Tick {
    fn add_assign(&mut self, rhs: Tick) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Tick {
    fn sub_assign(&mut self, rhs: Tick) {
        self.0 -= rhs.0;
    }
}

/// Project tempo in beats (quarter notes) per minute.
///
/// Valid range [`Tempo::MIN_BPM`] ..= [`Tempo::MAX_BPM`] (the prototype's tempo drag range); edits
/// reject other values. Displayed with two decimals (`124.00`).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Tempo {
    /// Quarter notes per minute.
    pub bpm: f64,
}

impl Tempo {
    /// Lowest accepted tempo.
    pub const MIN_BPM: f64 = 40.0;
    /// Highest accepted tempo.
    pub const MAX_BPM: f64 = 240.0;
    /// Tempo of a new project and the Alt-click default of the tempo field.
    pub const DEFAULT: Tempo = Tempo { bpm: 120.0 };

    /// A tempo of `bpm` quarter notes per minute. Does not validate.
    pub const fn new(bpm: f64) -> Tempo {
        Tempo { bpm }
    }
}

impl Default for Tempo {
    fn default() -> Tempo {
        Tempo::DEFAULT
    }
}

/// Time signature (`4/4`).
///
/// Valid: `num` 1 ..= 32, `den` one of 1, 2, 4, 8, 16. A beat is one `1/den` note. v0.1 projects
/// use 4/4; the type allows others so the display and grid math stay general.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Meter {
    /// Beats per bar.
    pub num: u8,
    /// Note value of one beat (4 = quarter note).
    pub den: u8,
}

impl Meter {
    /// 4/4, the meter of new projects.
    pub const FOUR_FOUR: Meter = Meter { num: 4, den: 4 };

    /// Ticks per beat: `PPQ × 4 / den` (960 in x/4, 480 in x/8).
    pub const fn ticks_per_beat(self) -> i64 {
        PPQ * 4 / self.den as i64
    }

    /// Ticks per bar: `num × ticks_per_beat` (3840 in 4/4).
    pub const fn ticks_per_bar(self) -> i64 {
        self.num as i64 * self.ticks_per_beat()
    }
}

impl Default for Meter {
    fn default() -> Meter {
        Meter::FOUR_FOUR
    }
}

/// A half-open tick range `start .. end`.
///
/// Used for the loop range, export ranges and selections. `end <= start` means empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TickRange {
    /// First tick inside the range.
    pub start: Tick,
    /// First tick after the range.
    pub end: Tick,
}

impl TickRange {
    /// The range `start .. end`.
    pub const fn new(start: Tick, end: Tick) -> TickRange {
        TickRange { start, end }
    }

    /// Length in ticks, 0 when empty.
    pub fn len(self) -> Tick {
        Tick((self.end.0 - self.start.0).max(0))
    }

    /// Whether the range contains no tick.
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// Whether `t` lies inside `start .. end`.
    pub fn contains(self, t: Tick) -> bool {
        self.start <= t && t < self.end
    }
}

/// A position split into bar, beat and display tick, all as shown in `Takt.Schlag.Tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bbt {
    /// Bar number, 1-based. Tick 0 is bar 1; count-in positions give bar 0 or less (floor
    /// division).
    pub bar: i64,
    /// Beat within the bar, 1-based (1 ..= `meter.num`).
    pub beat: u32,
    /// Display tick within the beat, 0 ..= 239 ([`DISPLAY_TICKS_PER_BEAT`]), floored.
    pub tick: u32,
}

/// Maps ticks to bars, beats, seconds and samples.
///
/// v0.1 has one segment (constant tempo and meter). All conversions take the map, so tempo changes
/// later only change this type. Cheap to copy; build it with [`Project::tempo_map`](crate::Project::tempo_map).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoMap {
    /// The tempo of the only segment.
    pub tempo: Tempo,
    /// The meter of the only segment.
    pub meter: Meter,
}

impl TempoMap {
    /// A map with one constant segment.
    pub const fn constant(tempo: Tempo, meter: Meter) -> TempoMap {
        TempoMap { tempo, meter }
    }

    /// Seconds per tick: `60 / (bpm × PPQ)`.
    pub fn seconds_per_tick(&self) -> f64 {
        60.0 / (self.tempo.bpm * PPQ as f64)
    }

    /// Samples per tick at `sample_rate` Hz.
    pub fn samples_per_tick(&self, sample_rate: f64) -> f64 {
        self.seconds_per_tick() * sample_rate
    }

    /// Exact time of `tick` in seconds from tick 0 (negative during the count-in).
    pub fn tick_to_seconds(&self, tick: Tick) -> f64 {
        tick.0 as f64 * self.seconds_per_tick()
    }

    /// Exact (fractional) tick at `seconds` from tick 0.
    pub fn seconds_to_ticks(&self, seconds: f64) -> f64 {
        seconds / self.seconds_per_tick()
    }

    /// Exact (fractional) sample position of `tick` at `sample_rate` Hz, sample 0 = tick 0.
    pub fn tick_to_samples(&self, tick: Tick, sample_rate: f64) -> f64 {
        tick.0 as f64 * self.samples_per_tick(sample_rate)
    }

    /// The sample at which an event at `tick` is rendered: nearest sample, ties up
    /// (`floor(x + 0.5)`).
    pub fn tick_to_sample(&self, tick: Tick, sample_rate: f64) -> i64 {
        (self.tick_to_samples(tick, sample_rate) + 0.5).floor() as i64
    }

    /// Exact (fractional) tick at `samples` from sample 0.
    pub fn samples_to_ticks(&self, samples: f64, sample_rate: f64) -> f64 {
        samples / self.samples_per_tick(sample_rate)
    }

    /// The tick of sample `sample`, floored (playhead display, recording).
    pub fn sample_to_tick(&self, sample: i64, sample_rate: f64) -> Tick {
        Tick(self.samples_to_ticks(sample as f64, sample_rate).floor() as i64)
    }

    /// First tick of bar `bar` (1-based): `(bar − 1) × ticks_per_bar`.
    pub fn bar_start(&self, bar: i64) -> Tick {
        Tick((bar - 1) * self.meter.ticks_per_bar())
    }

    /// Splits `tick` into bar, beat and display tick (floor division, so it works for negative
    /// count-in ticks too).
    pub fn bbt(&self, tick: Tick) -> Bbt {
        let per_bar = self.meter.ticks_per_bar();
        let per_beat = self.meter.ticks_per_beat();
        let in_bar = tick.0.rem_euclid(per_bar);
        Bbt {
            bar: tick.0.div_euclid(per_bar) + 1,
            beat: (in_bar / per_beat) as u32 + 1,
            tick: ((in_bar % per_beat) * DISPLAY_TICKS_PER_BEAT / per_beat) as u32,
        }
    }

    /// The tick of a bar/beat/display-tick position; inverse of [`TempoMap::bbt`] for display-tick
    /// granularity (`from_bbt(bbt(t))` equals `t` rounded down to a multiple of
    /// `ticks_per_beat / 240`).
    pub fn from_bbt(&self, bbt: Bbt) -> Tick {
        let per_beat = self.meter.ticks_per_beat();
        Tick(
            (bbt.bar - 1) * self.meter.ticks_per_bar()
                + (i64::from(bbt.beat) - 1) * per_beat
                + i64::from(bbt.tick) * per_beat / DISPLAY_TICKS_PER_BEAT,
        )
    }
}

/// Snap and quantise grid of the arrangement and the piano roll ("Raster", 1 Takt … 1/32 and
/// triplets).
///
/// `Ctrl [` / `Ctrl ]` step through [`Grid::finer`] / [`Grid::coarser`]. The straight and the triplet
/// values form two separate ladders; stepping never leaves a ladder and stops at its ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Grid {
    /// One bar of the project meter ("1 Takt").
    Bar,
    /// 1/2 note, 1920 ticks.
    Half,
    /// 1/4 note, 960 ticks.
    Quarter,
    /// 1/8 note, 480 ticks.
    Eighth,
    /// 1/16 note, 240 ticks (default of the piano roll).
    #[default]
    Sixteenth,
    /// 1/32 note, 120 ticks.
    ThirtySecond,
    /// 1/4 triplet, 640 ticks.
    QuarterTriplet,
    /// 1/8 triplet, 320 ticks.
    EighthTriplet,
    /// 1/16 triplet, 160 ticks.
    SixteenthTriplet,
}

impl Grid {
    /// Grid spacing in ticks; `meter` matters only for [`Grid::Bar`].
    pub fn ticks(self, meter: Meter) -> Tick {
        Tick(match self {
            Grid::Bar => meter.ticks_per_bar(),
            Grid::Half => 2 * PPQ,
            Grid::Quarter => PPQ,
            Grid::Eighth => PPQ / 2,
            Grid::Sixteenth => PPQ / 4,
            Grid::ThirtySecond => PPQ / 8,
            Grid::QuarterTriplet => PPQ * 2 / 3,
            Grid::EighthTriplet => PPQ / 3,
            Grid::SixteenthTriplet => PPQ / 6,
        })
    }

    /// The next finer grid on the same ladder (`Ctrl [`); the finest stays.
    pub fn finer(self) -> Grid {
        match self {
            Grid::Bar => Grid::Half,
            Grid::Half => Grid::Quarter,
            Grid::Quarter => Grid::Eighth,
            Grid::Eighth => Grid::Sixteenth,
            Grid::Sixteenth | Grid::ThirtySecond => Grid::ThirtySecond,
            Grid::QuarterTriplet => Grid::EighthTriplet,
            Grid::EighthTriplet | Grid::SixteenthTriplet => Grid::SixteenthTriplet,
        }
    }

    /// The next coarser grid on the same ladder (`Ctrl ]`); the coarsest stays.
    pub fn coarser(self) -> Grid {
        match self {
            Grid::Bar | Grid::Half => Grid::Bar,
            Grid::Quarter => Grid::Half,
            Grid::Eighth => Grid::Quarter,
            Grid::Sixteenth => Grid::Eighth,
            Grid::ThirtySecond => Grid::Sixteenth,
            Grid::QuarterTriplet | Grid::EighthTriplet => Grid::QuarterTriplet,
            Grid::SixteenthTriplet => Grid::EighthTriplet,
        }
    }

    /// Rounds `tick` to the nearest grid line (ties away from zero). Grid lines are multiples of
    /// [`Grid::ticks`] counted from tick 0.
    pub fn snap(self, tick: Tick, meter: Meter) -> Tick {
        let g = self.ticks(meter).0;
        let a = tick.0.abs();
        let mut q = a / g;
        if 2 * (a % g) >= g {
            q += 1;
        }
        Tick(tick.0.signum() * q * g)
    }

    /// Rounds `tick` down to the grid line at or before it.
    pub fn snap_floor(self, tick: Tick, meter: Meter) -> Tick {
        let g = self.ticks(meter).0;
        Tick(tick.0.div_euclid(g) * g)
    }
}

/// Step length of a step clip ("Auflösung": 1/8, 1/16, 1/32 and triplets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StepResolution {
    /// 1/8, 480 ticks per step.
    Eighth,
    /// 1/16, 240 ticks per step (default; 16 steps = one 4/4 bar).
    #[default]
    Sixteenth,
    /// 1/32, 120 ticks per step.
    ThirtySecond,
    /// 1/8 triplet, 320 ticks per step.
    EighthTriplet,
    /// 1/16 triplet, 160 ticks per step.
    SixteenthTriplet,
}

impl StepResolution {
    /// Ticks per step.
    pub const fn ticks(self) -> Tick {
        Tick(match self {
            StepResolution::Eighth => PPQ / 2,
            StepResolution::Sixteenth => PPQ / 4,
            StepResolution::ThirtySecond => PPQ / 8,
            StepResolution::EighthTriplet => PPQ / 3,
            StepResolution::SixteenthTriplet => PPQ / 6,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototype_steps_convert_to_ticks() {
        assert_eq!(Tick::from_steps(16), Tick(3840));
        assert_eq!(Tick::from_steps(1), Tick(240));
        assert_eq!(Tick::from_quarters(1), Tick(PPQ));
    }

    #[test]
    fn meter_lengths() {
        assert_eq!(Meter::FOUR_FOUR.ticks_per_bar(), 3840);
        let six_eight = Meter { num: 6, den: 8 };
        assert_eq!(six_eight.ticks_per_beat(), 480);
        assert_eq!(six_eight.ticks_per_bar(), 2880);
    }

    #[test]
    fn grid_ladders_stay_closed() {
        let m = Meter::FOUR_FOUR;
        assert_eq!(Grid::Bar.ticks(m), Tick(3840));
        assert_eq!(Grid::SixteenthTriplet.ticks(m), Tick(160));
        assert_eq!(Grid::ThirtySecond.finer(), Grid::ThirtySecond);
        assert_eq!(Grid::Bar.coarser(), Grid::Bar);
        assert_eq!(
            Grid::SixteenthTriplet.coarser().coarser(),
            Grid::QuarterTriplet
        );
        assert_eq!(Grid::QuarterTriplet.coarser(), Grid::QuarterTriplet);
    }

    #[test]
    fn tick_sample_rounding() {
        // 120 bpm at 48 kHz: one tick is 25 samples.
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        assert_eq!(map.tick_to_sample(Tick(1), 48_000.0), 25);
        assert_eq!(map.sample_to_tick(49, 48_000.0), Tick(1));
        assert_eq!(map.sample_to_tick(-1, 48_000.0), Tick(-1));
        assert_eq!(map.bar_start(10), Tick(9 * 3840));
    }

    #[test]
    fn bbt_splits_positions() {
        let map = TempoMap::constant(Tempo::new(124.0), Meter::FOUR_FOUR);
        let bbt = |bar, beat, tick| Bbt { bar, beat, tick };
        assert_eq!(map.bbt(Tick(0)), bbt(1, 1, 0));
        // 10.2.120: nine bars, one beat and half a beat.
        assert_eq!(map.bbt(Tick(9 * 3840 + 960 + 480)), bbt(10, 2, 120));
        assert_eq!(map.bbt(Tick(3839)), bbt(1, 4, 239));
        assert_eq!(map.bbt(Tick(3840)), bbt(2, 1, 0));
        // Count-in: floor division.
        assert_eq!(map.bbt(Tick(-1)), bbt(0, 4, 239));
        assert_eq!(map.bbt(Tick(-3840)), bbt(0, 1, 0));
        assert_eq!(map.bbt(Tick(-3841)), bbt(-1, 4, 239));

        let six_eight = TempoMap::constant(Tempo::DEFAULT, Meter { num: 6, den: 8 });
        assert_eq!(six_eight.bbt(Tick(5 * 480 + 100)), bbt(1, 6, 50));
        assert_eq!(six_eight.bbt(Tick(2880)), bbt(2, 1, 0));
    }

    #[test]
    fn from_bbt_inverts_bbt() {
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        let t = Tick(9 * 3840 + 960 + 483);
        assert_eq!(map.from_bbt(map.bbt(t)), Tick(9 * 3840 + 960 + 480));
        assert_eq!(map.from_bbt(map.bbt(Tick(-1))), Tick(-4));
        assert_eq!(map.bar_start(1), map.from_bbt(map.bbt(Tick::ZERO)));
    }

    #[test]
    fn seconds_follow_the_tempo() {
        let map = TempoMap::constant(Tempo::new(124.0), Meter::FOUR_FOUR);
        // 37 beats at 124 bpm.
        let s = map.tick_to_seconds(Tick::from_quarters(37));
        assert!((s - 37.0 * 60.0 / 124.0).abs() < 1e-12);
        assert!((map.seconds_to_ticks(s) - 37.0 * 960.0).abs() < 1e-9);
        assert!((map.tick_to_samples(Tick(960), 48_000.0) - 48_000.0 * 60.0 / 124.0).abs() < 1e-9);
        assert!((map.samples_to_ticks(48_000.0, 48_000.0) - 960.0 * 124.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn tick_to_sample_rounds_ties_up() {
        // 120 bpm at 44.1 kHz: 22.96875 samples per tick.
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        assert_eq!(map.tick_to_sample(Tick(1), 44_100.0), 23);
        // 3 ticks = 68.90625 samples → 69; 16 ticks = 367.5 samples exactly → 368 (tie up).
        assert_eq!(map.tick_to_sample(Tick(3), 44_100.0), 69);
        assert_eq!(map.tick_to_sample(Tick(16), 44_100.0), 368);
        assert_eq!(map.tick_to_sample(Tick(-16), 44_100.0), -367);
        // Floor on the way back: sample 367 lies before tick 16.
        assert_eq!(map.sample_to_tick(367, 44_100.0), Tick(15));
        assert_eq!(map.sample_to_tick(368, 44_100.0), Tick(16));
    }

    #[test]
    fn snap_rounds_to_the_nearest_line() {
        let m = Meter::FOUR_FOUR;
        let g = Grid::Sixteenth;
        assert_eq!(g.snap(Tick(119), m), Tick(0));
        assert_eq!(g.snap(Tick(120), m), Tick(240));
        assert_eq!(g.snap(Tick(121), m), Tick(240));
        assert_eq!(g.snap(Tick(-119), m), Tick(0));
        assert_eq!(g.snap(Tick(-120), m), Tick(-240));
        assert_eq!(Grid::Bar.snap(Tick(1919), m), Tick(0));
        assert_eq!(Grid::Bar.snap(Tick(1920), m), Tick(3840));
        assert_eq!(Grid::EighthTriplet.snap(Tick(500), m), Tick(640));
        assert_eq!(
            Grid::Bar.snap(Tick(1500), Meter { num: 3, den: 4 }),
            Tick(2880)
        );
    }

    #[test]
    fn snap_floor_rounds_down() {
        let m = Meter::FOUR_FOUR;
        assert_eq!(Grid::Sixteenth.snap_floor(Tick(239), m), Tick(0));
        assert_eq!(Grid::Sixteenth.snap_floor(Tick(240), m), Tick(240));
        assert_eq!(Grid::Sixteenth.snap_floor(Tick(-1), m), Tick(-240));
        assert_eq!(
            Grid::Quarter.snap_floor(Tick(36_000), m),
            Tick(36_000 - 480)
        );
    }

    #[test]
    fn step_resolutions() {
        assert_eq!(StepResolution::default().ticks(), Tick(240));
        assert_eq!(StepResolution::EighthTriplet.ticks(), Tick(320));
        assert_eq!(StepResolution::SixteenthTriplet.ticks(), Tick(160));
    }

    #[test]
    fn tick_range_basics() {
        let r = TickRange::new(Tick(10), Tick(20));
        assert_eq!(r.len(), Tick(10));
        assert!(r.contains(Tick(10)) && !r.contains(Tick(20)));
        assert!(TickRange::new(Tick(5), Tick(5)).is_empty());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        const METERS: [Meter; 5] = [
            Meter::FOUR_FOUR,
            Meter { num: 3, den: 4 },
            Meter { num: 6, den: 8 },
            Meter { num: 7, den: 16 },
            Meter { num: 5, den: 2 },
        ];

        const GRIDS: [Grid; 9] = [
            Grid::Bar,
            Grid::Half,
            Grid::Quarter,
            Grid::Eighth,
            Grid::Sixteenth,
            Grid::ThirtySecond,
            Grid::QuarterTriplet,
            Grid::EighthTriplet,
            Grid::SixteenthTriplet,
        ];

        proptest! {
            #[test]
            fn bbt_round_trip(t in -100_000i64..10_000_000, m in 0usize..METERS.len()) {
                let meter = METERS[m];
                let map = TempoMap::constant(Tempo::DEFAULT, meter);
                let bbt = map.bbt(Tick(t));
                prop_assert!(bbt.beat >= 1 && bbt.beat <= u32::from(meter.num));
                prop_assert!(bbt.tick < DISPLAY_TICKS_PER_BEAT as u32);
                let unit = meter.ticks_per_beat() / DISPLAY_TICKS_PER_BEAT;
                prop_assert_eq!(map.from_bbt(bbt), Tick(t.div_euclid(unit) * unit));
            }

            #[test]
            fn snap_lands_on_the_nearest_line(
                t in -1_000_000i64..1_000_000,
                g in 0usize..GRIDS.len(),
                m in 0usize..METERS.len(),
            ) {
                let (grid, meter) = (GRIDS[g], METERS[m]);
                let step = grid.ticks(meter).0;
                let snapped = grid.snap(Tick(t), meter).0;
                prop_assert_eq!(snapped % step, 0);
                prop_assert!(2 * (snapped - t).abs() <= step);
                let floor = grid.snap_floor(Tick(t), meter).0;
                prop_assert_eq!(floor % step, 0);
                prop_assert!(floor <= t && t < floor + step);
            }

            #[test]
            fn sample_and_tick_conversions_agree(
                s in -10_000_000i64..100_000_000,
                bpm in 40.0f64..=240.0,
                rate in prop::sample::select(vec![44_100.0f64, 48_000.0, 96_000.0]),
            ) {
                let map = TempoMap::constant(Tempo::new(bpm), Meter::FOUR_FOUR);
                let tick = map.sample_to_tick(s, rate);
                // Floor: the tick starts at or before the sample, the next one after it.
                let eps = 1e-6;
                prop_assert!(map.tick_to_samples(tick, rate) <= s as f64 + eps);
                prop_assert!(map.tick_to_samples(tick + Tick(1), rate) > s as f64 - eps);
                // Nearest sample of a tick, within half a sample.
                let back = map.tick_to_sample(tick, rate);
                prop_assert!((back as f64 - map.tick_to_samples(tick, rate)).abs() <= 0.5 + eps);
            }
        }
    }
}
