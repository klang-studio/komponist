//! Section 05 "Spur, Clip, Note" of the gallery: every timeline element in every state of the
//! design system (`headers` 12, `clips` 12, `notes` 11), built from the design data.
//!
//! [`header_demos`], [`clip_demos`] and [`note_demos`] list the states with their labels; the
//! `paint_*_demo` functions paint one state at a position and register AccessKit nodes for the
//! painted objects, as a canvas would. The card contents ([`header_card`], [`clip_card`],
//! [`note_card`]) lay all states out like the design page; the snapshot tests render each state
//! on its own.

use egui::{Color32, Id, Rect, Sense, Ui, Vec2, pos2, vec2};
use komponist_model::{
    Meter, Note, NoteId, Step, StepClip, StepResolution, StepRow, Tick, project::MAX_STEPS,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, size, space};

use super::clip::{ClipEdge, ClipModel, ClipState, PreviewMesh, PreviewSpec};
use super::note::{NoteBatch, NoteState, note_access};
use super::track_header::HeaderModel;
use super::{clip, track_header};
use crate::Lang;
use crate::state::State;
use crate::widgets::fader;
use crate::widgets::meter::MeterState;
use crate::widgets::metrics::timeline as metrics;
use crate::{fonts, px};

/// The demo hue of the design system (`HUE = 150`, green).
const HUE: u16 = 150;
/// Demo notes (`NOTES`): start and length in 1/16 steps, pitch, velocity; two bars.
const NOTES: [[i64; 4]; 17] = [
    [0, 8, 60, 70],
    [0, 8, 64, 62],
    [0, 2, 67, 100],
    [2, 2, 69, 80],
    [4, 4, 71, 112],
    [8, 8, 62, 58],
    [8, 2, 69, 72],
    [10, 2, 67, 60],
    [12, 4, 64, 96],
    [16, 8, 57, 82],
    [16, 8, 60, 74],
    [16, 6, 72, 124],
    [22, 2, 71, 76],
    [24, 8, 55, 64],
    [24, 8, 59, 58],
    [24, 4, 67, 90],
    [28, 4, 69, 48],
];
/// Demo step pattern (`STEPS`): the `on` steps of four rows, 32 steps.
const STEPS: [&[usize]; 4] = [
    &[0, 6, 8, 14, 16, 22, 24, 30],
    &[4, 12, 20, 28],
    &[2, 6, 10, 14, 18, 22, 26, 30],
    &[12, 28, 31],
];
/// Length of the demo content: two 4/4 bars.
const DEMO_BARS: i64 = 2;
const METER: Meter = Meter::FOUR_FOUR;

fn demo_ticks() -> Tick {
    Tick(DEMO_BARS * METER.ticks_per_bar())
}

/// The demo notes as model notes.
pub fn demo_notes() -> Vec<Note> {
    NOTES
        .iter()
        .enumerate()
        .map(|(i, &[s, l, p, v])| Note {
            id: NoteId(i as u64 + 1),
            start: Tick::from_steps(s),
            length: Tick::from_steps(l),
            pitch: p as u8,
            velocity: v as u8,
        })
        .collect()
}

/// The demo step pattern: four rows, 32 sixteenths.
pub fn demo_steps() -> StepClip {
    let rows = STEPS
        .iter()
        .enumerate()
        .map(|(r, on)| {
            let mut steps = [Step::default(); MAX_STEPS];
            for &i in *on {
                steps[i].on = true;
            }
            StepRow {
                name: format!("Pad {}", r + 1),
                note: 36 + r as u8,
                mute: false,
                solo: false,
                steps,
            }
        })
        .collect();
    StepClip {
        rows,
        length_steps: MAX_STEPS as u8,
        resolution: StepResolution::Sixteenth,
        swing: 0.0,
    }
}

/// Space between a sample and its label (`gap:8px`).
const LABEL_GAP: f32 = space::SPACE_2;

/// Width of a state label.
fn label_width(ui: &Ui, text: &str) -> f32 {
    let style = typography::VALUE_S;
    ui.painter()
        .layout_job(fonts::layout_job(text, &style, Color32::PLACEHOLDER))
        .size()
        .x
}

/// Paints a state label (`value.s`, `text.3`) with its top left at `pos`; returns its height.
fn label(ui: &Ui, pos: egui::Pos2, text: &str) -> f32 {
    let style = typography::VALUE_S;
    let galley =
        ui.painter()
            .layout_job(fonts::layout_job(text, &style, fonts::style_color(&style)));
    let h = galley.size().y;
    ui.painter().galley(pos, galley, Color32::PLACEHOLDER);
    h
}

/// Registers an AccessKit node for a painted object (as the canvases do).
fn register(ui: &Ui, id: Id, rect: Rect, access: super::Access) {
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| access.widget_info());
}

/// Lays `items` out left to right, wrapping, with `gap` (column, row) between them, aligned to the
/// bottom of their row when `bottom` is set; then paints each with `paint(ui, rect, item)`.
fn flow<T>(
    ui: &mut Ui,
    items: &[T],
    gap: Vec2,
    bottom: bool,
    size: impl Fn(&Ui, &T) -> Vec2,
    mut paint: impl FnMut(&Ui, Rect, &T),
) {
    let width = ui.available_width();
    let origin = ui.cursor().min;
    let mut rects = Vec::with_capacity(items.len());
    let (mut x, mut y, mut row_h) = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut row: Vec<(usize, Vec2)> = Vec::new();
    let place =
        |row: &mut Vec<(usize, Vec2)>, y: f32, row_h: f32, rects: &mut Vec<(usize, Rect)>| {
            let mut x = 0.0;
            for (i, s) in row.drain(..) {
                let top = if bottom { y + row_h - s.y } else { y };
                rects.push((i, Rect::from_min_size(origin + vec2(x, top), s)));
                x += s.x + gap.x;
            }
        };
    for (i, item) in items.iter().enumerate() {
        let s = size(ui, item);
        if x > 0.0 && x + s.x > width {
            place(&mut row, y, row_h, &mut rects);
            y += row_h + gap.y;
            x = 0.0;
            row_h = 0.0;
        }
        row.push((i, s));
        x += s.x + gap.x;
        row_h = row_h.max(s.y);
    }
    place(&mut row, y, row_h, &mut rects);
    let total = Rect::from_min_size(origin, vec2(width, y + row_h));
    ui.allocate_rect(total, Sense::hover());
    rects.sort_by_key(|(i, _)| *i);
    for (i, r) in rects {
        paint(ui, r, &items[i]);
    }
}

// ---------------------------------------------------------------------------------------------
// Track headers

/// One header of a header state.
#[derive(Clone, Debug)]
pub struct HeaderRow {
    /// Track name.
    pub name: &'static str,
    /// Instrument line.
    pub instrument: &'static str,
    /// The instrument line is an error.
    pub error: bool,
    /// Track hue.
    pub hue: u16,
    /// Header state.
    pub state: State,
    /// M S R I.
    pub toggles: [bool; 4],
    /// Muted by solo elsewhere.
    pub implied: bool,
    /// Level and held peak of the left channel as shares of the meter height (`hdr` `l`, `pk`);
    /// the right one follows the design data (× 0.94 / × 0.97).
    pub meter: (f32, f32),
}

/// A header state of the design system: label, file slug, headers.
#[derive(Clone, Debug)]
pub struct HeaderDemo {
    /// Label under the sample (design data).
    pub label: &'static str,
    /// Name for snapshot files.
    pub slug: &'static str,
    /// The headers, top to bottom.
    pub rows: Vec<HeaderRow>,
}

fn row(name: &'static str, instrument: &'static str, hue: u16) -> HeaderRow {
    HeaderRow {
        name,
        instrument,
        error: false,
        hue,
        state: State::REST,
        toggles: [false; 4],
        implied: false,
        meter: (0.0, 0.0),
    }
}

/// The 12 header states of `headers`.
pub fn header_demos() -> Vec<HeaderDemo> {
    let lead = || HeaderRow {
        meter: (0.5, 0.6),
        ..row("Lead", "pisynth", HUE)
    };
    let with = |state| HeaderRow { state, ..lead() };
    let demo = |label, slug, rows| HeaderDemo { label, slug, rows };
    vec![
        demo("standard", "standard", vec![lead()]),
        demo("hover", "hover", vec![with(State::HOVERED)]),
        demo(
            "ausgewählt · accent.surface",
            "selected",
            vec![with(State::SELECTED)],
        ),
        demo("fokus · Tastatur", "focus", vec![with(State::FOCUSED)]),
        demo(
            "mehrfach ausgewählt",
            "multi-selected",
            vec![
                with(State::SELECTED),
                HeaderRow {
                    state: State::SELECTED,
                    meter: (0.62, 0.7),
                    ..row("Bass", "pisynth", 250)
                },
            ],
        ),
        demo(
            "aufnahmebereit",
            "armed",
            vec![HeaderRow {
                toggles: [false, false, true, true],
                meter: (0.3, 0.4),
                ..lead()
            }],
        ),
        demo(
            "solo",
            "solo",
            vec![HeaderRow {
                toggles: [false, true, false, false],
                meter: (0.7, 0.78),
                ..row("Drums", "klopfer", 45)
            }],
        ),
        demo(
            "stumm",
            "muted",
            vec![HeaderRow {
                toggles: [true, false, false, false],
                ..row("Pad", "pisynth", 300)
            }],
        ),
        demo(
            "stumm durch Solo woanders",
            "implied-mute",
            vec![HeaderRow {
                implied: true,
                ..row("Pad", "stumm durch Solo", 300)
            }],
        ),
        demo(
            "pegel clipping",
            "clipping",
            vec![HeaderRow {
                meter: (1.0, 1.0),
                ..row("Drums", "klopfer", 45)
            }],
        ),
        demo(
            "ohne Instrument · leer",
            "empty",
            vec![row("MIDI 3", "Instrument hierher ziehen", 225)],
        ),
        demo(
            "plugin fehlt",
            "plugin-missing",
            vec![HeaderRow {
                error: true,
                ..row("Strings", "Plugin fehlt: Vital", 325)
            }],
        ),
    ]
}

/// A still meter from the design's shares of the meter height (`meterH(l, pk)`, right channel
/// `l × .94`, `pk × .97`), read on the fader taper the meter uses. A peak at the top (+6 dB) has
/// clipped.
pub fn demo_meter(level: f32, peak: f32) -> MeterState {
    let db = |share: f32| fader::p_to_db(1.0 - share);
    MeterState::frozen([db(level), db(level * 0.94)], [db(peak), db(peak * 0.97)])
}

fn header_size(rows: usize) -> Vec2 {
    vec2(size::TRACK_HEADER, rows as f32 * size::track::DEFAULT)
}

/// Size of a header state sample with its label (a flex column: as wide as the wider of both).
pub fn header_demo_size(ui: &Ui, demo: &HeaderDemo) -> Vec2 {
    let h = header_size(demo.rows.len());
    vec2(
        h.x.max(label_width(ui, demo.label)),
        h.y + LABEL_GAP + typography::VALUE_S.line_height,
    )
}

/// Paints the headers of `demo` from `rect.min` (without label) and registers their nodes.
pub fn paint_header_demo(ui: &Ui, rect: Rect, demo: &HeaderDemo) {
    for (i, r) in demo.rows.iter().enumerate() {
        let palette = TrackPalette::new(r.hue);
        let meter = demo_meter(r.meter.0, r.meter.1);
        let model = HeaderModel {
            instrument_error: r.error,
            mute: r.toggles[0],
            solo: r.toggles[1],
            arm: r.toggles[2],
            monitor: r.toggles[3],
            implied_mute: r.implied,
            meter: Some(&meter),
            ..HeaderModel::new(r.name, r.instrument, &palette)
        };
        let hr = Rect::from_min_size(
            rect.min + vec2(0.0, i as f32 * size::track::DEFAULT),
            header_size(1),
        );
        track_header::paint_track_header(ui.painter(), hr, &model, r.state);
        register(
            ui,
            Id::unique(("timeline-header", demo.slug, i)),
            hr,
            track_header::header_access(Lang::De, &model, r.state),
        );
    }
}

/// The header card: all 12 states (`grid-template-columns: repeat(auto-fill, 224px)`, gap 20 24).
pub fn header_card(ui: &mut Ui) {
    let demos = header_demos();
    flow(
        ui,
        &demos,
        vec2(space::SPACE_6, space::SPACE_4 + space::SPACE_1),
        false,
        header_demo_size,
        |ui, r, d| {
            paint_header_demo(ui, r, d);
            let h = header_size(d.rows.len()).y;
            label(ui, r.min + vec2(0.0, h + LABEL_GAP), d.label);
        },
    );
}

// ---------------------------------------------------------------------------------------------
// Clips

/// Content of a demo clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoContent {
    /// The demo notes.
    Midi,
    /// The demo step pattern (hue 45, as `Beat 1`).
    Steps,
    /// No content.
    Empty,
}

/// One clip of a clip state (`clipO` options).
#[derive(Clone, Debug)]
pub struct DemoClip {
    /// Head text.
    pub name: &'static str,
    /// Track hue.
    pub hue: u16,
    /// Content.
    pub content: DemoContent,
    /// x in the sample (`x`).
    pub x: f32,
    /// Width (`w`, default 180).
    pub w: f32,
    /// Width of one content pass, i.e. two bars (`P`, default `w`).
    pub pass: f32,
    /// The clip loops one pass (two bars) until its end.
    pub looped: bool,
    /// Muted.
    pub muted: bool,
    /// Recording.
    pub recording: bool,
    /// State.
    pub state: ClipState,
}

/// A clip state: label, slug, sample width, clips, and the dashed original (Alt copy).
#[derive(Clone, Debug)]
pub struct ClipDemo {
    /// Label under the sample.
    pub label: &'static str,
    /// Name for snapshot files.
    pub slug: &'static str,
    /// Width of the sample (`CV` `w`, default 180).
    pub width: f32,
    /// The clips.
    pub clips: Vec<DemoClip>,
    /// The dashed original at `0..180` (Alt-drag copy).
    pub ghost: bool,
}

/// Default clip width in the samples (`clipO`: `W = 180`).
const CLIP_W: f32 = 180.0;
/// Clip height in the samples (`Hh = 50`): a default track of 56 minus 6.
fn clip_h() -> f32 {
    size::track::DEFAULT - 2.0 * metrics::CLIP_INSET_Y
}

fn lead() -> DemoClip {
    DemoClip {
        name: "Lead A",
        hue: HUE,
        content: DemoContent::Midi,
        x: 0.0,
        w: CLIP_W,
        pass: CLIP_W,
        looped: false,
        muted: false,
        recording: false,
        state: ClipState::REST,
    }
}

fn beat() -> DemoClip {
    DemoClip {
        name: "Beat 1",
        hue: 45,
        content: DemoContent::Steps,
        ..lead()
    }
}

/// The 12 clip states of `clips`.
pub fn clip_demos() -> Vec<ClipDemo> {
    let demo = |label, slug, clips| ClipDemo {
        label,
        slug,
        width: CLIP_W,
        clips,
        ghost: false,
    };
    let sel = ClipState::SELECTED;
    vec![
        demo("midi · standard", "midi", vec![lead()]),
        demo("step-pattern", "steps", vec![beat()]),
        demo(
            "hover · Griffe kürzen, loopen",
            "hover",
            vec![DemoClip {
                state: ClipState::HOVERED,
                ..lead()
            }],
        ),
        demo(
            "ausgewählt",
            "selected",
            vec![DemoClip {
                state: sel,
                ..lead()
            }],
        ),
        ClipDemo {
            width: 244.0,
            ..demo(
                "mehrfach ausgewählt",
                "multi-selected",
                vec![
                    DemoClip {
                        state: sel,
                        w: 120.0,
                        pass: 120.0,
                        ..lead()
                    },
                    DemoClip {
                        state: sel,
                        x: 124.0,
                        w: 120.0,
                        pass: 120.0,
                        ..beat()
                    },
                ],
            )
        },
        demo(
            "stumm (Clip deaktiviert)",
            "muted",
            vec![DemoClip {
                muted: true,
                ..lead()
            }],
        ),
        demo(
            "aufnahme läuft",
            "recording",
            vec![DemoClip {
                name: "Aufnahme",
                recording: true,
                w: 120.0,
                ..lead()
            }],
        ),
        demo(
            "frisch aufgenommen · gewählt, im Editor",
            "recorded-selected",
            vec![DemoClip {
                name: "Lead · Take 1",
                state: sel,
                ..lead()
            }],
        ),
        demo(
            "leer",
            "empty",
            vec![DemoClip {
                name: "MIDI-Clip",
                content: DemoContent::Empty,
                ..lead()
            }],
        ),
        ClipDemo {
            width: 300.0,
            ..demo(
                "geloopt · 2 Takte × 2,5",
                "looped",
                vec![DemoClip {
                    w: 300.0,
                    pass: 120.0,
                    looped: true,
                    ..lead()
                }],
            )
        },
        demo(
            "kürzen · Kante aktiv",
            "trim",
            vec![DemoClip {
                w: 140.0,
                state: ClipState {
                    trim: Some(ClipEdge::End),
                    ..sel
                },
                ..lead()
            }],
        ),
        ClipDemo {
            width: 250.0,
            ghost: true,
            ..demo(
                "alt-ziehen · kopieren",
                "alt-copy",
                vec![DemoClip {
                    x: 70.0,
                    state: sel,
                    ..lead()
                }],
            )
        },
    ]
}

/// Size of a clip state sample: the strip (`padding: 3px 0`, `margin: 0 4px`) with its label, as
/// wide as the wider of both.
pub fn clip_demo_size(ui: &Ui, demo: &ClipDemo) -> Vec2 {
    let pad = metrics::CLIP_INSET_Y;
    vec2(
        demo.width.max(label_width(ui, demo.label)) + 2.0 * space::SPACE_1,
        pad + clip_h() + LABEL_GAP + typography::VALUE_S.line_height + space::SPACE_1 + pad,
    )
}

/// The preview of a demo clip at its zoom.
fn demo_preview(c: &DemoClip) -> Option<PreviewMesh> {
    let ppt = c.pass / demo_ticks().0 as f32;
    let length = Tick((c.w / ppt).round() as i64);
    let spec = PreviewSpec {
        length,
        loop_length: c.looped.then(demo_ticks),
        px_per_tick: ppt,
        body_height: clip::body_height(clip_h()),
    };
    match c.content {
        DemoContent::Midi => Some(PreviewMesh::midi(&demo_notes(), spec)),
        DemoContent::Steps => Some(PreviewMesh::steps(&demo_steps(), spec)),
        DemoContent::Empty => None,
    }
}

/// Paints a clip state sample in `rect` (strip, clips, label) and registers the clips' nodes.
pub fn paint_clip_demo(ui: &Ui, rect: Rect, demo: &ClipDemo) {
    let painter = ui.painter();
    let pad = metrics::CLIP_INSET_Y;
    px::fill(painter, rect, 0.0, color32(color::SURFACE_2));
    let label_top = rect.min.y + pad + clip_h() + LABEL_GAP;
    let label_box = Rect::from_min_max(
        pos2(rect.min.x, label_top),
        pos2(rect.max.x, rect.max.y - pad),
    );
    px::fill(painter, label_box, 0.0, color32(color::SURFACE_1));
    let origin = rect.min + vec2(space::SPACE_1, pad);
    if demo.ghost {
        clip::paint_clip_ghost(painter, Rect::from_min_size(origin, vec2(CLIP_W, clip_h())));
    }
    for (i, c) in demo.clips.iter().enumerate() {
        let palette = TrackPalette::new(c.hue);
        let preview = demo_preview(c);
        let model = ClipModel {
            muted: c.muted,
            recording: c.recording,
            loop_mark: c.looped,
            preview: preview.as_ref(),
            ..ClipModel::new(c.name, &palette)
        };
        let r = Rect::from_min_size(origin + vec2(c.x, 0.0), vec2(c.w, clip_h()));
        clip::paint_clip(painter, r, &model, c.state);
        // Bars as the sample shows them: one pass is two bars.
        let ppt = c.pass / demo_ticks().0 as f32;
        let start = Tick((c.x / ppt).round() as i64);
        let end = Tick(((c.x + c.w) / ppt).round() as i64);
        register(
            ui,
            Id::unique(("timeline-clip", demo.slug, i)),
            r,
            clip::clip_access(Lang::De, &model, start, end, METER, c.state),
        );
    }
    label(ui, pos2(rect.min.x + space::SPACE_1, label_top), demo.label);
}

/// The clip card: all 12 states (`flex-wrap`, gap 20 24, aligned to the bottom).
pub fn clip_card(ui: &mut Ui) {
    let demos = clip_demos();
    flow(
        ui,
        &demos,
        vec2(space::SPACE_6, space::SPACE_4 + space::SPACE_1),
        true,
        clip_demo_size,
        paint_clip_demo,
    );
}

// ---------------------------------------------------------------------------------------------
// Notes

/// One note of a note state: rect in the box, velocity, pitch, state.
#[derive(Clone, Copy, Debug)]
pub struct DemoNote {
    /// Rect relative to the box (`nS(x, y, w, h)`).
    pub rect: Rect,
    /// Velocity.
    pub velocity: u8,
    /// Pitch (for the name).
    pub pitch: u8,
    /// State.
    pub state: NoteState,
}

/// A note state: label, slug, box size, notes.
#[derive(Clone, Debug)]
pub struct NoteDemo {
    /// Label under the sample.
    pub label: &'static str,
    /// Name for snapshot files.
    pub slug: &'static str,
    /// Box size (`noteBox(w, h)`), one bar wide.
    pub size: Vec2,
    /// The notes.
    pub notes: Vec<DemoNote>,
    /// Notes large enough show their name (only the "mit Name" sample does in the design).
    pub names: bool,
}

fn n(x: f32, y: f32, w: f32, h: f32, velocity: u8, state: NoteState) -> DemoNote {
    DemoNote {
        rect: Rect::from_min_size(pos2(x, y), vec2(w, h)),
        velocity,
        pitch: 60,
        state,
    }
}

/// The 11 note states of `notes`.
pub fn note_demos() -> Vec<NoteDemo> {
    let rest = NoteState::REST;
    let sel = NoteState {
        selected: true,
        ..rest
    };
    let demo = |label, slug, w: f32, h: f32, notes| NoteDemo {
        label,
        slug,
        size: vec2(w, h),
        notes,
        names: false,
    };
    // `dichte`: 120 short notes of varying velocity (the design's generator).
    let dense = (0..120)
        .map(|i| {
            let x = ((i * 37) % 216) as f32;
            let y = (2 + (i * 53) % 20) as f32;
            let w = (4 + (i % 3) * 3) as f32;
            n(x, y, w, 2.0, (30 + (i * 29) % 97) as u8, rest)
        })
        .collect();
    vec![
        demo(
            "velocity 20 · 50 · 80 · 105 · 127",
            "velocity",
            220.0,
            24.0,
            [20, 50, 80, 105, 127]
                .iter()
                .enumerate()
                .map(|(i, &v)| n(4.0 + i as f32 * 43.0, 6.0, 38.0, 12.0, v, rest))
                .collect(),
        ),
        demo(
            "hover · L +0.08",
            "hover",
            100.0,
            24.0,
            vec![n(
                10.0,
                6.0,
                80.0,
                12.0,
                90,
                NoteState {
                    hovered: true,
                    ..rest
                },
            )],
        ),
        demo(
            "ausgewählt",
            "selected",
            100.0,
            24.0,
            vec![n(10.0, 6.0, 80.0, 12.0, 90, sel)],
        ),
        demo(
            "mehrfach",
            "multi-selected",
            140.0,
            24.0,
            vec![
                n(6.0, 6.0, 60.0, 12.0, 60, sel),
                n(72.0, 6.0, 60.0, 12.0, 110, sel),
            ],
        ),
        demo(
            "spielt gerade",
            "playing",
            100.0,
            24.0,
            vec![n(
                10.0,
                6.0,
                80.0,
                12.0,
                90,
                NoteState {
                    playing: true,
                    ..rest
                },
            )],
        ),
        demo(
            "stumm",
            "muted",
            100.0,
            24.0,
            vec![n(
                10.0,
                6.0,
                80.0,
                12.0,
                90,
                NoteState {
                    muted: true,
                    ..rest
                },
            )],
        ),
        demo(
            "ziehen · Original gestrichelt",
            "drag-original",
            140.0,
            36.0,
            vec![
                n(
                    10.0,
                    18.0,
                    70.0,
                    12.0,
                    90,
                    NoteState {
                        ghost: true,
                        ..rest
                    },
                ),
                n(54.0, 4.0, 70.0, 12.0, 90, sel),
            ],
        ),
        NoteDemo {
            names: true,
            ..demo(
                "mit Name ab h 12, w 28",
                "name",
                100.0,
                24.0,
                vec![DemoNote {
                    pitch: 61,
                    ..n(10.0, 6.0, 80.0, 12.0, 110, rest)
                }],
            )
        },
        demo(
            "zoom · Taste 8",
            "zoom-key-8",
            120.0,
            24.0,
            vec![
                n(6.0, 4.0, 50.0, 6.0, 70, rest),
                n(60.0, 12.0, 50.0, 6.0, 110, sel),
            ],
        ),
        demo(
            "zoom · Taste 4 · Auswahl = Akzentfläche",
            "zoom-key-4",
            120.0,
            24.0,
            vec![
                n(6.0, 6.0, 40.0, 3.0, 70, rest),
                n(50.0, 10.0, 40.0, 3.0, 90, sel),
                n(70.0, 14.0, 40.0, 3.0, 110, rest),
            ],
        ),
        demo("dichte · 400 Noten", "density", 220.0, 24.0, dense),
    ]
}

/// Size of a note state sample with its label, as wide as the wider of both.
pub fn note_demo_size(ui: &Ui, demo: &NoteDemo) -> Vec2 {
    vec2(
        demo.size.x.max(label_width(ui, demo.label)),
        demo.size.y + LABEL_GAP + typography::VALUE_S.line_height,
    )
}

/// Paints a note state sample (box with beat lines, notes, label) and registers the notes'
/// nodes. The box is one bar: positions in the names follow from x.
pub fn paint_note_demo(ui: &Ui, rect: Rect, demo: &NoteDemo) {
    let ppp = ui.pixels_per_point();
    let painter = ui.painter();
    let bx = Rect::from_min_size(rect.min, demo.size);
    let clipped = painter.with_clip_rect(bx.intersect(painter.clip_rect()));
    px::fill(&clipped, bx, 0.0, color32(color::SURFACE_2));
    // `noteBox` lines at i·w/4: bar lines line.2, beats line.1.
    for i in 0..4 {
        let c = if i % 4 == 0 {
            color::LINE_2
        } else {
            color::LINE_1
        };
        px::vline(
            &clipped,
            bx.min.x + i as f32 * demo.size.x / 4.0,
            bx.y_range(),
            color32(c),
        );
    }
    let palette = TrackPalette::new(HUE);
    let mut batch = NoteBatch::new(ppp);
    batch.show_names(demo.names);
    for (i, note) in demo.notes.iter().enumerate() {
        let r = note.rect.translate(bx.min.to_vec2());
        batch.add(r, &palette, note.pitch, note.velocity, note.state);
        if demo.notes.len() <= 8 {
            let beat_w = demo.size.x / 4.0;
            let start =
                Tick((note.rect.min.x / beat_w * METER.ticks_per_beat() as f32).round() as i64);
            register(
                ui,
                Id::unique(("timeline-note", demo.slug, i)),
                r,
                note_access(Lang::De, note.pitch, start, METER, note.state),
            );
        }
    }
    batch.paint(&clipped);
    label(ui, pos2(bx.min.x, bx.max.y + LABEL_GAP), demo.label);
}

/// The note card: all 11 states (`flex-wrap`, gap 20, aligned to the bottom).
pub fn note_card(ui: &mut Ui) {
    let demos = note_demos();
    flow(
        ui,
        &demos,
        vec2(
            space::SPACE_4 + space::SPACE_1,
            space::SPACE_4 + space::SPACE_1,
        ),
        true,
        note_demo_size,
        paint_note_demo,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_counts_match_the_design() {
        assert_eq!(header_demos().len(), 12);
        assert_eq!(clip_demos().len(), 12);
        assert_eq!(note_demos().len(), 11);
        let mut slugs: Vec<&str> = header_demos().iter().map(|d| d.slug).collect();
        slugs.dedup();
        assert_eq!(slugs.len(), 12);
    }

    #[test]
    fn demo_data() {
        let notes = demo_notes();
        assert_eq!(notes.len(), 17);
        assert!(
            notes
                .iter()
                .all(|n| n.start.0 + n.length.0 <= demo_ticks().0)
        );
        let steps = demo_steps();
        assert_eq!(steps.rows.len(), 4);
        assert_eq!(steps.rows[3].steps.iter().filter(|s| s.on).count(), 3);
    }
}
