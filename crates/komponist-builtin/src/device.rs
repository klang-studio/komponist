//! pisynth's device card (WP-86): the parameters of its layout and the layout itself.
//!
//! The device state stays pisynth's [`Patch`] as JSON (see [`crate::state`]); a card parameter is
//! a *view onto a patch field*: [`Field::get`] reads it, [`Field::set`] writes it, and both the
//! processor (on an `EventKind::Param`) and the app (on the document's copy of the state) run the
//! same `set`, so the running patch and the document stay equal. Nothing in the state changed:
//! projects saved with the 8 macros of the earlier card load as before, and every field the card
//! does not show (filters, effects, LFOs, modulation matrix, …) survives a knob turn.
//!
//! # Wire ids
//!
//! `Command::SetParam::id` / `EventKind::Param::id` of pisynth: 0–7 are the old macros
//! ([`crate::Macro`], unchanged, still understood; the plugin window's parameter view still lists
//! them), 8 and up are the card's own parameters: per operator 14 in the order of
//! [`OpField`] (`8 + 14 · (op − 1) + field`), then the global ones from [`GLOBAL_BASE`]. The card's
//! *Algorithmus* and *Feedback* are the macros of the same name (wire ids 0 and 3, same keys
//! `algorithm` and `feedback`, same semantics), so those two ids mean the same in old and new
//! projects. Model ids ([`ParamId`]) are readable keys and are the stable names.
//!
//! # Parameters
//!
//! | Key | Name | Patch field written by `set` |
//! |---|---|---|
//! | `algorithm` | Algor. | macro Algorithmus (routing of all operators; 0 = free) |
//! | `feedback` | Feedback | macro Feedback (the feedback operator's `feedback`) |
//! | `volume` | Lautstärke | `volume` |
//! | `glide` | Glide | `portamento` |
//! | `voices` | Unison | `unison` (voices per key; pisynth's polyphony is a fixed 16) |
//! | `mono` | Mono | `voice_mode`: on = Mono (Legato stays Legato), off = Poly |
//! | `spread` | Spread | `unison_spread` |
//! | `tune` | Tune | `tune` (semitones, ±24) |
//! | `op{n}_ratio` | Ratio n | `operators[n-1].ratio`, and `pitch_mode = Ratio` |
//! | `op{n}_level` | Op n | `operators[n-1].level` |
//! | `op{n}_attack` / `_decay` / `_release` | Attack n … | `operators[n-1].envelope.attack` / `decay` / `release` |
//! | `op{n}_sustain` | Sustain n | `envelope.sustain` |
//! | `op{n}_delay` / `_hold` | Delay n / Hold n | `envelope.delay` / `hold` |
//! | `op{n}_loop` | Loop n | `envelope.looping` |
//! | `op{n}_attack_curve` / `_decay_curve` / `_release_curve` | Kurve A/D/R n | `envelope.*_curve`, ±1 |
//! | `op{n}_key_scale` | Key Scale n | `key_scaling.right`: ±24 dB per octave above the break point |
//! | `op{n}_output` | Träger n | `operators[n-1].output` (carrier: heard); on the card only the fill of the level fader |
//!
//! Not in the table because `synth-core` has no field for them (only free routes of the modulation
//! matrix, which a card parameter cannot own): per-operator velocity sensitivity and the pitch
//! envelope.
//!
//! Operators are numbered 1–6 as in pisynth's UI. Every `set` ignores non-finite values, clamps to
//! the range, keeps the patch sanitized, allocates nothing and is RT-safe.

use std::sync::OnceLock;

use komponist_model::format::MINUS;
use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, ENV_OP, GraphBinding, GraphConfig, Half, Page, WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};
use synth_core::{
    MAX_KEY_SCALING, MAX_PORTAMENTO, MAX_RATIO, MAX_TIME, MAX_TUNE, MAX_UNISON, OPERATORS, Patch,
    PitchMode, VoiceMode,
};

use crate::algorithms::ALGORITHMS;
use crate::macros::Macro;

/// First wire id of the card's own parameters (0–7 are the macros).
pub const FIRST_WIRE: u32 = 8;

/// Wire id of the first global parameter (after the 6 × [`OpField::COUNT`] operator ones).
pub const GLOBAL_BASE: u32 = FIRST_WIRE + (OPERATORS * OpField::COUNT) as u32;

/// Shortest time of a time parameter (seconds); a delay or hold at or below it is 0.
pub const TIME_MIN: f64 = 0.001;

/// Level scaling of `op{n}_key_scale` at ±1 (dB per octave), pisynth's `MAX_KEY_SCALING`.
const KEY_SCALE_DB: f32 = MAX_KEY_SCALING;

/// A per-operator parameter, in wire order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpField {
    /// Frequency ratio.
    Ratio = 0,
    /// Output level.
    Level,
    /// Envelope attack time.
    Attack,
    /// Envelope decay time.
    Decay,
    /// Envelope sustain level.
    Sustain,
    /// Envelope release time.
    Release,
    /// Envelope delay before the attack.
    Delay,
    /// Envelope hold at full level.
    Hold,
    /// Envelope loop.
    Loop,
    /// Attack curve.
    AttackCurve,
    /// Decay curve.
    DecayCurve,
    /// Release curve.
    ReleaseCurve,
    /// Level scaling above the break point.
    KeyScale,
    /// Whether the operator is heard (a carrier).
    Output,
}

impl OpField {
    /// Number of per-operator parameters.
    pub const COUNT: usize = 14;

    /// All of them in wire order.
    pub const ALL: [OpField; OpField::COUNT] = [
        OpField::Ratio,
        OpField::Level,
        OpField::Attack,
        OpField::Decay,
        OpField::Sustain,
        OpField::Release,
        OpField::Delay,
        OpField::Hold,
        OpField::Loop,
        OpField::AttackCurve,
        OpField::DecayCurve,
        OpField::ReleaseCurve,
        OpField::KeyScale,
        OpField::Output,
    ];

    /// The key after `op{n}_`.
    fn key(self) -> &'static str {
        match self {
            OpField::Ratio => "ratio",
            OpField::Level => "level",
            OpField::Attack => "attack",
            OpField::Decay => "decay",
            OpField::Sustain => "sustain",
            OpField::Release => "release",
            OpField::Delay => "delay",
            OpField::Hold => "hold",
            OpField::Loop => "loop",
            OpField::AttackCurve => "attack_curve",
            OpField::DecayCurve => "decay_curve",
            OpField::ReleaseCurve => "release_curve",
            OpField::KeyScale => "key_scale",
            OpField::Output => "output",
        }
    }

    /// The name before the operator number ("Ratio", "Kurve A").
    fn name(self) -> &'static str {
        match self {
            OpField::Ratio => "Ratio",
            // The level fader of operator n is labelled "Op n" (handoff).
            OpField::Level => "Op",
            OpField::Attack => "Attack",
            OpField::Decay => "Decay",
            OpField::Sustain => "Sustain",
            OpField::Release => "Release",
            OpField::Delay => "Delay",
            OpField::Hold => "Hold",
            OpField::Loop => "Loop",
            OpField::AttackCurve => "Kurve A",
            OpField::DecayCurve => "Kurve D",
            OpField::ReleaseCurve => "Kurve R",
            OpField::KeyScale => "Key Scale",
            OpField::Output => "Träger",
        }
    }
}

/// One parameter of the card: the macro it reuses, an operator field or a global.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// `algorithm` or `feedback`: the macro of that name.
    Macro(Macro),
    /// Master gain.
    Volume,
    /// Portamento time.
    Glide,
    /// Unison voices per key.
    Voices,
    /// Mono instead of poly.
    Mono,
    /// Unison stereo spread.
    Spread,
    /// Master tuning.
    Tune,
    /// An operator field (operator index 0–5).
    Op(usize, OpField),
}

/// The globals in wire order from [`GLOBAL_BASE`].
const GLOBALS: [Field; 6] = [
    Field::Volume,
    Field::Glide,
    Field::Voices,
    Field::Mono,
    Field::Spread,
    Field::Tune,
];

impl Field {
    /// The field with wire id `wire`: a macro (0–7, any of the 8) or one of the card's own.
    pub fn from_wire(wire: u32) -> Option<Field> {
        if let Some(m) = Macro::from_id(wire) {
            return Some(Field::Macro(m));
        }
        let k = wire.checked_sub(FIRST_WIRE)? as usize;
        match k.checked_sub(OPERATORS * OpField::COUNT) {
            None => Some(Field::Op(k / OpField::COUNT, OpField::ALL[k % OpField::COUNT])),
            Some(g) => GLOBALS.get(g).copied(),
        }
    }

    /// The wire id (`Command::SetParam::id`).
    pub fn wire(self) -> u32 {
        match self {
            Field::Macro(m) => m.id(),
            Field::Op(op, f) => FIRST_WIRE + (op * OpField::COUNT + f as usize) as u32,
            g => {
                let at = GLOBALS.iter().position(|x| *x == g).unwrap_or_default();
                GLOBAL_BASE + at as u32
            }
        }
    }

    /// The stable key in the parameter model.
    pub fn key(self) -> String {
        match self {
            Field::Macro(m) => m.key().to_owned(),
            Field::Volume => "volume".into(),
            Field::Glide => "glide".into(),
            Field::Voices => "voices".into(),
            Field::Mono => "mono".into(),
            Field::Spread => "spread".into(),
            Field::Tune => "tune".into(),
            Field::Op(op, f) => format!("op{}_{}", op + 1, f.key()),
        }
    }

    /// The value in `patch`.
    pub fn get(self, patch: &Patch) -> f64 {
        match self {
            Field::Macro(m) => m.get(patch),
            Field::Volume => f64::from(patch.volume),
            Field::Glide => f64::from(patch.portamento),
            Field::Voices => f64::from(patch.unison),
            Field::Mono => f64::from(patch.voice_mode != VoiceMode::Poly),
            Field::Spread => f64::from(patch.unison_spread),
            Field::Tune => f64::from(patch.tune),
            Field::Op(op, f) => {
                let o = &patch.operators[op];
                let e = &o.envelope;
                match f {
                    OpField::Ratio => f64::from(o.ratio),
                    OpField::Level => f64::from(o.level),
                    OpField::Attack => f64::from(e.attack),
                    OpField::Decay => f64::from(e.decay),
                    OpField::Sustain => f64::from(e.sustain),
                    OpField::Release => f64::from(e.release),
                    OpField::Delay => f64::from(e.delay),
                    OpField::Hold => f64::from(e.hold),
                    OpField::Loop => f64::from(e.looping),
                    OpField::AttackCurve => f64::from(e.attack_curve),
                    OpField::DecayCurve => f64::from(e.decay_curve),
                    OpField::ReleaseCurve => f64::from(e.release_curve),
                    OpField::KeyScale => f64::from(o.key_scaling.right),
                    OpField::Output => f64::from(o.output),
                }
            }
        }
    }

    /// Writes `value` (plain, clamped to the range) into `patch`. Non-finite values are ignored.
    /// RT-safe.
    pub fn set(self, patch: &mut Patch, value: f64) {
        if !value.is_finite() {
            return;
        }
        let (min, max) = self.range();
        let v = value.clamp(min, max);
        match self {
            Field::Macro(m) => m.set(patch, value),
            Field::Volume => patch.volume = v as f32,
            Field::Glide => patch.portamento = v as f32,
            Field::Voices => patch.unison = v.round() as u8,
            Field::Mono => {
                patch.voice_mode = match (v >= 0.5, patch.voice_mode) {
                    (false, _) => VoiceMode::Poly,
                    (true, VoiceMode::Poly) => VoiceMode::Mono,
                    (true, keep) => keep,
                }
            }
            Field::Spread => patch.unison_spread = v as f32,
            Field::Tune => patch.tune = v as f32,
            Field::Op(op, f) => {
                let o = &mut patch.operators[op];
                let e = &mut o.envelope;
                let time = |v: f64| if v <= TIME_MIN { 0.0 } else { v as f32 };
                match f {
                    OpField::Ratio => {
                        o.ratio = v as f32;
                        o.pitch_mode = PitchMode::Ratio;
                    }
                    OpField::Level => o.level = v as f32,
                    OpField::Attack => e.attack = v as f32,
                    OpField::Decay => e.decay = v as f32,
                    OpField::Sustain => e.sustain = v as f32,
                    OpField::Release => e.release = v as f32,
                    OpField::Delay => e.delay = time(v),
                    OpField::Hold => e.hold = time(v),
                    OpField::Loop => e.looping = v >= 0.5,
                    OpField::AttackCurve => e.attack_curve = v as f32,
                    OpField::DecayCurve => e.decay_curve = v as f32,
                    OpField::ReleaseCurve => e.release_curve = v as f32,
                    OpField::KeyScale => o.key_scaling.right = v as f32,
                    OpField::Output => o.output = v >= 0.5,
                }
            }
        }
    }

    /// Lowest and highest plain value.
    pub fn range(self) -> (f64, f64) {
        match self {
            Field::Macro(m) => (m.min(), m.max()),
            Field::Volume | Field::Spread => (0.0, 1.0),
            Field::Glide => (0.0, f64::from(MAX_PORTAMENTO)),
            Field::Voices => (1.0, MAX_UNISON as f64),
            Field::Mono => (0.0, 1.0),
            Field::Tune => (-f64::from(MAX_TUNE), f64::from(MAX_TUNE)),
            Field::Op(_, f) => match f {
                OpField::Ratio => (RATIO_MIN, f64::from(MAX_RATIO)),
                OpField::Level | OpField::Sustain => (0.0, 1.0),
                OpField::Attack | OpField::Decay | OpField::Release => {
                    (TIME_MIN, f64::from(MAX_TIME))
                }
                OpField::Delay | OpField::Hold => (TIME_MIN, f64::from(MAX_TIME)),
                OpField::Loop | OpField::Output => (0.0, 1.0),
                OpField::AttackCurve | OpField::DecayCurve | OpField::ReleaseCurve => (-1.0, 1.0),
                OpField::KeyScale => (-f64::from(KEY_SCALE_DB), f64::from(KEY_SCALE_DB)),
            },
        }
    }

    /// The parameter in the model: id, name, kind, default (the "Init" patch's), unit and text.
    pub fn info(self) -> ParamInfo {
        if let Field::Macro(m) = self {
            return m.param_info();
        }
        let (min, max) = self.range();
        // A delay or hold of 0 is the knob's lowest position.
        let default = self.get(&Patch::default()).clamp(min, max);
        let number = match self {
            Field::Op(op, _) => op + 1,
            _ => 0,
        };
        let (name, kind, unit): (String, ParamKind, Unit) = match self {
            Field::Macro(_) => unreachable!("returned above"),
            Field::Volume => ("Lautstärke".into(), ParamKind::lin(min, max), Unit::Percent),
            Field::Glide => ("Glide".into(), ParamKind::lin(min, max), Unit::S),
            Field::Voices => (
                "Unison".into(),
                ParamKind::Int {
                    min: min as i32,
                    max: max as i32,
                },
                Unit::None,
            ),
            Field::Mono => ("Mono".into(), ParamKind::Bool, Unit::None),
            Field::Spread => ("Spread".into(), ParamKind::lin(min, max), Unit::Percent),
            Field::Tune => (
                "Tune".into(),
                ParamKind::scaled(min, max, Scale::Pan),
                Unit::Semitones,
            ),
            Field::Op(_, f) => {
                let name = format!("{} {number}", f.name());
                match f {
                    OpField::Ratio => (name, ParamKind::scaled(min, max, Scale::Log), Unit::None),
                    OpField::Level | OpField::Sustain => {
                        (name, ParamKind::lin(min, max), Unit::Percent)
                    }
                    OpField::Attack | OpField::Decay | OpField::Release => {
                        (name, ParamKind::scaled(min, max, Scale::Log), Unit::S)
                    }
                    OpField::Delay | OpField::Hold => {
                        (name, ParamKind::scaled(min, max, Scale::Log), Unit::S)
                    }
                    OpField::Loop | OpField::Output => (name, ParamKind::Bool, Unit::None),
                    OpField::AttackCurve | OpField::DecayCurve | OpField::ReleaseCurve => {
                        (name, ParamKind::scaled(min, max, Scale::Pan), Unit::Percent)
                    }
                    OpField::KeyScale => (
                        name,
                        ParamKind::scaled(min, max, Scale::Pan),
                        Unit::None,
                    ),
                }
            }
        };
        let info = ParamInfo::new(ParamId::new(self.key()), name, kind, default).unit(unit);
        match self {
            Field::Tune => info.formatted(|v| format!("{} st", signed(v, 1))),
            Field::Op(_, OpField::Ratio) => info.formatted(|v| format!("{v:.2}")),
            Field::Op(_, OpField::Delay | OpField::Hold) => info.formatted(|v| {
                if v <= TIME_MIN {
                    "Aus".to_owned()
                } else if v < 1.0 {
                    format!("{:.0} ms", v * 1000.0)
                } else if v < 10.0 {
                    format!("{v:.2} s")
                } else {
                    format!("{v:.1} s")
                }
            }),
            Field::Op(_, OpField::AttackCurve | OpField::DecayCurve | OpField::ReleaseCurve) => {
                info.formatted(|v| format!("{} %", signed((v * 100.0).round(), 0)))
            }
            Field::Op(_, OpField::KeyScale) => {
                info.formatted(|v| format!("{} dB/Okt", signed(v, 1)))
            }
            _ => info,
        }
    }
}

/// Lowest ratio of the knob (a patch may hold less, down to 0; it shows at this end).
pub const RATIO_MIN: f64 = 0.25;

/// `+1.5`, `−3.0`, `0.0`: one decimal with the real minus.
fn signed(v: f64, decimals: usize) -> String {
    let text = format!("{:.*}", decimals, v.abs());
    let zero = text.bytes().all(|b| b == b'0' || b == b'.');
    match v {
        v if v > 0.0 && !zero => format!("+{text}"),
        v if v < 0.0 && !zero => format!("{MINUS}{text}"),
        _ => text,
    }
}

/// One parameter of the card with its wire id.
#[derive(Clone, Debug)]
pub struct CardParam {
    /// `Command::SetParam::id`.
    pub wire: u32,
    /// The field it views.
    pub field: Field,
    /// Its description in the parameter model.
    pub info: ParamInfo,
}

/// The card's parameters: the globals (Algorithmus and Feedback first), then the 14 of every
/// operator.
pub fn params() -> &'static [CardParam] {
    static PARAMS: OnceLock<Vec<CardParam>> = OnceLock::new();
    PARAMS.get_or_init(|| {
        let globals = [Field::Macro(Macro::Algorithm), Field::Macro(Macro::Feedback)]
            .into_iter()
            .chain(GLOBALS);
        let ops = (0..OPERATORS)
            .flat_map(|op| OpField::ALL.into_iter().map(move |f| Field::Op(op, f)));
        globals
            .chain(ops)
            .map(|field| CardParam {
                wire: field.wire(),
                field,
                info: field.info(),
            })
            .collect()
    })
}

/// The parameter with model id `id`.
pub fn param(id: &ParamId) -> Option<&'static CardParam> {
    params().iter().find(|p| p.info.id == *id)
}

/// Whether operator `op` (0–5) is a carrier of algorithm `number` (1–32).
pub fn is_carrier(number: usize, op: usize) -> bool {
    ALGORITHMS
        .get(number.wrapping_sub(1))
        .is_some_and(|a| a.is_carrier(op))
}

fn id(key: &str) -> ParamId {
    ParamId::new(key)
}

/// The layout of the card: Operatoren (10 columns), Hüllkurven (8), LFO and Global (empty).
///
/// *Operatoren:* per operator a column with the ratio knob over the level fader (the fader fills in
/// the track colour for a carrier and `text.2` for a modulator, from the operator's `output`
/// flag); a click anywhere in a column selects the operator, which the Hüllkurven page then
/// shows (`{env_op}`). Next to them the algorithm display (also selecting), the algorithm number,
/// feedback, volume (large), glide, unison voices over mono, spread and master tune.
///
/// *Hüllkurven:* the envelope graph of the selected operator with its attack, decay, sustain, release,
/// attack curve and key scale knobs below; on the right the operator switch, loop, delay, hold and
/// the decay and release curves. The handoff's velocity and pitch envelope cells are left
/// out, `synth-core` has no field for them.
pub fn layout() -> DeviceLayout {
    let select = || CellParam::Select {
        key: ENV_OP,
        count: OPERATORS as u8,
    };
    let mut cells = Vec::new();
    // The overlay first: it lies below the knobs and faders that cover it.
    cells.push(Cell::span(
        0,
        0,
        OPERATORS as u8,
        3,
        WidgetKind::ColumnSelect,
        select(),
    ));
    for op in 1..=OPERATORS {
        let col = (op - 1) as u8;
        cells.push(Cell::knob(col, 0, id(&format!("op{op}_ratio"))));
        cells.push(
            Cell::span(
                col,
                1,
                1,
                2,
                WidgetKind::MiniFader,
                CellParam::Id(id(&format!("op{op}_level"))),
            )
            .carrier(id(&format!("op{op}_output"))),
        );
    }
    cells.push(Cell::span(6, 0, 2, 2, WidgetKind::FmAlgorithm, CellParam::Id(id("algorithm"))));
    cells.push(Cell::half(
        6,
        2,
        1,
        Half::Top,
        WidgetKind::Number,
        CellParam::Id(id("algorithm")),
    ));
    cells.push(Cell::knob(7, 2, id("feedback")));
    cells.push(Cell::span(8, 0, 2, 1, WidgetKind::KnobLarge, CellParam::Id(id("volume"))));
    cells.push(Cell::knob(8, 1, id("glide")));
    cells.push(Cell::half(
        9,
        1,
        1,
        Half::Top,
        WidgetKind::Number,
        CellParam::Id(id("voices")),
    ));
    cells.push(Cell::half(
        9,
        1,
        1,
        Half::Bottom,
        WidgetKind::ToggleButton,
        CellParam::Id(id("mono")),
    ));
    cells.push(Cell::knob(8, 2, id("tune")));
    cells.push(Cell::knob(9, 2, id("spread")));
    let operators = Page {
        name: "Operatoren".into(),
        cols: 10,
        cells,
        dividers: vec![5, 7],
    };

    let env = |key: &str| CellParam::Template(format!("op{{{ENV_OP}}}_{key}"));
    let tknob = |col: u8, row: u8, key: &str| {
        Cell::span(col, row, 1, 1, WidgetKind::Knob, env(key))
    };
    let mut cells = vec![Cell::graph(
        0,
        0,
        6,
        2,
        GraphBinding::new(GraphConfig::Envelope)
            .titled(format!("Op {{{ENV_OP}}} · Amplitude"))
            .bind("attack", format!("op{{{ENV_OP}}}_attack"))
            .bind("decay", format!("op{{{ENV_OP}}}_decay"))
            .bind("sustain", format!("op{{{ENV_OP}}}_sustain"))
            .bind("release", format!("op{{{ENV_OP}}}_release")),
    )];
    for (col, key) in [
        (0, "attack"),
        (1, "decay"),
        (2, "sustain"),
        (3, "release"),
        (4, "attack_curve"),
        (5, "key_scale"),
    ] {
        cells.push(tknob(col, 2, key));
    }
    // The operator switch sits over the right-hand group: the grid has no half-row offset to put
    // it above the graph as the handoff does.
    cells.push(Cell::half(6, 0, 2, Half::Top, WidgetKind::Segment, select()));
    cells.push(Cell::half(6, 0, 2, Half::Bottom, WidgetKind::ToggleButton, env("loop")));
    cells.push(tknob(6, 1, "delay"));
    cells.push(tknob(7, 1, "hold"));
    cells.push(tknob(6, 2, "decay_curve"));
    cells.push(tknob(7, 2, "release_curve"));
    let envelopes = Page {
        name: "Hüllkurven".into(),
        cols: 8,
        cells,
        dividers: vec![5],
    };

    let empty = |name: &str| Page {
        name: name.into(),
        cols: 8,
        cells: Vec::new(),
        dividers: Vec::new(),
    };
    DeviceLayout {
        pages: vec![operators, envelopes, empty("LFO"), empty("Global")],
    }
}
