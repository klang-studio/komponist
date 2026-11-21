//! volt's parameters: wire ids, ranges, defaults and texts (proposal `va-synth`, variant B).
//!
//! The wire id is the index in [`params`]; [`id`] names every index. Plain values follow the
//! model: Hz, seconds, cents, semitones, a choice's index, **fractions** for everything shown as
//! percent (0.5 = 50 %). The two view switches of the proposal (`env_view`, `lfo_view`) are not
//! parameters: they are context selections of the layout, like hexa's operator
//! (`CellParam::Choice`), so a view is not part of the sound or of a saved state.

use komponist_i18n::Lang;
use komponist_model::format::MINUS;
use komponist_model::params::va_curve::{FILTER_TYPES, LFO_DIVS, dst as dest, src as source};
use komponist_model::params::{ParamInfo, ParamKind, Scale, Unit};

/// Declares the wire ids in order (`id::CUTOFF` …) and the list of their keys.
macro_rules! ids {
    ($($name:ident = $key:literal,)*) => {
        /// The model id (key) of every wire id, in order.
        pub const KEYS: &[&str] = &[$($key),*];
        /// The wire ids.
        pub mod id {
            ids!(@consts 0usize; $($name,)*);
        }
    };
    (@consts $n:expr; $name:ident, $($rest:ident,)*) => {
        pub const $name: usize = $n;
        ids!(@consts $n + 1; $($rest,)*);
    };
    (@consts $n:expr;) => {};
}

ids! {
    OSC_A_WAVE = "osc_a_wave",
    OSC_B_WAVE = "osc_b_wave",
    OSC_A_PW = "osc_a_pw",
    OSC_B_PW = "osc_b_pw",
    OSC_B_OCT = "osc_b_oct",
    OSC_B_FINE = "osc_b_fine",
    OSC_MIX = "osc_mix",
    SUB_LEVEL = "sub_level",
    NOISE_LEVEL = "noise_level",
    FILTER_TYPE = "filter_type",
    CUTOFF = "cutoff",
    RESONANCE = "resonance",
    DRIVE = "drive",
    KEY_AMOUNT = "key_amount",
    HP_FREQ = "hp_freq",
    ENV_AMOUNT = "env_amount",
    AMP_A = "amp_a",
    AMP_D = "amp_d",
    AMP_S = "amp_s",
    AMP_R = "amp_r",
    FILT_A = "filt_a",
    FILT_D = "filt_d",
    FILT_S = "filt_s",
    FILT_R = "filt_r",
    MOD_A = "mod_a",
    MOD_D = "mod_d",
    MOD_S = "mod_s",
    MOD_R = "mod_r",
    LFO1_SHAPE = "lfo1_shape",
    LFO1_RATE = "lfo1_rate",
    LFO1_SYNC = "lfo1_sync",
    LFO1_DIV = "lfo1_div",
    LFO1_KEY = "lfo1_key",
    LFO1_FADE = "lfo1_fade",
    LFO1_UNI = "lfo1_uni",
    LFO2_SHAPE = "lfo2_shape",
    LFO2_RATE = "lfo2_rate",
    LFO2_SYNC = "lfo2_sync",
    LFO2_DIV = "lfo2_div",
    LFO2_KEY = "lfo2_key",
    LFO2_FADE = "lfo2_fade",
    LFO2_UNI = "lfo2_uni",
    MOD1_SRC = "mod1_src",
    MOD1_DST = "mod1_dst",
    MOD1_AMT = "mod1_amt",
    MOD2_SRC = "mod2_src",
    MOD2_DST = "mod2_dst",
    MOD2_AMT = "mod2_amt",
    MOD3_SRC = "mod3_src",
    MOD3_DST = "mod3_dst",
    MOD3_AMT = "mod3_amt",
    MOD4_SRC = "mod4_src",
    MOD4_DST = "mod4_dst",
    MOD4_AMT = "mod4_amt",
    VELOCITY = "velocity",
    VEL_FILTER = "vel_filter",
    WHEEL_VIB = "wheel_vib",
    AT_CUT = "at_cut",
    GLIDE_LEGATO = "glide_legato",
    MODE = "mode",
    VOICES = "voices",
    GLIDE = "glide",
    BEND = "bend",
    OCTAVE = "octave",
    UNISON = "unison",
    DETUNE = "detune",
    SPREAD = "spread",
    ANALOG = "analog",
    VOLUME = "volume",
}

/// How many parameters there are.
pub const COUNT: usize = KEYS.len();

/// The voice modes in the order of `mode`.
pub const MODES: [&str; 3] = ["Poly", "Mono", "Legato"];

/// Glide and fade are `Log` from this many seconds; the lowest value means "aus".
pub const OFF_S: f64 = 0.001;

/// The wire id of `modN_src` (`slot` 0 … 3); `+ 1` is its destination, `+ 2` its amount.
pub const fn mod_src(slot: usize) -> usize {
    id::MOD1_SRC + 3 * slot
}

/// The wire ids of an LFO's parameters in order: shape, rate, sync, divider, key, fade, uni.
pub const fn lfo_base(lfo: usize) -> usize {
    id::LFO1_SHAPE + 7 * lfo
}

/// Offsets inside an LFO's block ([`lfo_base`]).
pub mod lfo {
    /// Shape.
    pub const SHAPE: usize = 0;
    /// Rate in Hz.
    pub const RATE: usize = 1;
    /// Tempo sync on.
    pub const SYNC: usize = 2;
    /// Divider when synced.
    pub const DIV: usize = 3;
    /// Restart on every key.
    pub const KEY: usize = 4;
    /// Fade-in.
    pub const FADE: usize = 5;
    /// Unipolar.
    pub const UNI: usize = 6;
}

fn names(list: &[&str]) -> ParamKind {
    ParamKind::Enum(list.iter().map(|s| (*s).to_owned()).collect())
}

fn percent(key: &str, name: &str, min: f64, max: f64, default: f64) -> ParamInfo {
    ParamInfo::new(key, name, ParamKind::lin(min, max), default).unit(Unit::Percent)
}

fn time(key: &str, name: &str, min: f64, max: f64, default: f64) -> ParamInfo {
    ParamInfo::new(key, name, ParamKind::scaled(min, max, Scale::Log), default).unit(Unit::S)
}

/// A whole number with its sign (U+2212 for the minus) and a suffix.
fn whole(v: f64, suffix: &str) -> String {
    let r = v.round();
    if r < 0.0 {
        format!("{MINUS}{} {suffix}", -r)
    } else {
        format!("{r} {suffix}")
    }
}

fn cents_text(v: f64) -> String {
    whole(v, "ct")
}

fn semitones_text(v: f64) -> String {
    whole(v, "st")
}

/// Glide and fade: "aus" ("off") at the lowest value, else seconds.
fn off_or_seconds(t: Lang) -> impl Fn(f64) -> String + Send + Sync + 'static {
    let off = t.value_off_lower();
    move |v| {
        if v <= OFF_S * 1.05 {
            return off.to_owned();
        }
        ParamInfo::new("", "", ParamKind::scaled(OFF_S, 20.0, Scale::Log), v)
            .unit(Unit::S)
            .format(v)
    }
}

/// Octave offsets: "−1 Okt." in German, "−1 oct" in English.
fn octaves_text(t: Lang) -> impl Fn(f64) -> String + Send + Sync + 'static {
    let unit = t.value_unit_octave();
    move |v| whole(v, unit)
}

/// The waveforms in `t`, in the order of `va_curve::WAVES`.
fn wave_names(t: Lang) -> [&'static str; 4] {
    [
        t.choice_volt_wave_sine(),
        t.choice_volt_wave_triangle(),
        t.choice_volt_wave_saw(),
        t.choice_volt_wave_square(),
    ]
}

/// The LFO shapes in `t`, in the order of `va_curve::LFO_SHAPES`.
fn lfo_shape_names(t: Lang) -> [&'static str; 6] {
    let [sine, triangle, saw, square] = wave_names(t);
    [
        sine,
        triangle,
        saw,
        square,
        t.choice_volt_lfo_random(),
        t.choice_volt_lfo_smooth(),
    ]
}

/// The tempo dividers in `t`, in the order of [`LFO_DIVS`]; from 1/2 on they are notation.
fn lfo_div_names(t: Lang) -> [&'static str; 8] {
    [
        t.choice_volt_div_4_bars(),
        t.choice_volt_div_2_bars(),
        t.choice_volt_div_1_bar(),
        LFO_DIVS[3],
        LFO_DIVS[4],
        LFO_DIVS[5],
        LFO_DIVS[6],
        LFO_DIVS[7],
    ]
}

/// The matrix sources in `t`, in the order of [`source::NAMES`].
fn source_names(t: Lang) -> [&'static str; 9] {
    [
        t.value_off(),
        source::NAMES[1],
        source::NAMES[2],
        t.choice_volt_source_env3(),
        t.param_velocity(),
        t.param_key(),
        t.choice_volt_source_wheel(),
        t.choice_volt_source_pressure(),
        t.choice_volt_lfo_random(),
    ]
}

/// The matrix destinations in `t`, in the order of [`dest::NAMES`].
fn dest_names(t: Lang) -> [&'static str; dest::COUNT] {
    [
        t.value_off(),
        t.choice_volt_dest_pitch(),
        t.choice_volt_dest_pitch_b(),
        t.param_volt_width_a(),
        t.param_volt_width_b(),
        t.param_volt_osc_mix(),
        t.param_cutoff(),
        t.param_resonance(),
        t.param_drive(),
        t.param_volume(),
        t.param_detune(),
        t.choice_volt_dest_lfo1_rate(),
        t.choice_volt_dest_lfo2_rate(),
    ]
}

/// An LFO rate: two places under 0.1 Hz, one under 10 Hz, whole above.
fn rate_text(v: f64) -> String {
    if v < 0.1 {
        format!("{v:.2} Hz")
    } else if v < 10.0 {
        format!("{v:.1} Hz")
    } else {
        format!("{} Hz", v.round())
    }
}

fn lfo_params(t: Lang, n: usize, shape: f64, rate: f64) -> Vec<ParamInfo> {
    let key = |what: &str| format!("lfo{n}_{what}");
    vec![
        ParamInfo::new(
            key("shape").as_str(),
            t.param_volt_lfo_shape(),
            names(&lfo_shape_names(t)),
            shape,
        ),
        ParamInfo::new(
            key("rate").as_str(),
            t.param_rate(),
            ParamKind::scaled(0.02, 30.0, Scale::Log),
            rate,
        )
        .unit(Unit::Hz)
        .formatted(rate_text)
        .flags(true, true),
        ParamInfo::new(key("sync").as_str(), t.param_sync(), ParamKind::Bool, 0.0),
        ParamInfo::new(
            key("div").as_str(),
            t.param_volt_lfo_div(),
            names(&lfo_div_names(t)),
            4.0,
        ),
        ParamInfo::new(key("key").as_str(), t.param_key(), ParamKind::Bool, 1.0),
        time(&key("fade"), t.param_fade(), OFF_S, 5.0, OFF_S).formatted(off_or_seconds(t)),
        ParamInfo::new(key("uni").as_str(), t.param_lfo_uni(), ParamKind::Bool, 0.0),
    ]
}

/// The parameters; the index is the wire id ([`id`]).
pub fn params(t: Lang) -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::new(
            "osc_a_wave",
            t.param_volt_osc_a(),
            names(&wave_names(t)),
            2.0,
        ),
        ParamInfo::new(
            "osc_b_wave",
            t.param_volt_osc_b(),
            names(&wave_names(t)),
            3.0,
        ),
        percent("osc_a_pw", t.param_volt_width_a(), 0.05, 0.95, 0.5).flags(true, true),
        percent("osc_b_pw", t.param_volt_width_b(), 0.05, 0.95, 0.5).flags(true, true),
        ParamInfo::new(
            "osc_b_oct",
            t.param_volt_b_oct(),
            ParamKind::Int { min: -3, max: 3 },
            0.0,
        )
        .formatted(octaves_text(t)),
        ParamInfo::new(
            "osc_b_fine",
            t.param_volt_b_fine(),
            ParamKind::lin(-100.0, 100.0),
            7.0,
        )
        .unit(Unit::Cents)
        .formatted(cents_text)
        .flags(true, true),
        percent("osc_mix", t.param_volt_osc_mix(), 0.0, 1.0, 0.5).flags(true, true),
        percent("sub_level", t.param_sub(), 0.0, 1.0, 0.25),
        percent("noise_level", t.param_volt_noise(), 0.0, 1.0, 0.0),
        ParamInfo::new("filter_type", t.param_type(), names(&FILTER_TYPES), 0.0),
        ParamInfo::new(
            "cutoff",
            t.param_cutoff(),
            ParamKind::scaled(20.0, 20_000.0, Scale::Log),
            1200.0,
        )
        .unit(Unit::Hz)
        .flags(true, true),
        percent("resonance", t.param_resonance(), 0.0, 1.0, 0.3).flags(true, true),
        percent("drive", t.param_drive(), 0.0, 1.0, 0.15).flags(true, true),
        percent("key_amount", t.param_key(), -1.0, 2.0, 0.5),
        ParamInfo::new(
            "hp_freq",
            t.param_volt_hp(),
            ParamKind::scaled(20.0, 2000.0, Scale::Log),
            20.0,
        )
        .unit(Unit::Hz),
        percent("env_amount", t.param_volt_env_amount(), -1.0, 1.0, 0.4),
    ];
    for (prefix, a, d, s, r) in [
        ("amp", 0.003, 0.3, 0.7, 0.25),
        ("filt", 0.002, 0.35, 0.3, 0.25),
        ("mod", 0.001, 0.6, 0.0, 0.3),
    ] {
        p.push(time(
            &format!("{prefix}_a"),
            t.param_volt_envelope_a(),
            0.001,
            10.0,
            a,
        ));
        p.push(time(
            &format!("{prefix}_d"),
            t.param_volt_envelope_d(),
            0.001,
            10.0,
            d,
        ));
        p.push(percent(
            &format!("{prefix}_s"),
            t.param_volt_envelope_s(),
            0.0,
            1.0,
            s,
        ));
        p.push(time(
            &format!("{prefix}_r"),
            t.param_volt_envelope_r(),
            0.001,
            20.0,
            r,
        ));
    }
    p.extend(lfo_params(t, 1, 0.0, 4.5));
    p.extend(lfo_params(t, 2, 1.0, 0.6));
    for n in 1..=4 {
        p.push(ParamInfo::new(
            format!("mod{n}_src").as_str(),
            t.param_volt_source(n),
            names(&source_names(t)),
            0.0,
        ));
        p.push(ParamInfo::new(
            format!("mod{n}_dst").as_str(),
            t.param_volt_target(n),
            names(&dest_names(t)),
            0.0,
        ));
        p.push(percent(
            &format!("mod{n}_amt"),
            &t.param_volt_amount(n),
            -1.0,
            1.0,
            0.0,
        ));
    }
    p.extend([
        percent("velocity", t.param_velocity(), 0.0, 1.0, 0.6),
        percent("vel_filter", t.param_volt_vel_filter(), 0.0, 1.0, 0.3),
        ParamInfo::new(
            "wheel_vib",
            t.param_volt_wheel_vib(),
            ParamKind::lin(0.0, 200.0),
            50.0,
        )
        .unit(Unit::Cents)
        .formatted(cents_text),
        percent("at_cut", t.param_volt_at_cut(), -1.0, 1.0, 0.0),
        ParamInfo::new(
            "glide_legato",
            t.param_volt_glide_legato(),
            ParamKind::Bool,
            0.0,
        ),
        ParamInfo::new("mode", t.param_mode(), names(&MODES), 0.0),
        ParamInfo::new(
            "voices",
            t.param_volt_voices(),
            ParamKind::Int { min: 1, max: 16 },
            8.0,
        ),
        time("glide", t.param_glide(), OFF_S, 5.0, OFF_S).formatted(off_or_seconds(t)),
        ParamInfo::new(
            "bend",
            t.param_bend(),
            ParamKind::Int { min: 0, max: 24 },
            2.0,
        )
        .formatted(semitones_text),
        ParamInfo::new(
            "octave",
            t.param_volt_octave(),
            ParamKind::Int { min: -2, max: 2 },
            0.0,
        )
        .formatted(octaves_text(t)),
        ParamInfo::new(
            "unison",
            t.param_unison(),
            ParamKind::Int { min: 1, max: 7 },
            1.0,
        ),
        ParamInfo::new("detune", t.param_detune(), ParamKind::lin(0.0, 100.0), 14.0)
            .unit(Unit::Cents)
            .formatted(cents_text)
            .flags(true, true),
        percent("spread", t.param_spread(), 0.0, 1.0, 0.6),
        percent("analog", t.param_analog(), 0.0, 1.0, 0.25),
        ParamInfo::new(
            "volume",
            t.param_volume(),
            ParamKind::scaled(-60.0, 6.0, Scale::Db),
            -6.0,
        )
        .unit(Unit::Db)
        .flags(true, true),
    ]);
    p
}
