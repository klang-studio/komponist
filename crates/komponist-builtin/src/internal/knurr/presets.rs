//! The factory presets of knurr (the ten of the proposal, variant B). A preset is the parameters
//! that differ from the defaults, by model id; [`Preset::values`] gives all of them by wire id, so
//! a preset keeps working when a parameter is added later (it gets its default).
//!
//! Internal devices have no preset menu yet (the device card's preset button is pisynth's), so
//! these are data and tests for now; [`state`] is the device state a menu would store.

use komponist_model::params::va_curve::{dst, src};

use crate::internal::Internal;

/// One preset.
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// Name in the menu.
    pub name: &'static str,
    /// Folder in the menu.
    pub category: &'static str,
    /// The parameters that differ from the defaults: model id, plain value.
    pub changes: &'static [(&'static str, f64)],
}

/// The presets, in menu order.
pub fn presets() -> &'static [Preset] {
    const LFO1: usize = src::LFO1;
    const ENV3: usize = src::ENV3;
    const RANDOM: usize = src::RANDOM;
    // Wave: 0 sine, 1 triangle, 2 saw, 3 pulse. Filter: 0 LP 24, 1 LP 12, 2 BP 12.
    static LIST: [Preset; 10] = [
        Preset {
            name: "Fette Säge",
            category: "Bass",
            changes: &[
                ("osc_b_oct", -1.0),
                ("cutoff", 900.0),
                ("env_amount", 0.4),
                ("sub_level", 0.3),
            ],
        },
        Preset {
            name: "Sub-Brumm",
            category: "Bass",
            changes: &[
                ("osc_a_wave", 0.0),
                ("osc_b_wave", 0.0),
                ("osc_mix", 0.3),
                ("osc_b_fine", 0.0),
                ("sub_level", 0.6),
                ("cutoff", 700.0),
                ("resonance", 0.0),
                ("drive", 0.1),
                ("env_amount", 0.1),
                ("mod_a", 0.001),
                ("mod_d", 0.08),
                ("mod_s", 0.0),
                ("mod1_src", ENV3 as f64),
                ("mod1_dst", dst::PITCH as f64),
                ("mod1_amt", 0.12),
                ("amp_d", 0.5),
                ("amp_s", 0.8),
            ],
        },
        Preset {
            name: "Acid-Zupf",
            category: "Bass",
            changes: &[
                ("mode", 1.0),
                ("glide", 0.06),
                ("osc_a_wave", 2.0),
                ("osc_mix", 0.0),
                ("sub_level", 0.0),
                ("cutoff", 350.0),
                ("resonance", 0.75),
                ("drive", 0.4),
                ("env_amount", 0.7),
                ("filt_d", 0.2),
                ("filt_s", 0.0),
                ("vel_filter", 0.7),
                ("key_amount", 0.3),
                ("amp_d", 0.25),
                ("amp_s", 0.5),
                ("amp_r", 0.08),
            ],
        },
        Preset {
            name: "Mono-Lead",
            category: "Lead",
            changes: &[
                ("mode", 2.0),
                ("glide", 0.04),
                ("osc_a_wave", 3.0),
                ("osc_b_wave", 2.0),
                ("osc_b_fine", 7.0),
                ("sub_level", 0.0),
                ("cutoff", 3000.0),
                ("resonance", 0.25),
                ("env_amount", 0.2),
                ("lfo1_rate", 5.5),
                ("lfo1_fade", 0.4),
                ("wheel_vib", 60.0),
                ("amp_a", 0.005),
                ("amp_s", 1.0),
                ("amp_r", 0.15),
            ],
        },
        Preset {
            name: "Breiter Pad",
            category: "Pad",
            changes: &[
                ("osc_a_wave", 3.0),
                ("osc_b_wave", 2.0),
                ("osc_b_fine", 0.0),
                ("unison", 5.0),
                ("detune", 25.0),
                ("spread", 0.8),
                ("analog", 0.5),
                ("sub_level", 0.0),
                ("cutoff", 1800.0),
                ("resonance", 0.15),
                ("env_amount", 0.15),
                ("filt_a", 0.8),
                ("amp_a", 0.6),
                ("amp_d", 1.0),
                ("amp_s", 0.8),
                ("amp_r", 1.4),
                ("lfo2_rate", 0.2),
                ("lfo2_shape", 1.0),
                ("mod1_src", src::LFO2 as f64),
                ("mod1_dst", dst::PW_A as f64),
                ("mod1_amt", 0.5),
            ],
        },
        Preset {
            name: "Blechstab",
            category: "Brass",
            changes: &[
                ("osc_a_wave", 2.0),
                ("osc_b_wave", 2.0),
                ("osc_b_fine", 7.0),
                ("hp_freq", 120.0),
                ("cutoff", 700.0),
                ("resonance", 0.2),
                ("env_amount", 0.6),
                ("filt_a", 0.08),
                ("filt_d", 0.3),
                ("filt_s", 0.6),
                ("vel_filter", 0.5),
                ("sub_level", 0.0),
                ("amp_a", 0.02),
                ("amp_s", 0.9),
                ("amp_r", 0.12),
            ],
        },
        Preset {
            name: "Wobble",
            category: "Bass",
            changes: &[
                ("mode", 1.0),
                ("osc_b_oct", -1.0),
                ("sub_level", 0.3),
                ("cutoff", 500.0),
                ("resonance", 0.55),
                ("env_amount", 0.0),
                ("lfo1_shape", 1.0),
                ("lfo1_sync", 1.0),
                ("lfo1_div", 5.0),
                ("lfo1_key", 0.0),
                ("mod1_src", LFO1 as f64),
                ("mod1_dst", dst::CUTOFF as f64),
                ("mod1_amt", 0.7),
            ],
        },
        Preset {
            name: "Zufalls-Zupf",
            category: "Pluck",
            changes: &[
                ("osc_b_wave", 3.0),
                ("cutoff", 800.0),
                ("env_amount", 0.5),
                ("filt_d", 0.25),
                ("filt_s", 0.0),
                ("sub_level", 0.0),
                ("amp_d", 0.35),
                ("amp_s", 0.0),
                ("amp_r", 0.2),
                ("mod1_src", RANDOM as f64),
                ("mod1_dst", dst::CUTOFF as f64),
                ("mod1_amt", 0.35),
                ("mod2_src", RANDOM as f64),
                ("mod2_dst", dst::PW_B as f64),
                ("mod2_amt", 0.4),
            ],
        },
        Preset {
            name: "Kick-Bass",
            category: "Bass",
            changes: &[
                ("osc_a_wave", 0.0),
                ("osc_mix", 0.0),
                ("sub_level", 0.3),
                ("cutoff", 900.0),
                ("resonance", 0.0),
                ("env_amount", 0.0),
                ("mod_a", 0.001),
                ("mod_d", 0.09),
                ("mod_s", 0.0),
                ("mod1_src", ENV3 as f64),
                ("mod1_dst", dst::PITCH as f64),
                ("mod1_amt", 0.5),
                ("amp_a", 0.001),
                ("amp_d", 0.35),
                ("amp_s", 0.0),
                ("amp_r", 0.1),
            ],
        },
        Preset {
            name: "Rauschwind",
            category: "FX",
            changes: &[
                ("osc_a_wave", 0.0),
                ("osc_b_wave", 0.0),
                ("osc_b_oct", -3.0),
                ("sub_level", 0.0),
                ("noise_level", 0.9),
                ("filter_type", 2.0),
                ("cutoff", 1500.0),
                ("resonance", 0.6),
                ("env_amount", 0.0),
                ("lfo2_rate", 0.15),
                ("lfo2_shape", 5.0),
                ("mod1_src", src::LFO2 as f64),
                ("mod1_dst", dst::CUTOFF as f64),
                ("mod1_amt", 0.4),
                ("at_cut", 0.5),
                ("amp_a", 0.5),
                ("amp_s", 1.0),
                ("amp_r", 1.2),
            ],
        },
    ];
    &LIST
}

impl Preset {
    /// All parameters by wire id: the defaults with this preset's changes.
    pub fn values(&self) -> Vec<f64> {
        let kind = Internal::Knurr;
        let mut values = kind.defaults();
        for (key, value) in self.changes {
            if let Some(wire) = kind.wire(&komponist_model::params::ParamId::new(*key)) {
                values[wire as usize] = kind.params()[wire as usize].snap(*value);
            }
        }
        values
    }
}

/// The device state (as the project saves it) of preset `index`.
pub fn state(index: usize) -> Option<Vec<u8>> {
    let preset = presets().get(index)?;
    Some(Internal::Knurr.encode_state(&preset.values()))
}
