//! Delay: a stereo echo with time and feedback on the XY field, tempo sync, ping-pong, low and high
//! cut in the feedback loop, width and mix (WP-90).
//!
//! Two delay lines (4 s) read with Hermite interpolation. The time (free in milliseconds, or a note
//! value of the transport tempo) glides with a 60 ms time constant and at most 0.5 samples per sample,
//! so a change bends the echoes in pitch like a tape delay (at most an octave) instead of clicking,
//! and a tempo change follows the same way. The
//! feedback passes a one-pole high-pass (`low_cut`) and low-pass (`high_cut`) on every repeat;
//! the first echo is unfiltered. Ping-pong is a smoothed cross-feedback: at 100 % the stereo input
//! enters the left line as a mono sum and the repeats bounce right, left, right, … `width` scales
//! the side of the wet signal (0 mono, 100 % as the lines produce it). At 0 % mix the audio
//! passes bit-exact.

use komponist_i18n::Lang;
use komponist_model::params::layout::{
    Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half, WidgetKind,
};
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};

use komponist_device_core::dsp::{
    CONTROL, Glide, OnePole, Ring, finite, flush, pole, smooth_coeff,
};
use komponist_device_core::layout::{knob, single_page, toggle};
use komponist_device_core::{Dsp, INITIAL_RATE, Preset, Spec};

/// The device.
pub static SPEC: Spec = Spec {
    catalog: komponist_device_core::CatalogSpec {
        id: "delay",
        name: "Delay",
        name_in: Lang::device_name_delay,
        role: komponist_model::DeviceRole::Effect,
        features: &["audio-effect", "delay", "echo"],
        vendor: "komponist",
        sidechain: false,
    },
    params,
    layout,
    dsp,
    presets,
    event_stage: None,
    preset_aliases: &[
        ("Achtel Ping-Pong", "Eighth Ping-Pong"),
        ("Punktierte Achtel", "Dotted Eighths"),
        ("Dub-Echo", "Dub Echo"),
        ("Weiches Band", "Soft Tape"),
    ],
    migrate_params: None,
    cache: komponist_device_core::SpecCache::new(),
};

/// Longest delay time in milliseconds.
pub const MAX_TIME_MS: f64 = 2000.0;

/// Highest feedback; below 1 so the echo always dies out.
pub const MAX_FEEDBACK: f64 = 0.95;

/// Longest delay the lines hold (a whole note at 60 BPM), in milliseconds.
pub const LINE_MS: f64 = 4000.0;

/// Note values of the sync, in quarter notes: `1/32` … `1/1`, with triplets (`T`) and dotted (`.`).
pub const NOTES: [(&str, f64); 13] = [
    ("1/32", 0.125),
    ("1/16T", 1.0 / 6.0),
    ("1/16", 0.25),
    ("1/16.", 0.375),
    ("1/8T", 1.0 / 3.0),
    ("1/8", 0.5),
    ("1/8.", 0.75),
    ("1/4T", 2.0 / 3.0),
    ("1/4", 1.0),
    ("1/4.", 1.5),
    ("1/2", 2.0),
    ("1/2.", 3.0),
    ("1/1", 4.0),
];

/// Wire ids (the first four are the placeholder's, so saved states keep working).
const TIME: usize = 0;
const FEEDBACK: usize = 1;
const PING_PONG: usize = 2;
const MIX: usize = 3;
const SYNC: usize = 4;
const NOTE: usize = 5;
const LOW_CUT: usize = 6;
const HIGH_CUT: usize = 7;
const WIDTH: usize = 8;

/// The factory presets (WP-96): a small starter set.
fn presets() -> &'static [Preset] {
    static LIST: [Preset; 5] = [
        Preset::new(
            "Slapback",
            &[
                ("time", 110.0),
                ("feedback", 0.1),
                ("ping_pong", 0.0),
                ("mix", 0.25),
            ],
        ),
        Preset::new(
            "Eighth Ping-Pong",
            &[("sync", 1.0), ("note", 5.0), ("feedback", 0.4)],
        ),
        Preset::new(
            "Dotted Eighths",
            &[
                ("sync", 1.0),
                ("note", 6.0),
                ("ping_pong", 0.0),
                ("feedback", 0.45),
            ],
        ),
        Preset::new(
            "Dub Echo",
            &[
                ("sync", 1.0),
                ("note", 8.0),
                ("feedback", 0.7),
                ("low_cut", 200.0),
                ("high_cut", 3500.0),
                ("mix", 0.35),
            ],
        ),
        Preset::new(
            "Soft Tape",
            &[
                ("time", 300.0),
                ("feedback", 0.35),
                ("ping_pong", 0.0),
                ("low_cut", 120.0),
                ("high_cut", 4500.0),
                ("mix", 0.2),
                ("width", 0.6),
            ],
        ),
    ];
    &LIST
}

fn params(t: Lang) -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(
            "time",
            t.param_time(),
            ParamKind::scaled(1.0, MAX_TIME_MS, Scale::Log),
            375.0,
        )
        .unit(Unit::Ms),
        ParamInfo::new(
            "feedback",
            t.param_feedback(),
            ParamKind::lin(0.0, MAX_FEEDBACK),
            0.42,
        )
        .unit(Unit::Percent),
        ParamInfo::new("ping_pong", t.param_delay_ping_pong(), ParamKind::Bool, 1.0),
        ParamInfo::new("mix", t.param_mix(), ParamKind::lin(0.0, 1.0), 0.3).unit(Unit::Percent),
        ParamInfo::new("sync", t.param_sync(), ParamKind::Bool, 0.0),
        ParamInfo::new(
            "note",
            t.param_delay_note(),
            ParamKind::Enum(NOTES.iter().map(|(n, _)| (*n).to_owned()).collect()),
            8.0,
        ),
        ParamInfo::new(
            "low_cut",
            t.param_low_cut(),
            ParamKind::scaled(20.0, 2000.0, Scale::Log),
            20.0,
        )
        .unit(Unit::Hz),
        ParamInfo::new(
            "high_cut",
            t.param_high_cut(),
            ParamKind::scaled(500.0, 20_000.0, Scale::Log),
            20_000.0,
        )
        .unit(Unit::Hz),
        ParamInfo::new("width", t.param_width(), ParamKind::lin(0.0, 1.0), 1.0).unit(Unit::Percent),
    ]
}

/// Four columns: the XY field (time × feedback) with ping-pong, sync and the note value below it;
/// time and feedback, the two cuts in the loop, width and mix as knobs.
fn layout(_: Lang) -> DeviceLayout {
    single_page(
        4,
        vec![
            Cell::graph(
                0,
                0,
                2,
                2,
                GraphBinding::new(GraphConfig::Xy)
                    .bind("x", "time")
                    .bind("y", "feedback"),
            ),
            toggle(0, 2, 1, Half::Top, CellParam::Id(ParamId::new("ping_pong"))),
            toggle(1, 2, 1, Half::Top, CellParam::Id(ParamId::new("sync"))),
            Cell::half(
                0,
                2,
                2,
                Half::Bottom,
                WidgetKind::Dropdown,
                CellParam::Id(ParamId::new("note")),
            ),
            knob(2, 0, "time"),
            knob(3, 0, "feedback"),
            knob(2, 1, "low_cut"),
            knob(3, 1, "high_cut"),
            knob(2, 2, "width"),
            knob(3, 2, "mix"),
        ],
    )
}

/// Seconds the delay time takes to cover 63 % of a change.
const TIME_GLIDE: f32 = 0.060;
/// Fastest change of the delay time in samples per sample: the echoes bend by at most an octave
/// down or half an octave up while the time moves.
const MAX_SLEW: f32 = 0.5;
/// Seconds of the other smoothers (feedback, mix, width, ping-pong, cuts).
const GLIDE: f32 = 0.020;

struct Delay {
    rate: f32,
    lines: [Ring; 2],
    /// Delay in samples, and the poles of its glide and of the others'.
    time: Glide,
    time_coeff: f32,
    glide: f32,
    feedback: Glide,
    ping_pong: Glide,
    mix: Glide,
    width: Glide,
    /// Cut poles (`exp(-2π f / rate)`) as set.
    low_cut_hz: Glide,
    high_cut_hz: Glide,
    low_pole: f32,
    high_pole: f32,
    /// Parameters as set.
    free_ms: f64,
    sync: bool,
    note: usize,
    bpm: f64,
    /// Whether the transport tempo has arrived since activation: the first one is taken at once
    /// (no glide from the 120 BPM the device starts with).
    have_tempo: bool,
    hp: [OnePole; 2],
    lp: [OnePole; 2],
    countdown: usize,
}

fn dsp() -> Box<dyn Dsp> {
    let mut d = Delay {
        rate: INITIAL_RATE,
        lines: [Ring::default(), Ring::default()],
        time: Glide::new(1.0),
        time_coeff: 0.0,
        glide: 0.0,
        feedback: Glide::new(0.0),
        ping_pong: Glide::new(1.0),
        mix: Glide::new(0.3),
        width: Glide::new(1.0),
        low_cut_hz: Glide::new(20.0),
        high_cut_hz: Glide::new(20_000.0),
        low_pole: 0.0,
        high_pole: 0.0,
        free_ms: 375.0,
        sync: false,
        note: 8,
        bpm: 120.0,
        have_tempo: false,
        hp: [OnePole::default(); 2],
        lp: [OnePole::default(); 2],
        countdown: 0,
    };
    d.allocate();
    Box::new(d)
}

impl Delay {
    fn allocate(&mut self) {
        let len = (f64::from(self.rate) * LINE_MS / 1000.0) as usize + 8;
        self.lines = [Ring::with_len(len), Ring::with_len(len)];
        self.time_coeff = smooth_coeff(TIME_GLIDE, CONTROL, self.rate);
        self.glide = smooth_coeff(GLIDE, CONTROL, self.rate);
        self.retarget();
        self.snap();
    }

    /// The delay in samples the parameters ask for now.
    fn retarget(&mut self) {
        let ms = if self.sync {
            NOTES[self.note.min(NOTES.len() - 1)].1 * 60_000.0 / self.bpm
        } else {
            self.free_ms
        };
        let max = (self.lines[0].max_delay() as f32 - 2.0).max(2.0);
        self.time.target = ((ms * f64::from(self.rate) / 1000.0) as f32).clamp(2.0, max);
    }

    fn snap(&mut self) {
        self.time.snap();
        self.feedback.snap();
        self.ping_pong.snap();
        self.mix.snap();
        self.width.snap();
        self.low_cut_hz.snap();
        self.high_cut_hz.snap();
        self.low_pole = pole(self.low_cut_hz.next, self.rate);
        self.high_pole = pole(self.high_cut_hz.next, self.rate);
        self.countdown = 0;
    }

    fn control(&mut self) {
        self.time.start_limited(self.time_coeff, 0.01, MAX_SLEW);
        let g = self.glide;
        self.feedback.start(g, 1e-5);
        self.ping_pong.start(g, 1e-4);
        self.mix.start(g, 1e-5);
        self.width.start(g, 1e-4);
        self.low_cut_hz.start(g, 1e-2);
        self.high_cut_hz.start(g, 1e-1);
        self.low_pole = pole(self.low_cut_hz.next, self.rate);
        self.high_pole = pole(self.high_cut_hz.next, self.rate);
    }
}

impl Dsp for Delay {
    fn activate(&mut self, sample_rate: f32) {
        self.rate = sample_rate;
        self.have_tempo = false;
        self.allocate();
    }

    fn set_param(&mut self, index: usize, value: f64) {
        match index {
            TIME => {
                self.free_ms = value;
                self.retarget();
            }
            FEEDBACK => self.feedback.target = value as f32,
            PING_PONG => self.ping_pong.target = if value >= 0.5 { 1.0 } else { 0.0 },
            MIX => self.mix.target = value as f32,
            SYNC => {
                self.sync = value >= 0.5;
                self.retarget();
            }
            NOTE => {
                self.note = value.round().max(0.0) as usize;
                self.retarget();
            }
            LOW_CUT => self.low_cut_hz.target = value as f32,
            HIGH_CUT => self.high_cut_hz.target = value as f32,
            WIDTH => self.width.target = value as f32,
            _ => {}
        }
    }

    fn set_tempo(&mut self, bpm: f64) {
        let bpm = if bpm.is_finite() {
            bpm.clamp(20.0, 999.0)
        } else {
            120.0
        };
        let first = !self.have_tempo;
        self.have_tempo = true;
        if bpm != self.bpm {
            self.bpm = bpm;
            if self.sync {
                self.retarget();
                if first {
                    self.time.snap();
                }
            }
        }
    }

    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]) {
        let [il, ir] = input;
        let [ol, or] = output;
        for n in 0..il.len() {
            if self.countdown == 0 {
                self.control();
                self.countdown = CONTROL;
            }
            let done = CONTROL - self.countdown;
            self.countdown -= 1;
            let (xl, xr) = (finite(il[n]), finite(ir[n]));
            let delay = self.time.at(done).max(2.0);
            let wl = self.lines[0].read_hermite(delay);
            let wr = self.lines[1].read_hermite(delay);
            // The repeats: high-pass (low cut), then low-pass (high cut).
            let fl = self.lp[0].lp(self.hp[0].hp(wl, self.low_pole), self.high_pole);
            let fr = self.lp[1].lp(self.hp[1].hp(wr, self.low_pole), self.high_pole);
            let pp = self.ping_pong.at(done);
            let mono = 0.5 * (xl + xr);
            let fb = self.feedback.at(done);
            let cross_l = fl + (fr - fl) * pp;
            let cross_r = fr + (fl - fr) * pp;
            let in_l = xl + (mono - xl) * pp;
            let in_r = xr * (1.0 - pp);
            self.lines[0].push(flush(in_l + cross_l * fb));
            self.lines[1].push(flush(in_r + cross_r * fb));
            // Width of the wet signal (mid/side).
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * self.width.at(done);
            let mix = self.mix.at(done);
            ol[n] = xl + (mid + side - xl) * mix;
            or[n] = xr + (mid - side - xr) * mix;
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
        for f in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            f.reset();
        }
        self.have_tempo = false;
        self.snap();
    }
}

#[cfg(test)]
mod tests;
