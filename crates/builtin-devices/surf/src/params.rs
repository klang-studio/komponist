//! surf's parameters: volt's table ([`komponist_volt::params`]) by wire id, so the shared engine
//! reads them in the same places, with the oscillators read as wavetables, and two parameters of
//! its own after it.
//!
//! | Wire id | volt | surf |
//! |---|---|---|
//! | `id::OSC_A_WAVE`, `id::OSC_B_WAVE` | `osc_*_wave` (4 waveforms) | `osc_*_table` (16 tables) |
//! | `id::OSC_A_PW`, `id::OSC_B_PW` | `osc_*_pw` (pulse width) | `osc_*_pos` (table position 0–100 %) |
//! | `mod*_dst` 3 and 4 | Breite A/B | Position A/B |
//! | [`OSC_A_OCT`], [`OSC_A_FINE`] | – | `osc_a_oct`, `osc_a_fine` |
//!
//! Oscillator B keeps volt's `osc_b_oct` and `osc_b_fine`; in surf both oscillators are tuned
//! against the played pitch (volt tunes B against A, which has no tuning of its own).

use komponist_device_dsp::va_curve::dst;
use komponist_device_dsp::wavetable::TABLES;
use komponist_i18n::Lang;
use komponist_model::params::{ParamInfo, ParamKind};
use komponist_volt::params::{
    self as volt, cents_text, dest_names, id, names, octaves_text, percent,
};

/// Wire id of oscillator A's octave.
pub const OSC_A_OCT: usize = volt::COUNT;
/// Wire id of oscillator A's fine tune.
pub const OSC_A_FINE: usize = volt::COUNT + 1;
/// How many parameters there are.
pub const COUNT: usize = volt::COUNT + 2;

/// Table indices ([`TABLES`]) the defaults and presets use by name.
pub mod table {
    /// Sine, triangle, saw, square.
    pub const BASIC: f64 = 0.0;
    /// Pulse width.
    pub const PULSE: f64 = 1.0;
    /// Hard sync.
    pub const SYNC: f64 = 2.0;
    /// Harmonics one by one.
    pub const HARMONICS: f64 = 3.0;
    /// Odd to even.
    pub const ODD_EVEN: f64 = 4.0;
    /// Organ.
    pub const ORGAN: f64 = 5.0;
    /// Vowels.
    pub const VOWELS: f64 = 6.0;
    /// Choir.
    pub const CHOIR: f64 = 7.0;
    /// FM.
    pub const FM: f64 = 8.0;
    /// Wavefolder.
    pub const FOLD: f64 = 9.0;
    /// Bit reduction.
    pub const BITS: f64 = 10.0;
    /// Comb.
    pub const COMB: f64 = 11.0;
    /// Glass.
    pub const GLASS: f64 = 12.0;
    /// Hollow.
    pub const HOLLOW: f64 = 13.0;
    /// Resonance.
    pub const RESONANCE: f64 = 14.0;
    /// Random.
    pub const RANDOM: f64 = 15.0;
}

/// The table names in `t`, in the order of [`TABLES`].
pub fn table_names(t: Lang) -> [&'static str; TABLES.len()] {
    [
        t.choice_surf_table_basic(),
        t.choice_surf_table_pulse(),
        t.choice_surf_table_sync(),
        t.choice_surf_table_harmonics(),
        t.choice_surf_table_odd_even(),
        t.choice_surf_table_organ(),
        t.choice_surf_table_vowels(),
        t.choice_surf_table_choir(),
        t.choice_surf_table_fm(),
        t.choice_surf_table_fold(),
        t.choice_surf_table_bits(),
        t.choice_surf_table_comb(),
        t.choice_surf_table_glass(),
        t.choice_surf_table_hollow(),
        t.choice_surf_table_resonance(),
        t.choice_surf_table_random(),
    ]
}

/// The matrix destinations in `t`: volt's, the widths become the positions.
fn destinations(t: Lang) -> [&'static str; dst::COUNT] {
    let mut list = dest_names(t);
    list[dst::PW_A] = t.param_surf_position_a();
    list[dst::PW_B] = t.param_surf_position_b();
    list
}

/// The parameters; the index is the wire id.
pub fn params(t: Lang) -> Vec<ParamInfo> {
    let mut p = volt::params(t);
    let tables = names(&table_names(t));
    // Init: a saw-like A against a square-like B, as volt's saw and pulse.
    p[id::OSC_A_WAVE] = ParamInfo::new(
        "osc_a_table",
        t.param_surf_table_a(),
        tables.clone(),
        table::BASIC,
    );
    p[id::OSC_B_WAVE] = ParamInfo::new("osc_b_table", t.param_surf_table_b(), tables, table::BASIC);
    p[id::OSC_A_PW] =
        percent("osc_a_pos", t.param_surf_position_a(), 0.0, 1.0, 0.67).flags(true, true);
    p[id::OSC_B_PW] =
        percent("osc_b_pos", t.param_surf_position_b(), 0.0, 1.0, 1.0).flags(true, true);
    let dests = destinations(t);
    for n in 1..=4 {
        let wire = volt::mod_src(n - 1) + 1;
        p[wire] = ParamInfo::new(
            format!("mod{n}_dst").as_str(),
            t.param_volt_target(n),
            names(&dests),
            0.0,
        );
    }
    p.push(
        ParamInfo::new(
            "osc_a_oct",
            t.param_surf_a_oct(),
            ParamKind::Int { min: -3, max: 3 },
            0.0,
        )
        .formatted(octaves_text(t)),
    );
    p.push(
        ParamInfo::new(
            "osc_a_fine",
            t.param_surf_a_fine(),
            ParamKind::lin(-100.0, 100.0),
            0.0,
        )
        .formatted(cents_text)
        .flags(true, true),
    );
    debug_assert_eq!(p.len(), COUNT);
    p
}
