//! The 8 macros of hexa's device card (S05) and how they map to [`Patch`] fields.
//!
//! A macro is a function of the patch in both directions: [`Macro::get`] reads its value from a
//! patch, [`Macro::set`] writes a value into a patch. The processor applies `set` to its running
//! patch when an `EventKind::Param { id, value }` with the macro's [`Macro::id`] arrives; the app
//! applies the same `set` to the device state it keeps in the document, so both stay equal.
//!
//! Operators are numbered 1–6 as in hexa's UI (array index + 1). "Carriers" are the heard
//! operators (`output = true`); the "first carrier" is the lowest-numbered one (operator 1 in every
//! factory preset but "Wind"), or operator 1 if none is heard.
//!
//! | Id | Name | Plain range | Patch fields written by `set` |
//! |---:|---|---|---|
//! | 0 | Algorithmus | 0 ..= 32, integer | 1–32: routing of all operators per [`crate::algorithms`] (`modulates`, `output`, new routes get depth 1, the strongest feedback moves to the algorithm's feedback operator). 0 ("Frei", free routing): nothing. `get` gives the matching algorithm or 0. |
//! | 1 | Ratio 2 | 0 ..= 32 (`MAX_RATIO`), 64 steps of 0.5 | `operators[1].ratio`, and `operators[1].pitch_mode = Ratio` |
//! | 2 | Index | 0 ..= 1 | `operators[1].level` (operator 2: modulation depth, 1 = 16 rad; loudness where operator 2 is a carrier) |
//! | 3 | Feedback | 0 ..= 1 | `feedback` (1 = π rad) of the feedback operator: the one with the most feedback (lowest number on a tie), other operators with feedback scaled along; without any feedback the algorithm's feedback operator, or operator 2 in free routing |
//! | 4 | Attack | 0 ..= 1, knob position | `envelope.attack` of the carriers |
//! | 5 | Decay | 0 ..= 1, knob position | `envelope.decay` of the carriers |
//! | 6 | Sustain | 0 ..= 1 | `envelope.sustain` of the carriers |
//! | 7 | Release | 0 ..= 1, knob position | `envelope.release` of the carriers |
//!
//! Envelope macros (Attack, Decay, Sustain, Release) act on the carriers only (the amplitude
//! envelope; modulator envelopes shape the timbre and stay). The first carrier gets the value,
//! the other carriers keep their proportion to it: their stage is scaled by the same factor
//! (clamped to the valid range). If the first carrier's stage is 0, every carrier gets the value.
//! `get` reads the first carrier. Feedback follows the same rule among the operators that have
//! feedback, with the strongest one in the role of the first carrier.
//!
//! Algorithms that differ only in the feedback operator (1/2, 3/4, 5/6, …) have the same routing.
//! While no operator has feedback a patch matches both, and `get` gives the lower number (as
//! hexa's display does), so after `set(17)` on a patch without feedback `get` gives 16.
//!
//! Time macros use a knob position `v` in 0 ..= 1 as plain value, because a linear 0–30 s knob
//! is unusable: time = [`TIME_MIN`] + ([`TIME_MAX`] − [`TIME_MIN`]) · v⁴ (1 ms … 30 s; 50 ms at
//! v ≈ 0.20, 300 ms at 0.32, 1 s at 0.43, 10 s at 0.76). [`Macro::text`] shows the time.
//!
//! Values outside the range are clamped; non-finite values are ignored. Every `set` keeps a
//! sanitized patch sanitized ([`Patch::sanitized`]), allocates nothing and is RT-safe. `set`
//! is not invertible in general (Algorithmus rewrites the routing, proportions collapse at 0):
//! to restore an earlier patch (undo), load its state.

use hexa_core::{EnvelopeParams, MAX_RATIO, MAX_TIME, OPERATORS, Patch, PitchMode};
use komponist_i18n::Lang;
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};

use crate::algorithms::{self, ALGORITHMS};

/// Shortest time of the time macros, seconds (hexa's shortest stage time).
pub const TIME_MIN: f64 = 0.001;
/// Longest time of the time macros, seconds (`hexa_core::MAX_TIME`).
pub const TIME_MAX: f64 = MAX_TIME as f64;

/// One of hexa's 8 macros. The discriminant is the stable parameter id ([`Macro::id`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Macro {
    /// "Algorithmus": fixed algorithm 1–32, 0 = free routing.
    Algorithm = 0,
    /// "Ratio 2": frequency ratio of operator 2.
    Ratio2 = 1,
    /// "Index": level of operator 2.
    Index = 2,
    /// "Feedback": self-modulation of the feedback operator.
    Feedback = 3,
    /// "Attack": attack time of the carriers.
    Attack = 4,
    /// "Decay": decay time of the carriers.
    Decay = 5,
    /// "Sustain": sustain level of the carriers.
    Sustain = 6,
    /// "Release": release time of the carriers.
    Release = 7,
}

impl Macro {
    /// All macros in device-card order (= id order).
    pub const ALL: [Macro; 8] = [
        Macro::Algorithm,
        Macro::Ratio2,
        Macro::Index,
        Macro::Feedback,
        Macro::Attack,
        Macro::Decay,
        Macro::Sustain,
        Macro::Release,
    ];

    /// Stable parameter id (`EventKind::Param::id`, `Command::SetParam::id`), 0 ..= 7.
    pub fn id(self) -> u32 {
        self as u32
    }

    /// The macro with parameter id `id`.
    pub fn from_id(id: u32) -> Option<Macro> {
        Macro::ALL.get(id as usize).copied()
    }

    /// Name on the device card in `t` (the German one is S05's, verbatim from the prototype's
    /// catalog).
    pub fn name_in(self, t: Lang) -> &'static str {
        match self {
            Macro::Algorithm => t.param_hexa_algorithm(),
            Macro::Ratio2 => t.param_hexa_ratio2(),
            Macro::Index => t.param_hexa_index(),
            Macro::Feedback => t.param_feedback(),
            Macro::Attack => t.param_attack(),
            Macro::Decay => t.param_decay(),
            Macro::Sustain => t.param_sustain(),
            Macro::Release => t.param_release(),
        }
    }

    /// The German name on the device card (S05, verbatim from the prototype's catalog); views
    /// use [`Macro::name_in`].
    pub fn name(self) -> &'static str {
        match self {
            Macro::Algorithm => "Algorithmus",
            Macro::Ratio2 => "Ratio 2",
            Macro::Index => "Index",
            Macro::Feedback => "Feedback",
            Macro::Attack => "Attack",
            Macro::Decay => "Decay",
            Macro::Sustain => "Sustain",
            Macro::Release => "Release",
        }
    }

    /// Stable key of the macro in the parameter model ([`ParamInfo::id`], saved in projects):
    /// `algorithm`, `ratio2`, `index`, `feedback`, `attack`, `decay`, `sustain`, `release`.
    pub fn key(self) -> &'static str {
        match self {
            Macro::Algorithm => "algorithm",
            Macro::Ratio2 => "ratio2",
            Macro::Index => "index",
            Macro::Feedback => "feedback",
            Macro::Attack => "attack",
            Macro::Decay => "decay",
            Macro::Sustain => "sustain",
            Macro::Release => "release",
        }
    }

    /// The macro as a [`ParamInfo`] of the parameter model. The plain value stays the macro's
    /// own (algorithm number, ratio, knob position 0 ..= 1 for the time macros: `Lin`, the
    /// knob position already is the curve) and the text is [`Macro::text`].
    pub fn param_info(self) -> ParamInfo {
        self.param_info_in(Lang::De)
    }

    /// [`Macro::param_info`] with the name and the value texts of `t`.
    pub fn param_info_in(self, t: Lang) -> ParamInfo {
        let (kind, unit): (ParamKind, Unit) = match self {
            Macro::Algorithm => (
                ParamKind::Int {
                    min: 0,
                    max: ALGORITHMS.len() as i32,
                },
                Unit::None,
            ),
            Macro::Ratio2 => (
                ParamKind::Float {
                    min: self.min(),
                    max: self.max(),
                    scale: Scale::Lin,
                    steps: self.step_count(),
                },
                Unit::None,
            ),
            Macro::Index | Macro::Feedback | Macro::Sustain => {
                (ParamKind::lin(0.0, 1.0), Unit::Percent)
            }
            Macro::Attack | Macro::Decay | Macro::Release => (ParamKind::lin(0.0, 1.0), Unit::S),
        };
        komponist_device_core::macro_param(
            ParamInfo::new(self.key(), self.name_in(t), kind, self.default_value()).unit(unit),
            t,
            move |v| self.text_in(t, v),
            self != Macro::Algorithm,
        )
    }

    /// Lowest plain value.
    pub fn min(self) -> f64 {
        0.0
    }

    /// Highest plain value.
    pub fn max(self) -> f64 {
        match self {
            Macro::Algorithm => ALGORITHMS.len() as f64,
            Macro::Ratio2 => f64::from(MAX_RATIO),
            _ => 1.0,
        }
    }

    /// Steps between [`Macro::min`] and [`Macro::max`] for stepped macros, 0 = continuous
    /// (as `komponist_ipc::control::ParamInfo::step_count`).
    pub fn step_count(self) -> u32 {
        match self {
            Macro::Algorithm => ALGORITHMS.len() as u32,
            Macro::Ratio2 => 2 * MAX_RATIO as u32,
            _ => 0,
        }
    }

    /// Value in the "Init" patch ([`Patch::default`]), for resetting a knob.
    pub fn default_value(self) -> f64 {
        self.get(&Patch::default())
    }

    /// The macro's value in `patch`.
    pub fn get(self, patch: &Patch) -> f64 {
        let first = first_carrier(patch);
        let envelope = &patch.operators[first].envelope;
        match self {
            Macro::Algorithm => algorithms::detect(patch).map_or(0.0, |i| (i + 1) as f64),
            Macro::Ratio2 => f64::from(patch.operators[1].ratio),
            Macro::Index => f64::from(patch.operators[1].level),
            Macro::Feedback => f64::from(patch.operators[feedback_operator(patch)].feedback),
            Macro::Attack => time_to_knob(envelope.attack),
            Macro::Decay => time_to_knob(envelope.decay),
            Macro::Sustain => f64::from(envelope.sustain),
            Macro::Release => time_to_knob(envelope.release),
        }
    }

    /// Writes `value` (plain, clamped to the range) into `patch`; see the module docs for the
    /// fields. Non-finite values are ignored. RT-safe.
    pub fn set(self, patch: &mut Patch, value: f64) {
        if !value.is_finite() {
            return;
        }
        let value = value.clamp(self.min(), self.max());
        match self {
            Macro::Algorithm => {
                let number = value.round() as usize;
                if number > 0 {
                    algorithms::apply(patch, number - 1);
                }
            }
            Macro::Ratio2 => {
                let op = &mut patch.operators[1];
                op.ratio = value as f32;
                op.pitch_mode = PitchMode::Ratio;
            }
            Macro::Index => patch.operators[1].level = value as f32,
            Macro::Feedback => {
                let target = feedback_operator(patch);
                let old = patch.operators[target].feedback;
                let value = value as f32;
                for (i, op) in patch.operators.iter_mut().enumerate() {
                    if i == target {
                        op.feedback = value;
                    } else if old > 0.0 && op.feedback > 0.0 {
                        op.feedback = (op.feedback * (value / old)).clamp(0.0, 1.0);
                    }
                }
            }
            Macro::Attack => set_carriers(patch, knob_to_time(value), MAX_TIME, |e| &mut e.attack),
            Macro::Decay => set_carriers(patch, knob_to_time(value), MAX_TIME, |e| &mut e.decay),
            Macro::Sustain => set_carriers(patch, value as f32, 1.0, |e| &mut e.sustain),
            Macro::Release => {
                set_carriers(patch, knob_to_time(value), MAX_TIME, |e| &mut e.release)
            }
        }
    }

    /// Display text of a plain value: "Frei" or "1"…"32" (Algorithmus), "3.50" (Ratio 2),
    /// "35 %" (Index, Feedback, Sustain), "12 ms" / "1.20 s" / "12.0 s" (times). Not RT-safe.
    pub fn text(self, value: f64) -> String {
        self.text_in(Lang::De, value)
    }

    /// [`Macro::text`] in `t`: "Free" and "35%" in English.
    pub fn text_in(self, t: Lang, value: f64) -> String {
        let value = if value.is_finite() {
            value.clamp(self.min(), self.max())
        } else {
            self.min()
        };
        match self {
            Macro::Algorithm => match value.round() as u32 {
                0 => t.value_free().to_owned(),
                n => n.to_string(),
            },
            Macro::Ratio2 => format!("{value:.2}"),
            Macro::Index | Macro::Feedback | Macro::Sustain => {
                format!("{}{}", (value * 100.0).round(), t.value_percent())
            }
            Macro::Attack | Macro::Decay | Macro::Release => {
                let seconds = f64::from(knob_to_time(value));
                if seconds < 1.0 {
                    format!("{:.0} ms", seconds * 1000.0)
                } else if seconds < 10.0 {
                    format!("{seconds:.2} s")
                } else {
                    format!("{seconds:.1} s")
                }
            }
        }
    }
}

/// Index of the lowest-numbered heard operator, or 0 if none is heard.
fn first_carrier(patch: &Patch) -> usize {
    patch.operators.iter().position(|o| o.output).unwrap_or(0)
}

/// Index of the operator the Feedback macro acts on (see the module docs).
fn feedback_operator(patch: &Patch) -> usize {
    let mut strongest = None;
    let mut most = 0.0;
    for (i, op) in patch.operators.iter().enumerate() {
        if op.feedback > most {
            most = op.feedback;
            strongest = Some(i);
        }
    }
    strongest.unwrap_or_else(|| algorithms::detect(patch).map_or(1, |a| ALGORITHMS[a].feedback))
}

/// Sets the envelope stage selected by `stage` of the carriers: the first carrier gets `value`,
/// the others are scaled by the same factor, all clamped to `0 ..= max`.
fn set_carriers(
    patch: &mut Patch,
    value: f32,
    max: f32,
    stage: fn(&mut EnvelopeParams) -> &mut f32,
) {
    let first = first_carrier(patch);
    let old = *stage(&mut patch.operators[first].envelope);
    for (i, op) in patch.operators.iter_mut().enumerate() {
        if i != first && !op.output {
            continue;
        }
        let x = stage(&mut op.envelope);
        *x = if i == first || old <= 0.0 {
            value
        } else {
            (*x * (value / old)).clamp(0.0, max)
        };
    }
    debug_assert!(OPERATORS == patch.operators.len());
}

/// Seconds for a time-macro knob position `v` in 0 ..= 1.
fn knob_to_time(v: f64) -> f32 {
    (TIME_MIN + (TIME_MAX - TIME_MIN) * v.clamp(0.0, 1.0).powi(4)) as f32
}

/// Knob position for `seconds` (inverse of [`knob_to_time`], clamped to 0 ..= 1). Measured from
/// `TIME_MIN` as stored in a patch (`f32`), so the shortest time reads back as exactly 0.
fn time_to_knob(seconds: f32) -> f64 {
    let min = f64::from(TIME_MIN as f32);
    let x = (f64::from(seconds) - min) / (TIME_MAX - min);
    x.clamp(0.0, 1.0).powf(0.25)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::{PRESET_COUNT, preset};

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn ids_are_stable_and_named_as_in_the_design() {
        let names: Vec<_> = Macro::ALL.iter().map(|m| (m.id(), m.name())).collect();
        assert_eq!(
            names,
            [
                (0, "Algorithmus"),
                (1, "Ratio 2"),
                (2, "Index"),
                (3, "Feedback"),
                (4, "Attack"),
                (5, "Decay"),
                (6, "Sustain"),
                (7, "Release"),
            ]
        );
        assert!(
            Macro::ALL
                .iter()
                .all(|&m| Macro::from_id(m.id()) == Some(m))
        );
        assert_eq!(Macro::from_id(8), None);
    }

    #[test]
    fn defaults_are_the_init_patch() {
        let defaults: Vec<_> = Macro::ALL.iter().map(|m| m.default_value()).collect();
        assert_eq!(&defaults[..4], [0.0, 1.0, 0.125, 0.0]);
        assert!(close(f64::from(knob_to_time(defaults[4])), 0.005));
        assert!(close(f64::from(knob_to_time(defaults[5])), 0.3));
        assert!(close(defaults[6], 0.7));
        assert!(close(f64::from(knob_to_time(defaults[7])), 0.3));
    }

    #[test]
    fn time_knob_covers_1_ms_to_30_s() {
        assert_eq!(knob_to_time(0.0), 0.001);
        assert_eq!(knob_to_time(1.0), 30.0);
        for t in [0.001, 0.05, 0.3, 1.0, 10.0, 30.0] {
            assert!(
                (f64::from(knob_to_time(time_to_knob(t))) - f64::from(t)).abs()
                    < 1e-5 * f64::from(t).max(1.0)
            );
        }
        assert!((time_to_knob(1.0) - 0.43).abs() < 0.01);
        assert_eq!(time_to_knob(0.0), 0.0);
    }

    /// `get` returns what `set` wrote, for every macro on every factory preset, and the patch
    /// stays sanitized.
    #[test]
    fn set_then_get_round_trips_on_every_preset() {
        let values: [&[f64]; 8] = [
            &[1.0, 5.0, 17.0, 32.0],
            &[0.5, 1.0, 3.5, 32.0],
            &[0.0, 0.3, 1.0],
            &[0.0, 0.4, 1.0],
            &[0.0, 0.2, 0.9],
            &[0.1, 0.5, 1.0],
            &[0.0, 0.6, 1.0],
            &[0.05, 0.4, 0.8],
        ];
        for i in 0..PRESET_COUNT {
            let patch = preset(i).unwrap().patch;
            for (m, values) in Macro::ALL.iter().zip(values) {
                for &v in values {
                    let mut p = patch;
                    m.set(&mut p, v);
                    let got = m.get(&p);
                    let twin = *m == Macro::Algorithm
                        && p.operators.iter().all(|o| o.feedback == 0.0)
                        && ALGORITHMS[got as usize - 1].modulates
                            == ALGORITHMS[v as usize - 1].modulates;
                    assert!(close(got, v) || twin, "{i} {m:?} {v}: {got}");
                    assert_eq!(p.sanitized(), p, "preset {i} {m:?} {v}");
                }
            }
        }
    }

    #[test]
    fn twin_algorithms_read_back_by_their_feedback() {
        let mut p = Patch::default();
        Macro::Algorithm.set(&mut p, 17.0);
        assert_eq!(Macro::Algorithm.get(&p), 16.0);
        Macro::Feedback.set(&mut p, 0.3);
        // 16 has its feedback on operator 6: Feedback now lands there.
        assert_eq!(p.operators[5].feedback, 0.3);
        Macro::Algorithm.set(&mut p, 17.0);
        assert_eq!(Macro::Algorithm.get(&p), 17.0);
        assert_eq!(p.operators[1].feedback, 0.3);
    }

    #[test]
    fn values_are_clamped_and_nan_is_ignored() {
        let mut p = Patch::default();
        Macro::Ratio2.set(&mut p, 99.0);
        assert_eq!(p.operators[1].ratio, MAX_RATIO);
        Macro::Index.set(&mut p, -1.0);
        assert_eq!(p.operators[1].level, 0.0);
        let before = p;
        for m in Macro::ALL {
            m.set(&mut p, f64::NAN);
            m.set(&mut p, f64::INFINITY);
        }
        assert_eq!(p, before);
    }

    #[test]
    fn algorithm_zero_keeps_free_routing() {
        let mut p = Patch::default();
        Macro::Algorithm.set(&mut p, 0.0);
        assert_eq!(p, Patch::default());
        Macro::Algorithm.set(&mut p, 5.2);
        assert_eq!(Macro::Algorithm.get(&p), 5.0);
        // 2 → 1, 4 → 3, 6 → 5; carriers 1, 3, 5.
        let carriers: Vec<_> = p.operators.iter().map(|o| o.output).collect();
        assert_eq!(carriers, [true, false, true, false, true, false]);
        assert!(p.operators[5].modulates[4]);
    }

    #[test]
    fn ratio_2_switches_operator_2_to_ratio_pitch() {
        let mut p = Patch::default();
        p.operators[1].pitch_mode = PitchMode::Frequency;
        Macro::Ratio2.set(&mut p, 3.5);
        assert_eq!(
            (p.operators[1].ratio, p.operators[1].pitch_mode),
            (3.5, PitchMode::Ratio)
        );
    }

    #[test]
    fn feedback_follows_the_strongest_then_the_algorithm_then_operator_2() {
        let mut p = Patch::default();
        Macro::Feedback.set(&mut p, 0.5);
        assert_eq!(p.operators[1].feedback, 0.5);

        let mut p = Patch::default();
        Macro::Algorithm.set(&mut p, 1.0); // feedback operator 6
        Macro::Feedback.set(&mut p, 0.25);
        assert_eq!(p.operators[5].feedback, 0.25);
        assert_eq!(Macro::Algorithm.get(&p), 1.0);

        let mut p = Patch::default();
        p.operators[3].feedback = 0.1;
        p.operators[4].feedback = 0.3;
        Macro::Feedback.set(&mut p, 0.6);
        assert!((p.operators[3].feedback - 0.2).abs() < 1e-6);
        assert_eq!(p.operators[4].feedback, 0.6);
        assert!(close(Macro::Feedback.get(&p), 0.6));
    }

    #[test]
    fn envelope_macros_scale_the_other_carriers() {
        let mut p = Patch::default();
        Macro::Algorithm.set(&mut p, 5.0); // carriers 1, 3, 5
        p.operators[0].envelope.release = 0.5;
        p.operators[2].envelope.release = 1.0;
        p.operators[4].envelope.release = 2.0;
        let modulator = p.operators[1].envelope;
        Macro::Release.set(&mut p, time_to_knob(1.0));
        let releases: Vec<_> = [0, 2, 4]
            .iter()
            .map(|&i| p.operators[i].envelope.release)
            .collect();
        assert!((releases[0] - 1.0).abs() < 1e-5);
        assert!((releases[1] - 2.0).abs() < 1e-4 && (releases[2] - 4.0).abs() < 1e-4);
        assert_eq!(p.operators[1].envelope, modulator);

        // A first carrier at 0 sets every carrier.
        p.operators[0].envelope.sustain = 0.0;
        p.operators[2].envelope.sustain = 0.4;
        Macro::Sustain.set(&mut p, 0.8);
        assert_eq!(
            (
                p.operators[0].envelope.sustain,
                p.operators[2].envelope.sustain
            ),
            (0.8, 0.8)
        );
        // Scaling up clamps at the top of the range.
        Macro::Sustain.set(&mut p, 0.4);
        p.operators[4].envelope.sustain = 0.9;
        Macro::Sustain.set(&mut p, 1.0);
        assert_eq!(p.operators[4].envelope.sustain, 1.0);
    }

    #[test]
    fn param_infos_match_the_macros() {
        let keys: Vec<_> = Macro::ALL.iter().map(|m| m.key()).collect();
        assert_eq!(
            keys,
            [
                "algorithm",
                "ratio2",
                "index",
                "feedback",
                "attack",
                "decay",
                "sustain",
                "release"
            ]
        );
        for m in Macro::ALL {
            let info = m.param_info();
            assert_eq!(info.id.as_str(), m.key());
            assert_eq!(info.name, m.name());
            assert_eq!(info.range(), (m.min(), m.max()), "{m:?}");
            assert_eq!(info.steps(), m.step_count(), "{m:?}");
            assert_eq!(info.default, m.default_value());
            assert_eq!(info.format(0.4), m.text(0.4), "{m:?}");
            assert!(info.automatable);
            // Normalising and back keeps the macro's own value.
            let v = info.snap(0.4);
            assert!(
                (info.to_plain(info.to_normalized(v)) - v).abs() < 1e-9,
                "{m:?}"
            );
        }
        assert_eq!(Macro::Algorithm.param_info().format(0.0), "Frei");
        assert!(!Macro::Algorithm.param_info().modulatable);
    }

    #[test]
    fn texts() {
        assert_eq!(Macro::Algorithm.text(0.0), "Frei");
        assert_eq!(Macro::Algorithm.text(12.0), "12");
        assert_eq!(Macro::Ratio2.text(3.5), "3.50");
        assert_eq!(Macro::Index.text(0.125), "13 %");
        assert_eq!(Macro::Sustain.text(f64::NAN), "0 %");
        assert_eq!(Macro::Attack.text(0.0), "1 ms");
        assert_eq!(Macro::Release.text(time_to_knob(1.2)), "1.20 s");
        assert_eq!(Macro::Decay.text(1.0), "30.0 s");
    }
}
