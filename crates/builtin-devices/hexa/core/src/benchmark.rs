//! Patches for benchmarks, shared by the desktop benchmark (criterion) and the
//! Pi's boot benchmark, from light to the worst case the engine allows.

use crate::chorus::ChorusParams;
use crate::delay::DelayParams;
use crate::distortion::{BitcrusherParams, DistortionKind, DistortionParams};
use crate::envelope::EnvelopeParams;
use crate::eq::EqParams;
use crate::filter::{FilterKind, FilterParams, FilterSlope};
use crate::lfo::{LfoParams, MAX_LFOS};
use crate::mod_envelope::MAX_ENVELOPES;
use crate::modulation::{ModRoute, ModSource, ModTarget};
use crate::operator::{KeyScaling, ModulationMode, OPERATORS, OperatorKind};
use crate::reverb::ReverbParams;
use crate::{Patch, presets};

/// A named benchmark patch.
pub struct Scenario {
    pub name: &'static str,
    pub build: fn() -> Patch,
}

/// From light to heavy.
pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "Init",
        build: init,
    },
    Scenario {
        name: "E-Piano",
        build: e_piano,
    },
    Scenario {
        name: "6 Ops",
        build: six_operators,
    },
    Scenario {
        name: "6 Ops+Filter",
        build: filtered,
    },
    Scenario {
        name: "Worst",
        build: worst_case,
    },
    Scenario {
        name: "E-Piano+Rev",
        build: e_piano_reverb,
    },
];

fn init() -> Patch {
    presets::preset(0).map(|p| p.patch).unwrap_or_default()
}

fn e_piano() -> Patch {
    presets::preset(1).map(|p| p.patch).unwrap_or_default()
}

/// All six operators sounding forever (five modulate the first).
pub fn six_operators() -> Patch {
    let mut patch = Patch::default();
    for (i, op) in patch.operators.iter_mut().enumerate() {
        op.level = 0.3;
        op.ratio = (i + 1) as f32;
        op.envelope = EnvelopeParams {
            sustain: 1.0,
            ..Default::default()
        };
        op.modulates = core::array::from_fn(|t| i > 0 && t == 0);
    }
    patch
}

/// Adds a route; the benchmark patches stay within the matrix.
fn route(patch: &mut Patch, source: ModSource, target: ModTarget, depth: f32, via: ModSource) {
    assert!(
        patch
            .modulation
            .push(ModRoute {
                source,
                target,
                depth,
                via
            })
            .is_some(),
        "matrix full"
    );
}

/// `six_operators` with all 8 LFOs in use: every operator's pitch, level and
/// feedback plus tune and volume, spread over the LFOs (20 routes), the
/// pitch routes scaled by velocity (via).
pub fn eight_lfos() -> Patch {
    let mut patch = six_operators();
    for i in 0..MAX_LFOS {
        patch.lfos.push(LfoParams {
            rate: 1.0 + i as f32,
            ..Default::default()
        });
    }
    let lfo = |n: usize| ModSource::Lfo(n % MAX_LFOS);
    for i in 0..OPERATORS {
        route(
            &mut patch,
            lfo(3 * i),
            ModTarget::OpPitch(i),
            0.1,
            ModSource::Velocity,
        );
        route(
            &mut patch,
            lfo(3 * i + 1),
            ModTarget::OpLevel(i),
            0.1,
            ModSource::None,
        );
        route(
            &mut patch,
            lfo(3 * i + 2),
            ModTarget::OpFeedback(i),
            0.1,
            ModSource::None,
        );
    }
    route(&mut patch, lfo(18), ModTarget::Tune, 0.1, ModSource::None);
    route(&mut patch, lfo(19), ModTarget::Volume, 0.1, ModSource::None);
    patch
}

/// `eight_lfos` plus all 8 envelopes: each sets an LFO's amount (the first
/// four) or rate (the others), and they take over every other operator's
/// level from the LFOs (28 routes).
pub fn eight_lfos_eight_envelopes() -> Patch {
    let mut patch = eight_lfos();
    for _ in 0..MAX_ENVELOPES {
        patch.envelopes.push(EnvelopeParams {
            sustain: 1.0,
            ..Default::default()
        });
    }
    for l in 0..MAX_LFOS {
        let target = if l < MAX_LFOS / 2 {
            ModTarget::LfoAmount(l)
        } else {
            ModTarget::LfoRate(l)
        };
        route(
            &mut patch,
            ModSource::Envelope(l % MAX_ENVELOPES),
            target,
            0.5,
            ModSource::None,
        );
    }
    for r in patch.modulation.as_mut_slice() {
        if let ModTarget::OpLevel(i) = r.target
            && i % 2 == 0
        {
            r.source = ModSource::Envelope(i % MAX_ENVELOPES);
        }
    }
    patch
}

/// A 24 dB low-pass with drive; `heavy_filter_routes` sweeps the cutoff by
/// an envelope and the drive by velocity.
fn heavy_filter() -> FilterParams {
    FilterParams {
        kind: FilterKind::LowPass,
        slope: FilterSlope::Db24,
        cutoff: 800.0,
        resonance: 0.5,
        drive: 0.5,
        keytrack: 0.5,
    }
}

fn heavy_filter_routes(patch: &mut Patch, envelope: ModSource) {
    route(patch, envelope, ModTarget::Cutoff(0), 36.0, ModSource::None);
    route(
        patch,
        ModSource::Velocity,
        ModTarget::Drive(0),
        0.3,
        ModSource::None,
    );
}

/// `six_operators` through `heavy_filter`.
pub fn filtered() -> Patch {
    let mut patch = six_operators();
    let envelope = ModSource::Envelope(
        patch
            .envelopes
            .push(EnvelopeParams {
                sustain: 0.5,
                ..Default::default()
            })
            .unwrap_or(0),
    );
    patch.filters[0] = heavy_filter();
    heavy_filter_routes(&mut patch, envelope);
    patch
}

/// Everything at once: `eight_lfos_eight_envelopes` with frequency, pulse
/// width, sync and ring modulation, feedback and a wavefolder on every
/// operator, op 1 a resonant low-pass operator (fed by op 2, its cutoff
/// moved by op 3: filter FM), through `heavy_filter` with every filter
/// parameter modulated, in stereo (every operator heard and panned, the
/// master balance moved by an LFO), keyboard scaling (level and rate) on
/// every operator, a full matrix (32 routes), plus all effects.
pub fn worst_case() -> Patch {
    let mut patch = eight_lfos_eight_envelopes();
    let modes = [
        ModulationMode::Phase,
        ModulationMode::Phase,
        ModulationMode::Frequency,
        ModulationMode::PulseWidth,
        ModulationMode::Sync,
        ModulationMode::Ring,
    ];
    for (i, (op, mode)) in patch.operators.iter_mut().zip(modes).enumerate() {
        op.mode = mode;
        op.feedback = 0.2;
        op.fold = 0.2;
        op.output = true;
        op.pan = if i % 2 == 0 { -0.5 } else { 0.5 };
        op.key_scaling = KeyScaling {
            left: 3.0,
            right: -3.0,
            rate: 0.5,
            ..KeyScaling::NONE
        };
    }
    patch.operators[0].kind = OperatorKind::LowPass;
    patch.operators[0].resonance = 0.5;
    route(
        &mut patch,
        ModSource::Lfo(7),
        ModTarget::Balance,
        0.5,
        ModSource::None,
    );
    patch.filters[0] = heavy_filter();
    heavy_filter_routes(&mut patch, ModSource::Envelope(0));
    route(
        &mut patch,
        ModSource::Lfo(3),
        ModTarget::Resonance(0),
        0.2,
        ModSource::Velocity,
    );
    debug_assert_eq!(patch.modulation.len(), crate::MAX_ROUTES);
    patch.effects.reverb = reverb();
    patch.effects.distortion = DistortionParams {
        mix: 0.5,
        kind: DistortionKind::Fold,
        ..DistortionParams::DEFAULT
    };
    patch.effects.bitcrusher = BitcrusherParams {
        mix: 0.3,
        ..BitcrusherParams::DEFAULT
    };
    patch.effects.eq = EqParams {
        low_gain: 6.0,
        mid_gain: -6.0,
        high_gain: 6.0,
        ..EqParams::DEFAULT
    };
    patch.effects.chorus = ChorusParams {
        mix: 0.5,
        ..ChorusParams::DEFAULT
    };
    patch.effects.delay = DelayParams {
        mix: 0.3,
        ping_pong: true,
        ..DelayParams::DEFAULT
    };
    patch
}

/// The reverb as the master effects use it: on, long tail.
fn reverb() -> ReverbParams {
    ReverbParams {
        mix: 0.3,
        time: 4.0,
        ..ReverbParams::DEFAULT
    }
}

/// `e_piano` with the reverb, which costs the same for any number of voices.
fn e_piano_reverb() -> Patch {
    let mut patch = e_piano();
    patch.effects.reverb = reverb();
    patch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_are_valid_and_sound() {
        for scenario in SCENARIOS {
            let patch = (scenario.build)();
            assert_eq!(patch.sanitized(), patch, "{}", scenario.name);
            let mut engine = crate::Synth::<1>::boxed(48_000.0, patch);
            for v in 0..16u8 {
                engine.note_on(36 + 3 * v, 100);
            }
            let mut l = [0.0; 4800];
            let mut r = [0.0; 4800];
            engine.render(&mut l, &mut r);
            assert_eq!(engine.active_voices(), 16, "{}", scenario.name);
            assert!(l.iter().all(|x| x.is_finite()), "{}", scenario.name);
        }
    }
}
