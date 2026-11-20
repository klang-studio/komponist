//! Factory presets, grouped in categories.
//!
//! Each preset is a small build function, so [`preset`] constructs only the
//! one asked for: a patch is about 1.7 KB (room for all modulators), and the
//! Pi loads presets on a small stack. The volume of each preset is set in the
//! category tables, to level the presets against each other in one place;
//! volume modulation depths in the build functions are relative to it (0.1 =
//! ±10 % of the volume), so leveling keeps tremolos as deep as designed.

mod build;

mod ambient;
mod bass;
mod chip;
mod filter_ops;
mod folded;
mod fx;
mod keys;
mod leads;
mod lofi;
mod mallets;
mod matrix;
mod pads;
mod plucked;
mod rave;
mod rhythm;
mod synth;
mod synthbass;
mod techno;
mod winds;

use crate::Patch;

/// A factory preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub name: &'static str,
    pub category: &'static str,
    pub patch: Patch,
}

/// One table row: name, master volume, build function.
struct Entry {
    name: &'static str,
    volume: f32,
    build: fn() -> Patch,
}

const fn entry(name: &'static str, volume: f32, build: fn() -> Patch) -> Entry {
    Entry {
        name,
        volume,
        build,
    }
}

fn init() -> Patch {
    Patch::default()
}

/// Categories in menu order. "Init" stays first and "E-Piano" second: the
/// Pi's boot benchmark measures presets 0 and 1 (2 and 6 operators).
const CATEGORIES: [(&str, &[Entry]); 20] = [
    ("Init", &[entry("Init", 0.25, init)]),
    ("Tasten", keys::LIST),
    ("Mallets & Glocken", mallets::LIST),
    ("Gezupft", plucked::LIST),
    ("Bass", bass::LIST),
    ("Blech & Holz", winds::LIST),
    ("Streicher & Pads", pads::LIST),
    ("Leads", leads::LIST),
    ("Synth", synth::LIST),
    ("Effekte & Perkussion", fx::LIST),
    // New categories go last, so the numbers of the presets before stay.
    ("Verzerrt & Lo-Fi", lofi::LIST),
    ("Wavefolder", folded::LIST),
    ("Rhythmisch", rhythm::LIST),
    ("Filter-Operatoren", filter_ops::LIST),
    ("Matrix", matrix::LIST),
    ("Techno & House", techno::LIST),
    ("Trance & Rave", rave::LIST),
    ("Synthbass", synthbass::LIST),
    ("Chiptune", chip::LIST),
    ("Ambient & Glitch", ambient::LIST),
];

const fn count() -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < CATEGORIES.len() {
        n += CATEGORIES[i].1.len();
        i += 1;
    }
    n
}

/// Number of factory presets.
pub const PRESET_COUNT: usize = count();

/// Pitch wheel range of every factory preset, in semitones each way: the
/// wheel is a modulation source ([`crate::ModSource::PitchBend`]), and the
/// presets route it to the master tuning.
pub const PITCH_BEND_RANGE: f32 = 2.0;

/// Factory preset `index` (0-based), built on demand. Every preset bends
/// with the pitch wheel by [`PITCH_BEND_RANGE`].
pub fn preset(index: usize) -> Option<Preset> {
    let mut i = index;
    for (category, entries) in CATEGORIES {
        if let Some(entry) = entries.get(i) {
            let mut patch = (entry.build)();
            patch.volume = entry.volume;
            for route in patch.modulation.as_mut_slice() {
                if route.target == crate::ModTarget::Volume {
                    route.depth *= entry.volume;
                }
            }
            let bend = crate::ModSlot {
                source: crate::ModSource::PitchBend,
                depth: PITCH_BEND_RANGE,
            };
            patch.modulate(crate::ModTarget::Tune, bend);
            return Some(Preset {
                name: entry.name,
                category,
                patch,
            });
        }
        i -= entries.len();
    }
    None
}

/// Name of preset `index` without building it.
pub fn preset_name(index: usize) -> Option<&'static str> {
    let mut i = index;
    for (_, entries) in CATEGORIES {
        if let Some(entry) = entries.get(i) {
            return Some(entry.name);
        }
        i -= entries.len();
    }
    None
}

/// Category of preset `index` without building it.
pub fn preset_category(index: usize) -> Option<&'static str> {
    let mut i = index;
    for (category, entries) in CATEGORIES {
        if i < entries.len() {
            return Some(category);
        }
        i -= entries.len();
    }
    None
}

/// The categories in menu order.
pub fn categories() -> impl Iterator<Item = &'static str> {
    CATEGORIES.iter().map(|&(category, _)| category)
}

/// All factory presets in order.
pub fn presets() -> impl Iterator<Item = Preset> {
    (0..PRESET_COUNT).filter_map(preset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilterKind, ModSource, ModulationMode, PitchMode, Synth};

    const SR: f32 = 48_000.0;

    /// The whole synth with the master effects, as the presets are heard.
    fn synth(patch: Patch) -> Box<Synth<1>> {
        let mut synth = Box::<Synth<1>>::new_uninit();
        // SAFETY: a valid place, fully initialized by `write_new`.
        unsafe {
            Synth::write_new(synth.as_mut_ptr(), SR, patch);
            synth.assume_init()
        }
    }

    /// Both channels interleaved: peaks and silence checks cover both.
    fn render(synth: &mut Synth<1>, seconds: f32) -> Vec<f32> {
        let mut l = vec![0.0; (seconds * SR) as usize];
        let mut r = l.clone();
        synth.render(&mut l, &mut r);
        l.into_iter().zip(r).flat_map(|(a, b)| [a, b]).collect()
    }

    #[test]
    fn presets_have_unique_names_and_valid_values() {
        assert_eq!(PRESET_COUNT, 181);
        let all: Vec<Preset> = presets().collect();
        assert_eq!(all.len(), PRESET_COUNT);
        assert_eq!((all[0].name, all[1].name), ("Init", "E-Piano"));
        for (i, p) in all.iter().enumerate() {
            assert!(
                all[..i].iter().all(|q| q.name != p.name),
                "duplicate {}",
                p.name
            );
            assert_eq!(
                p.patch.sanitized(),
                p.patch,
                "{} has out-of-range values",
                p.name
            );
            assert_eq!(preset_name(i), Some(p.name));
            assert_eq!(preset_category(i), Some(p.category));
            let bends = p
                .patch
                .modulation
                .as_slice()
                .iter()
                .filter(|r| r.source == crate::ModSource::PitchBend);
            assert_eq!(
                bends.map(|r| (r.target, r.depth)).collect::<Vec<_>>(),
                [(crate::ModTarget::Tune, PITCH_BEND_RANGE)],
                "{}",
                p.name
            );
        }
        assert!(preset(PRESET_COUNT).is_none() && preset_name(PRESET_COUNT).is_none());
        assert!(preset_category(PRESET_COUNT).is_none());
        assert_eq!(categories().count(), CATEGORIES.len());
    }

    /// The presets show off what the engine can do.
    #[test]
    fn presets_use_the_features() {
        let all: Vec<Patch> = presets().map(|p| p.patch).collect();
        let count = |pred: &dyn Fn(&Patch) -> bool| all.iter().filter(|p| pred(p)).count();
        let any_op =
            |p: &Patch, pred: &dyn Fn(&crate::OperatorParams) -> bool| p.operators.iter().any(pred);
        use crate::ModTarget;
        let modulates = |p: &Patch, pred: &dyn Fn(&crate::ModRoute) -> bool| {
            p.modulation
                .as_slice()
                .iter()
                .any(|r| r.is_active() && pred(r))
        };
        let stereo = |p: &Patch| {
            p.balance != 0.0
                || modulates(p, &|r| r.target == ModTarget::Balance)
                || p.operators.iter().enumerate().any(|(i, o)| {
                    o.output && (o.pan != 0.0 || modulates(p, &|r| r.target == ModTarget::OpPan(i)))
                })
        };
        let looping = |p: &Patch| {
            p.envelopes.as_slice().iter().any(|e| e.looping)
                || p.operators.iter().any(|o| o.envelope.looping)
        };
        let shared_target = |p: &Patch| {
            let active: Vec<_> = p
                .modulation
                .as_slice()
                .iter()
                .filter(|r| r.is_active())
                .collect();
            active
                .iter()
                .enumerate()
                .any(|(i, a)| active[i + 1..].iter().any(|b| b.target == a.target))
        };
        let checks: [(&str, usize, usize); 27] = [
            ("stereo", count(&stereo), 30),
            (
                "filter",
                count(&|p| p.filters[0].kind != FilterKind::Off),
                35,
            ),
            (
                "drive",
                count(&|p| p.filters[0].kind != FilterKind::Off && p.filters[0].drive > 0.0),
                8,
            ),
            ("LFO", count(&|p| !p.lfos.is_empty()), 30),
            (
                "modulation envelope",
                count(&|p| !p.envelopes.is_empty()),
                25,
            ),
            (
                "velocity",
                count(&|p| modulates(p, &|r| r.source == ModSource::Velocity)),
                30,
            ),
            (
                "semitone pitch",
                count(&|p| any_op(p, &|o| o.pitch_mode == PitchMode::Semitone)),
                4,
            ),
            (
                "fixed pitch",
                count(&|p| any_op(p, &|o| o.pitch_mode == PitchMode::Frequency)),
                4,
            ),
            (
                "FM mode",
                count(&|p| any_op(p, &|o| o.mode == ModulationMode::Frequency)),
                4,
            ),
            (
                "PWM mode",
                count(&|p| any_op(p, &|o| o.mode == ModulationMode::PulseWidth)),
                4,
            ),
            (
                "sync mode",
                count(&|p| any_op(p, &|o| o.mode == ModulationMode::Sync)),
                4,
            ),
            (
                "LFO driven by envelope or velocity",
                count(&|p| {
                    modulates(p, &|r| {
                        matches!(r.target, ModTarget::LfoAmount(_) | ModTarget::LfoRate(_))
                    })
                }),
                3,
            ),
            (
                "depth per connection",
                count(&|p| any_op(p, &|o| o.depths.iter().any(|&d| d < 1.0))),
                5,
            ),
            (
                "keyboard scaling",
                count(&|p| any_op(p, &|o| o.key_scaling != crate::KeyScaling::NONE)),
                20,
            ),
            (
                "mono",
                count(&|p| p.voice_mode == crate::VoiceMode::Mono),
                5,
            ),
            (
                "legato",
                count(&|p| p.voice_mode == crate::VoiceMode::Legato),
                5,
            ),
            ("portamento", count(&|p| p.portamento > 0.0), 5),
            ("unison", count(&|p| p.unison > 1), 8),
            (
                "noise",
                count(&|p| {
                    any_op(p, &|o| {
                        o.waveform == crate::Waveform::Noise && o.level > 0.0
                    })
                }),
                8,
            ),
            (
                "ring modulation",
                count(&|p| any_op(p, &|o| o.mode == ModulationMode::Ring)),
                4,
            ),
            ("distortion", count(&|p| p.effects.distortion.mix > 0.0), 8),
            ("bitcrusher", count(&|p| p.effects.bitcrusher.mix > 0.0), 5),
            (
                "operator fold",
                count(&|p| {
                    any_op(p, &|o| o.fold > 0.0)
                        || modulates(p, &|r| matches!(r.target, ModTarget::OpFold(_)))
                }),
                5,
            ),
            ("looping envelope", count(&looping), 5),
            (
                "filter operator",
                count(&|p| any_op(p, &|o| o.kind != crate::OperatorKind::Oscillator)),
                5,
            ),
            (
                "via",
                count(&|p| modulates(p, &|r| r.via != ModSource::None)),
                5,
            ),
            ("several sources on one target", count(&shared_target), 8),
        ];
        let short: Vec<String> = checks
            .iter()
            .filter(|(_, n, min)| n < min)
            .map(|(what, n, min)| format!("{what}: {n} < {min}"))
            .collect();
        assert!(short.is_empty(), "{short:?}");
    }

    /// Peak of the loud test chord, held for 2 s.
    fn chord_peak(patch: Patch) -> f32 {
        let mut e = synth(patch);
        for note in [36, 55, 64, 72] {
            e.note_on(note, 127);
        }
        render(&mut e, 2.0)
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()))
    }

    /// Tool, not a check: prints the volume that gives each preset a chord
    /// peak of `TARGET`, or for mono and legato presets (which play only the
    /// chord's last note) `MONO_TARGET`, about the peak of a single poly
    /// note. Measured without the master effects, since the volume acts
    /// before them; the effects' output `level` (linear, after them) is
    /// printed for presets whose effects push the peak to the clamp.
    /// `PRESET=name` limits it to presets whose name contains `name`.
    /// Run with `cargo test -p hexa-core calibrate -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn calibrate_volumes() {
        const TARGET: f32 = 0.7;
        const MONO_TARGET: f32 = 0.5;
        let only = std::env::var("PRESET").ok();
        for p in presets().filter(|p| only.as_ref().is_none_or(|n| p.name.contains(n.as_str()))) {
            let target = if p.patch.voice_mode == crate::VoiceMode::Poly {
                TARGET
            } else {
                MONO_TARGET
            };
            // At a tenth, to stay clear of the output clamp; the volume acts
            // after filter and drive, so the peak scales linearly with it.
            let mut quiet = p.patch;
            quiet.effects = crate::EffectParams::default();
            quiet.volume *= 0.1;
            for route in quiet.modulation.as_mut_slice() {
                if route.target == crate::ModTarget::Volume {
                    route.depth *= 0.1;
                }
            }
            let peak = chord_peak(quiet) * 10.0;
            let volume = (p.patch.volume * target / peak).min(1.0);
            let mut through = p.patch;
            through.effects.level *= 0.1;
            let level = (p.patch.effects.level * target / (chord_peak(through) * 10.0)).min(1.0);
            println!(
                "CALIBRATE\t{}\t{volume:.3}\t{peak:.2}\tlevel {level:.3}",
                p.name
            );
        }
    }

    #[test]
    fn presets_sound_without_clipping_and_end() {
        let mut problems = Vec::new();
        for p in presets() {
            let mut e = synth(p.patch);
            // A loud four-note chord across the keyboard.
            for note in [36, 55, 64, 72] {
                e.note_on(note, 127);
            }
            let held = render(&mut e, 2.0);
            let peak = held.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            println!("{:<22} {:<20} peak {peak:.2}", p.name, p.category);
            // Comparable loudness across presets, and no clipping, through
            // the master effects.
            if !(0.4..0.95).contains(&peak) {
                problems.push(format!("{}: peak {peak:.2}", p.name));
            }

            for note in [36, 55, 64, 72] {
                e.note_off(note);
            }
            let mut seconds = 0.0;
            while e.active_voices() != 0 && seconds < 12.0 {
                render(&mut e, 0.5);
                seconds += 0.5;
            }
            if e.active_voices() != 0 {
                problems.push(format!("{}: does not end", p.name));
            }
        }
        assert!(problems.is_empty(), "{problems:?}");
    }
}
