//! Modulation envelopes: DAHDSR envelopes (see [`crate::envelope`]) that are
//! not tied to an operator and serve as modulation sources, 0..=1.
//!
//! They start on every note-on and release on note-off like the operator
//! envelopes, but never keep a voice alive. Envelopes that nothing uses are
//! gated but not advanced.

use crate::MAX_VOICES;
use crate::envelope::{Envelope, EnvelopeCoefs, EnvelopeParams};

/// Largest number of modulation envelopes per patch.
pub const MAX_ENVELOPES: usize = 8;

/// The modulation envelopes of a patch.
pub type EnvelopeBank = crate::bank::Bank<EnvelopeParams, MAX_ENVELOPES>;

/// Runtime state of all modulation envelopes across all voices.
pub(crate) struct ModEnvelopes {
    envelopes: [[Envelope; MAX_VOICES]; MAX_ENVELOPES],
}

impl ModEnvelopes {
    pub(crate) fn new() -> Self {
        Self {
            envelopes: [[Envelope::new(); MAX_VOICES]; MAX_ENVELOPES],
        }
    }

    /// Envelope `index` of `voice`.
    pub(crate) fn get(&self, index: usize, voice: usize) -> &Envelope {
        &self.envelopes[index][voice]
    }

    /// Starts the envelopes of `voice`; `fresh` (a new or stolen voice)
    /// restarts them from 0, otherwise the attack continues from the
    /// current level, like the operator envelopes.
    pub(crate) fn note_on(&mut self, voice: usize, fresh: bool) {
        for envelope in &mut self.envelopes {
            if fresh {
                envelope[voice].reset();
            }
            envelope[voice].gate_on();
        }
    }

    /// Cuts the envelopes of `voice` off at once.
    pub(crate) fn reset(&mut self, voice: usize) {
        for envelope in &mut self.envelopes {
            envelope[voice].reset();
        }
    }

    pub(crate) fn note_off(&mut self, voice: usize) {
        for envelope in &mut self.envelopes {
            envelope[voice].gate_off();
        }
    }

    /// Advances the envelopes in `used` (bit mask) of `voice` by one block.
    /// Returns their levels at the start and the end of the block; 0 for
    /// unused ones.
    pub(crate) fn advance_voice(
        &mut self,
        voice: usize,
        coefs: &[EnvelopeCoefs; MAX_ENVELOPES],
        used: u8,
    ) -> ([f32; MAX_ENVELOPES], [f32; MAX_ENVELOPES]) {
        let mut start = [0.0; MAX_ENVELOPES];
        let mut end = [0.0; MAX_ENVELOPES];
        for (e, envelope) in self.envelopes.iter_mut().enumerate() {
            if used & (1 << e) != 0 {
                start[e] = envelope[voice].level();
                end[e] = envelope[voice].advance(&coefs[e]);
            }
        }
        (start, end)
    }
}

/// Per-block coefficients for the envelopes of `bank` in `used` (bit mask);
/// the others get a placeholder, saving the work.
pub(crate) fn coefs(
    bank: &EnvelopeBank,
    used: u8,
    block_seconds: f32,
) -> [EnvelopeCoefs; MAX_ENVELOPES] {
    core::array::from_fn(|e| match bank.as_slice().get(e) {
        Some(params) if used & (1 << e) != 0 => EnvelopeCoefs::new(params, block_seconds),
        _ => EnvelopeCoefs::UNUSED,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: f32 = 32.0 / 48_000.0;

    #[test]
    fn rises_holds_and_releases_only_when_used() {
        let params = EnvelopeParams {
            attack: 0.01,
            decay: 0.1,
            sustain: 0.5,
            release: 0.05,
            ..Default::default()
        };
        let bank = EnvelopeBank::from_slice(&[params, params]);
        let coefs = coefs(&bank, 0b01, BLOCK);
        let mut envs = ModEnvelopes::new();
        envs.note_on(0, true);
        let mut last = ([0.0; MAX_ENVELOPES], [0.0; MAX_ENVELOPES]);
        for _ in 0..(1.0 / BLOCK) as usize {
            last = envs.advance_voice(0, &coefs, 0b01);
        }
        assert!((last.1[0] - 0.5).abs() < 1e-3, "sustain {}", last.1[0]);
        assert_eq!(last.1[1], 0.0, "unused envelope advanced");
        envs.note_off(0);
        for _ in 0..(0.5 / BLOCK) as usize {
            last = envs.advance_voice(0, &coefs, 0b01);
        }
        assert!(last.1[0] < 1e-3, "released to {}", last.1[0]);
    }
}
