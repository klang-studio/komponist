//! knurr's parameters: wire ids, ranges, defaults and texts (proposal `va-synth`, variant B).
//!
//! The wire id is the index in [`params`]; [`id`] names every index. Plain values follow the
//! model: Hz, seconds, cents, semitones, a choice's index, **fractions** for everything shown as
//! percent (0.5 = 50 %). The two view switches of the proposal (`env_view`, `lfo_view`) are not
//! parameters: they are context selections of the layout, like pisynth's operator
//! (`CellParam::Choice`), so a view is not part of the sound or of a saved state.

use komponist_model::format::MINUS;
use komponist_model::params::va_curve::{
    FILTER_TYPES, LFO_DIVS, LFO_SHAPES, WAVES, dst as dest, src as source,
};
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

fn octaves_text(v: f64) -> String {
    whole(v, "Okt.")
}

/// Glide and fade: "aus" at the lowest value, else seconds.
fn off_or_seconds(v: f64) -> String {
    if v <= OFF_S * 1.05 {
        return "aus".to_owned();
    }
    ParamInfo::new("", "", ParamKind::scaled(OFF_S, 20.0, Scale::Log), v)
        .unit(Unit::S)
        .format(v)
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

fn lfo_params(n: usize, shape: f64, rate: f64) -> Vec<ParamInfo> {
    let key = |what: &str| format!("lfo{n}_{what}");
    vec![
        ParamInfo::new(key("shape").as_str(), "Form", names(&LFO_SHAPES), shape),
        ParamInfo::new(
            key("rate").as_str(),
            "Rate",
            ParamKind::scaled(0.02, 30.0, Scale::Log),
            rate,
        )
        .unit(Unit::Hz)
        .formatted(rate_text)
        .flags(true, true),
        ParamInfo::new(key("sync").as_str(), "Sync", ParamKind::Bool, 0.0),
        ParamInfo::new(key("div").as_str(), "Teiler", names(&LFO_DIVS), 4.0),
        ParamInfo::new(key("key").as_str(), "Key", ParamKind::Bool, 1.0),
        time(&key("fade"), "Fade", OFF_S, 5.0, OFF_S).formatted(off_or_seconds),
        ParamInfo::new(key("uni").as_str(), "Uni", ParamKind::Bool, 0.0),
    ]
}

/// The parameters; the index is the wire id ([`id`]).
pub fn params() -> Vec<ParamInfo> {
    let mut p = vec![
        ParamInfo::new("osc_a_wave", "Osz A", names(&WAVES), 2.0),
        ParamInfo::new("osc_b_wave", "Osz B", names(&WAVES), 3.0),
        percent("osc_a_pw", "Breite A", 0.05, 0.95, 0.5).flags(true, true),
        percent("osc_b_pw", "Breite B", 0.05, 0.95, 0.5).flags(true, true),
        ParamInfo::new(
            "osc_b_oct",
            "B Okt.",
            ParamKind::Int { min: -3, max: 3 },
            0.0,
        )
        .formatted(octaves_text),
        ParamInfo::new("osc_b_fine", "B Fein", ParamKind::lin(-100.0, 100.0), 7.0)
            .formatted(cents_text)
            .flags(true, true),
        percent("osc_mix", "Mix A/B", 0.0, 1.0, 0.5).flags(true, true),
        percent("sub_level", "Sub", 0.0, 1.0, 0.25),
        percent("noise_level", "Rauschen", 0.0, 1.0, 0.0),
        ParamInfo::new("filter_type", "Typ", names(&FILTER_TYPES), 0.0),
        ParamInfo::new(
            "cutoff",
            "Cutoff",
            ParamKind::scaled(20.0, 20_000.0, Scale::Log),
            1200.0,
        )
        .unit(Unit::Hz)
        .flags(true, true),
        percent("resonance", "Resonanz", 0.0, 1.0, 0.3).flags(true, true),
        percent("drive", "Drive", 0.0, 1.0, 0.15).flags(true, true),
        percent("key_amount", "Key", -1.0, 2.0, 0.5),
        ParamInfo::new(
            "hp_freq",
            "HP",
            ParamKind::scaled(20.0, 2000.0, Scale::Log),
            20.0,
        )
        .unit(Unit::Hz),
        percent("env_amount", "Hk. Menge", -1.0, 1.0, 0.4),
    ];
    for (prefix, a, d, s, r) in [
        ("amp", 0.003, 0.3, 0.7, 0.25),
        ("filt", 0.002, 0.35, 0.3, 0.25),
        ("mod", 0.001, 0.6, 0.0, 0.3),
    ] {
        p.push(time(&format!("{prefix}_a"), "A", 0.001, 10.0, a));
        p.push(time(&format!("{prefix}_d"), "D", 0.001, 10.0, d));
        p.push(percent(&format!("{prefix}_s"), "S", 0.0, 1.0, s));
        p.push(time(&format!("{prefix}_r"), "R", 0.001, 20.0, r));
    }
    p.extend(lfo_params(1, 0.0, 4.5));
    p.extend(lfo_params(2, 1.0, 0.6));
    for n in 1..=4 {
        p.push(ParamInfo::new(
            format!("mod{n}_src").as_str(),
            format!("Quelle {n}"),
            names(&source::NAMES),
            0.0,
        ));
        p.push(ParamInfo::new(
            format!("mod{n}_dst").as_str(),
            format!("Ziel {n}"),
            names(&dest::NAMES),
            0.0,
        ));
        p.push(percent(
            &format!("mod{n}_amt"),
            &format!("Menge {n}"),
            -1.0,
            1.0,
            0.0,
        ));
    }
    p.extend([
        percent("velocity", "Velocity", 0.0, 1.0, 0.6),
        percent("vel_filter", "Vel → Hk.", 0.0, 1.0, 0.3),
        ParamInfo::new("wheel_vib", "Rad → Vibr.", ParamKind::lin(0.0, 200.0), 50.0)
            .formatted(cents_text),
        percent("at_cut", "Druck → Cut", -1.0, 1.0, 0.0),
        ParamInfo::new("glide_legato", "Glide Legato", ParamKind::Bool, 0.0),
        ParamInfo::new("mode", "Modus", names(&MODES), 0.0),
        ParamInfo::new("voices", "Stimmen", ParamKind::Int { min: 1, max: 16 }, 8.0),
        time("glide", "Glide", OFF_S, 5.0, OFF_S).formatted(off_or_seconds),
        ParamInfo::new("bend", "Bend", ParamKind::Int { min: 0, max: 24 }, 2.0)
            .formatted(semitones_text),
        ParamInfo::new("octave", "Okt.", ParamKind::Int { min: -2, max: 2 }, 0.0)
            .formatted(octaves_text),
        ParamInfo::new("unison", "Unison", ParamKind::Int { min: 1, max: 7 }, 1.0),
        ParamInfo::new("detune", "Detune", ParamKind::lin(0.0, 100.0), 14.0)
            .formatted(cents_text)
            .flags(true, true),
        percent("spread", "Spread", 0.0, 1.0, 0.6),
        percent("analog", "Analog", 0.0, 1.0, 0.25),
        ParamInfo::new(
            "volume",
            "Lautstärke",
            ParamKind::scaled(-60.0, 6.0, Scale::Db),
            -6.0,
        )
        .unit(Unit::Db)
        .flags(true, true),
    ]);
    p
}
