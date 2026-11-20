//! The envelopes: attack, decay, sustain, release, stepped once per control interval.
//!
//! The attack is a straight line, so its time is exact; decay and release are exponential and
//! their time is the time to fall 40 dB (to 1 % of the distance), the way analog envelopes read.
//! A new gate starts from the current level, so a retriggered or stolen voice does not click.

use komponist_device_core::dsp::CONTROL;

/// Stage of an envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Not running, level 0.
    Idle,
    /// Rising to 1.
    Attack,
    /// Falling to the sustain level.
    Decay,
    /// Holding the sustain level.
    Sustain,
    /// Falling to 0.
    Release,
}

/// What an envelope needs per control interval, from the times in seconds.
#[derive(Clone, Copy, Debug)]
pub struct Coefs {
    /// Level gained per interval in the attack.
    pub attack: f32,
    /// Pole per interval of the decay.
    pub decay: f32,
    /// Sustain level.
    pub sustain: f32,
    /// Pole per interval of the release.
    pub release: f32,
}

/// `ln(0.01)`: an exponential segment that has covered 99 % of its distance.
const LN_HUNDREDTH: f32 = -4.605_17;

/// The pole per interval of an exponential segment that covers 99 % in `seconds`.
pub fn pole(seconds: f32, rate: f32) -> f32 {
    (LN_HUNDREDTH * CONTROL as f32 / (seconds.max(1e-4) * rate)).exp()
}

impl Coefs {
    /// From times in seconds (`scale` stretches all three, the voice's tolerance).
    pub fn new(a: f32, d: f32, s: f32, r: f32, rate: f32, scale: [f32; 3]) -> Coefs {
        Coefs {
            attack: (CONTROL as f32 / (a * scale[0] * rate)).min(1.0),
            decay: pole(d * scale[1], rate),
            sustain: s.clamp(0.0, 1.0),
            release: pole(r * scale[2], rate),
        }
    }
}

/// Level below which a released envelope is over (−80 dB).
const SILENT: f32 = 1e-4;

/// One envelope.
#[derive(Clone, Copy, Debug)]
pub struct Adsr {
    /// The stage.
    pub stage: Stage,
    /// The level, 0 … 1.
    pub level: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Adsr {
            stage: Stage::Idle,
            level: 0.0,
        }
    }
}

impl Adsr {
    /// Key down: the attack starts from the current level.
    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    /// Key up.
    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// Back to idle at level 0.
    pub fn reset(&mut self) {
        *self = Adsr::default();
    }

    /// Whether the release has run out.
    pub fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    /// One control interval; returns the level. `release` overrides the release pole (a voice
    /// that is silenced quickly).
    #[inline]
    pub fn step(&mut self, c: &Coefs, release: Option<f32>) -> f32 {
        match self.stage {
            Stage::Idle => self.level = 0.0,
            Stage::Attack => {
                self.level += c.attack;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level = c.sustain + (self.level - c.sustain) * c.decay;
                if (self.level - c.sustain).abs() < 1e-4 {
                    self.level = c.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = c.sustain,
            Stage::Release => {
                self.level *= release.unwrap_or(c.release);
                if self.level < SILENT {
                    self.reset();
                }
            }
        }
        self.level
    }
}
