//! DAHDSR envelope (delay, attack, hold, decay, sustain, release) evaluated
//! at control rate (once per block). Stage times have block resolution.
//!
//! Attack, decay and release each run for exactly their time, along a curve
//! set per stage (-1..=1): 0 is a straight line; positive values bend it
//! convex ("exponential": the attack starts slowly, decay and release fall
//! fast and then tail off, like an analog envelope), negative values concave
//! ("logarithmic"). At ±1 the bend is as strong as a 60 dB exponential over
//! the stage, so a decay or release curve of 1 is the classic exponential
//! fall (the default).
//!
//! A looping envelope repeats attack, hold and decay while the note is held:
//! at the end of the decay it attacks again from the sustain level instead of
//! staying there, so the level cycles between the sustain level and 1. The
//! delay runs only once, the release as usual.

/// Level below which a release start counts as silent (-80 dB).
const SILENCE: f32 = 1e-4;

/// ln(1000): curvature at ±1, a 60 dB exponential over the stage.
const LN_1000: f32 = 6.907_755;

/// Shortest allowed attack/decay/release time, avoids division by zero and clicks.
const MIN_TIME: f32 = 0.001;

/// Progress counted as the end of a stage, absorbs the rounding of the
/// summed steps so a stage of exactly N blocks takes N blocks.
const STAGE_END: f32 = 1.0 - 1e-4;

/// Longest accepted stage time in seconds.
pub const MAX_TIME: f32 = 30.0;

/// User-facing envelope parameters. Times in seconds, sustain in 0..=1,
/// curves in -1..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct EnvelopeParams {
    /// Wait after note-on before the attack starts.
    pub delay: f32,
    pub attack: f32,
    /// Time at full level between attack and decay.
    pub hold: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// Stage curves, see the module docs; 0 = linear.
    pub attack_curve: f32,
    pub decay_curve: f32,
    pub release_curve: f32,
    /// Repeat attack, hold and decay while the note is held, see the module
    /// docs. Patches from before default to off.
    pub looping: bool,
}

impl EnvelopeParams {
    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::default();
        let time = |x: f32, fallback: f32| crate::clamp_or(x, 0.0, MAX_TIME, fallback);
        let curve = |x: f32, fallback: f32| crate::clamp_or(x, -1.0, 1.0, fallback);
        Self {
            delay: time(self.delay, d.delay),
            attack: time(self.attack, d.attack),
            hold: time(self.hold, d.hold),
            decay: time(self.decay, d.decay),
            sustain: crate::clamp_or(self.sustain, 0.0, 1.0, d.sustain),
            release: time(self.release, d.release),
            attack_curve: curve(self.attack_curve, d.attack_curve),
            decay_curve: curve(self.decay_curve, d.decay_curve),
            release_curve: curve(self.release_curve, d.release_curve),
            ..self
        }
    }
}

impl Default for EnvelopeParams {
    /// A linear attack, exponential decay and release.
    fn default() -> Self {
        Self {
            delay: 0.0,
            attack: 0.005,
            hold: 0.0,
            decay: 0.3,
            sustain: 0.7,
            release: 0.3,
            attack_curve: 0.0,
            decay_curve: 1.0,
            release_curve: 1.0,
            looping: false,
        }
    }
}

/// A rising stage shape 0..=1 → 0..=1 with curvature `k`:
/// (e^(k t) - 1) / (e^k - 1); `inv` = 1 / (e^k - 1). Convex for k > 0,
/// concave for k < 0, linear for k = 0.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Shape {
    k: f32,
    inv: f32,
}

impl Shape {
    /// The rising shape for a stage curve; `falling` mirrors it so a
    /// positive curve is convex for decays and releases too.
    fn new(curve: f32, falling: bool) -> Self {
        let k = curve.clamp(-1.0, 1.0) * LN_1000 * if falling { -1.0 } else { 1.0 };
        let inv = if k.abs() < 1e-3 {
            0.0
        } else {
            1.0 / (libm::expf(k) - 1.0)
        };
        Self { k, inv }
    }

    /// Progress 0..=1 along the rising shape.
    #[inline]
    fn rise(&self, t: f32) -> f32 {
        if self.inv == 0.0 {
            t
        } else {
            (libm::expf(self.k * t) - 1.0) * self.inv
        }
    }

    /// Remaining fraction 1..=0 of a falling stage.
    #[inline]
    fn fall(&self, t: f32) -> f32 {
        1.0 - self.rise(t)
    }
}

/// Per-block coefficients derived from [`EnvelopeParams`] for a given block
/// duration. Computed once per block and shared by all voices.
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeCoefs {
    block_seconds: f32,
    delay: f32,
    hold: f32,
    sustain: f32,
    /// Progress per block (fraction of the stage).
    attack_step: f32,
    decay_step: f32,
    release_step: f32,
    attack_shape: Shape,
    decay_shape: Shape,
    release_shape: Shape,
    looping: bool,
}

impl EnvelopeCoefs {
    /// Placeholder for envelopes that are not advanced.
    pub(crate) const UNUSED: Self = Self {
        block_seconds: 0.0,
        delay: 0.0,
        hold: 0.0,
        sustain: 0.0,
        attack_step: 0.0,
        decay_step: 0.0,
        release_step: 0.0,
        attack_shape: Shape { k: 0.0, inv: 0.0 },
        decay_shape: Shape { k: 0.0, inv: 0.0 },
        release_shape: Shape { k: 0.0, inv: 0.0 },
        looping: false,
    };

    pub fn new(p: &EnvelopeParams, block_seconds: f32) -> Self {
        let step = |time: f32| block_seconds / time.max(MIN_TIME);
        Self {
            block_seconds,
            delay: p.delay.max(0.0),
            hold: p.hold.max(0.0),
            sustain: p.sustain.clamp(0.0, 1.0),
            attack_step: step(p.attack),
            decay_step: step(p.decay),
            release_step: step(p.release),
            attack_shape: Shape::new(p.attack_curve, false),
            decay_shape: Shape::new(p.decay_curve, true),
            release_shape: Shape::new(p.release_curve, true),
            looping: p.looping,
        }
    }
}

impl EnvelopeCoefs {
    /// These coefficients with the stages of `p` moved by `offset(stage)`
    /// (0 delay … 5 release, see [`crate::ModTarget::OpEnvelope`]): times on
    /// the cubic scale of [`crate::modulation::time_offset`], the sustain
    /// level directly. The curves stay.
    pub(crate) fn modulated(&self, p: &EnvelopeParams, offset: impl Fn(usize) -> f32) -> Self {
        let time =
            |base: f32, stage: usize| crate::modulation::time_offset(base, offset(stage), MAX_TIME);
        let step = |t: f32| self.block_seconds / t.max(MIN_TIME);
        Self {
            delay: time(p.delay, 0).max(0.0),
            attack_step: step(time(p.attack, 1)),
            hold: time(p.hold, 2).max(0.0),
            decay_step: step(time(p.decay, 3)),
            sustain: (p.sustain + offset(4)).clamp(0.0, 1.0),
            release_step: step(time(p.release, 5)),
            ..*self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
}

/// The envelope of one voice; delay and hold keep the level constant.
#[derive(Clone, Copy, Debug)]
pub struct Envelope {
    stage: Stage,
    level: f32,
    /// Time spent in the current delay or hold stage.
    timer: f32,
    /// Progress 0..=1 through the current attack, decay or release.
    progress: f32,
    /// Level when the current attack or release started.
    start: f32,
}

impl Envelope {
    pub const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            level: 0.0,
            timer: 0.0,
            progress: 0.0,
            start: 0.0,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    /// Where the envelope is, for displays: -1 while idle, else the stage
    /// number plus the fraction done (0 delay, 1 attack, 2 hold, 3 decay,
    /// 4 sustain, 5 release; e.g. 1.25 = a quarter through the attack).
    /// `params` gives the delay and hold times.
    pub fn position(&self, params: &EnvelopeParams) -> f32 {
        let part = |done: f32, of: f32| {
            if of > 0.0 {
                (done / of).clamp(0.0, 0.999)
            } else {
                0.0
            }
        };
        match self.stage {
            Stage::Idle => -1.0,
            Stage::Delay => part(self.timer, params.delay),
            Stage::Attack => 1.0 + self.progress.clamp(0.0, 0.999),
            Stage::Hold => 2.0 + part(self.timer, params.hold),
            Stage::Decay => 3.0 + self.progress.clamp(0.0, 0.999),
            Stage::Sustain => 4.0,
            Stage::Release => 5.0 + self.progress.clamp(0.0, 0.999),
        }
    }

    /// Starts the delay, then the attack from the current level, so
    /// retriggering does not click.
    pub fn gate_on(&mut self) {
        self.stage = Stage::Delay;
        self.timer = 0.0;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
            self.progress = 0.0;
            self.start = self.level;
        }
    }

    /// Hard reset to silence, used when a voice is stolen.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Advances the envelope by one block and returns the level at its end.
    ///
    /// A zero delay or hold falls through to the next stage within the same
    /// block, so it costs no extra block.
    pub fn advance(&mut self, c: &EnvelopeCoefs) -> f32 {
        self.advance_scaled(c, 1.0)
    }

    /// Like [`Self::advance`], with all times divided by `speed` (keyboard
    /// rate scaling): the block counts as `speed` blocks.
    #[inline]
    pub fn advance_scaled(&mut self, c: &EnvelopeCoefs, speed: f32) -> f32 {
        if self.stage == Stage::Delay {
            self.timer += c.block_seconds * speed;
            if self.timer < c.delay {
                return self.level;
            }
            self.stage = Stage::Attack;
            self.progress = 0.0;
            self.start = self.level;
        }
        if self.stage == Stage::Attack {
            self.progress += c.attack_step * speed;
            if self.progress >= STAGE_END {
                self.level = 1.0;
                self.stage = Stage::Hold;
                self.timer = 0.0;
            } else {
                self.level = self.start + (1.0 - self.start) * c.attack_shape.rise(self.progress);
            }
            return self.level;
        }
        if self.stage == Stage::Hold {
            self.timer += c.block_seconds * speed;
            if self.timer < c.hold {
                return self.level;
            }
            self.stage = Stage::Decay;
            self.progress = 0.0;
        }
        match self.stage {
            Stage::Decay => {
                self.progress += c.decay_step * speed;
                if self.progress >= STAGE_END {
                    self.level = c.sustain;
                    if c.looping {
                        // Attack again from here in the next block.
                        self.stage = Stage::Attack;
                        self.progress = 0.0;
                        self.start = self.level;
                    } else {
                        self.stage = Stage::Sustain;
                    }
                } else {
                    self.level = c.sustain + (1.0 - c.sustain) * c.decay_shape.fall(self.progress);
                }
            }
            Stage::Sustain => self.level = c.sustain,
            Stage::Release => {
                self.progress += c.release_step * speed;
                if self.progress >= STAGE_END || self.start < SILENCE {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                } else {
                    self.level = self.start * c.release_shape.fall(self.progress);
                }
            }
            Stage::Idle | Stage::Delay | Stage::Attack | Stage::Hold => {}
        }
        self.level
    }
}

impl Default for Envelope {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK_SECONDS: f32 = 32.0 / 48_000.0;

    fn blocks(seconds: f32) -> usize {
        (seconds / BLOCK_SECONDS).ceil() as usize
    }

    #[test]
    fn full_cycle() {
        let p = EnvelopeParams {
            attack: 0.01,
            decay: 0.1,
            sustain: 0.5,
            release: 0.2,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        assert!(env.is_idle());

        env.gate_on();
        for _ in 0..blocks(p.attack) {
            env.advance(&c);
        }
        assert_eq!(env.stage(), Stage::Hold);
        assert_eq!(env.level(), 1.0);

        // The decay takes exactly its time.
        for _ in 0..blocks(p.decay) {
            env.advance(&c);
        }
        assert_eq!(env.stage(), Stage::Sustain);
        assert_eq!(env.level(), 0.5);

        env.gate_off();
        assert_eq!(env.stage(), Stage::Release);
        // So does the release.
        for _ in 0..blocks(p.release) {
            env.advance(&c);
        }
        assert!(env.is_idle());
        assert_eq!(env.level(), 0.0);
    }

    #[test]
    fn release_during_attack_and_retrigger_keeps_level() {
        let p = EnvelopeParams {
            attack: 0.1,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();
        for _ in 0..10 {
            env.advance(&c);
        }
        let before = env.level();
        assert!(before > 0.0 && before < 1.0);
        env.gate_off();
        let released = env.advance(&c);
        assert!(released < before);
        env.gate_on();
        let retriggered = env.advance(&c);
        // The new attack continues upwards from the released level.
        assert!(
            retriggered > released && retriggered - released < 0.05,
            "{released} → {retriggered}"
        );
    }

    #[test]
    fn delay_and_hold_keep_level() {
        let p = EnvelopeParams {
            delay: 0.05,
            attack: 0.01,
            hold: 0.1,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();

        // Silent for the whole delay.
        for _ in 0..blocks(p.delay) - 1 {
            assert_eq!(env.advance(&c), 0.0);
            assert_eq!(env.stage(), Stage::Delay);
        }
        // Attack follows, then the hold keeps full level.
        for _ in 0..blocks(p.attack) + 1 {
            env.advance(&c);
        }
        assert_eq!(env.stage(), Stage::Hold);
        for _ in 0..blocks(p.hold) - 1 {
            assert_eq!(env.advance(&c), 1.0);
            assert_eq!(env.stage(), Stage::Hold);
        }
        env.advance(&c);
        env.advance(&c);
        assert_eq!(env.stage(), Stage::Decay);
        assert!(env.level() < 1.0);
    }

    #[test]
    fn zero_delay_and_hold_add_no_blocks() {
        let p = EnvelopeParams {
            attack: 0.001,
            decay: 0.1,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();
        // Zero delay: the attack moves in the very first block.
        let mut blocks_to_peak = 1;
        while env.advance(&c) < 1.0 {
            blocks_to_peak += 1;
        }
        assert_eq!(blocks_to_peak, blocks(p.attack));
        // Zero hold: the decay starts in the block right after the peak.
        assert!(env.advance(&c) < 1.0);
        assert_eq!(env.stage(), Stage::Decay);
    }

    #[test]
    fn release_during_delay_goes_idle() {
        let p = EnvelopeParams {
            delay: 1.0,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();
        env.advance(&c);
        env.gate_off();
        env.advance(&c);
        assert!(env.is_idle());
    }

    /// Level halfway through each stage for the given curve.
    fn midpoints(curve: f32) -> (f32, f32, f32) {
        let p = EnvelopeParams {
            attack: 0.2,
            decay: 0.2,
            sustain: 0.0,
            release: 0.2,
            attack_curve: curve,
            decay_curve: curve,
            release_curve: curve,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let half = blocks(0.1);
        let mut env = Envelope::new();
        env.gate_on();
        for _ in 0..half {
            env.advance(&c);
        }
        let attack = env.level();
        for _ in half..blocks(0.2) + half {
            env.advance(&c);
        }
        let decay = env.level();
        // A release from full level: hold the note at a full sustain.
        let mut env = Envelope::new();
        let full = EnvelopeCoefs::new(&EnvelopeParams { sustain: 1.0, ..p }, BLOCK_SECONDS);
        env.gate_on();
        for _ in 0..blocks(0.5) {
            env.advance(&full);
        }
        env.gate_off();
        for _ in 0..half {
            env.advance(&full);
        }
        (attack, decay, env.level())
    }

    #[test]
    fn curves_bend_every_stage() {
        // Linear: halfway is half.
        let (a, d, r) = midpoints(0.0);
        for x in [a, d, r] {
            assert!((x - 0.5).abs() < 0.02, "linear {a} {d} {r}");
        }
        // Exponential (convex): slow attack, fast decay and release.
        let (a, d, r) = midpoints(1.0);
        assert!(a < 0.1 && d < 0.1 && r < 0.1, "convex {a} {d} {r}");
        // Logarithmic (concave): fast attack, slow decay and release.
        let (a, d, r) = midpoints(-1.0);
        assert!(a > 0.9 && d > 0.9 && r > 0.9, "concave {a} {d} {r}");
    }

    #[test]
    fn default_decay_is_the_classic_60_db_exponential() {
        let p = EnvelopeParams {
            decay: 0.5,
            sustain: 0.0,
            attack: 0.001,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();
        env.advance(&c);
        // A quarter into the decay: e^(-ln 1000 / 4) ≈ 0.178.
        for _ in 0..blocks(0.125) {
            env.advance(&c);
        }
        assert!((env.level() - 0.178).abs() < 0.01, "{}", env.level());
    }

    #[test]
    fn looping_repeats_attack_and_decay_while_held() {
        let p = EnvelopeParams {
            delay: 0.05,
            attack: 0.01,
            decay: 0.04,
            sustain: 0.25,
            release: 0.02,
            looping: true,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();
        let levels: Vec<f32> = (0..blocks(1.0)).map(|_| env.advance(&c)).collect();
        // Silent through the delay, once.
        assert!(levels[..blocks(p.delay) - 1].iter().all(|&x| x == 0.0));
        let cycling = &levels[blocks(p.delay) + blocks(p.attack)..];
        // Between sustain and full level, never settling.
        assert!(
            cycling.iter().all(|&x| (p.sustain..=1.0).contains(&x)),
            "{cycling:?}"
        );
        let peaks = cycling
            .windows(2)
            .filter(|w| w[0] < 1.0 && w[1] == 1.0)
            .count();
        // One cycle per attack + decay (50 ms): at most 19 more peaks in the
        // remaining ~0.94 s; block rounding makes cycles a little longer.
        assert!((16..=19).contains(&peaks), "{peaks} peaks");

        // The release ends it as usual.
        env.gate_off();
        for _ in 0..blocks(p.release) {
            env.advance(&c);
        }
        assert!(env.is_idle());
    }

    #[test]
    fn looping_with_full_sustain_stays_at_full_level() {
        let p = EnvelopeParams {
            attack: 0.01,
            decay: 0.01,
            sustain: 1.0,
            looping: true,
            ..Default::default()
        };
        let c = EnvelopeCoefs::new(&p, BLOCK_SECONDS);
        let mut env = Envelope::new();
        env.gate_on();
        let levels: Vec<f32> = (0..blocks(0.5)).map(|_| env.advance(&c)).collect();
        assert!(levels[blocks(p.attack)..].iter().all(|&x| x == 1.0));
    }

    #[test]
    fn gate_off_while_idle_stays_idle() {
        let mut env = Envelope::new();
        env.gate_off();
        assert!(env.is_idle());
    }
}
