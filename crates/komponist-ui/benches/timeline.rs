//! Paint 10 000 notes and report the CPU time, tessellation included (WP-22 "Done when").
//!
//! ```text
//! cargo bench -p komponist-ui --bench timeline
//! ```
//!
//! Two cases, each at 1× and 2×, best and median of 30 frames on one thread:
//!
//! - **piano roll**: 10 000 notes (key height 12, 20 px per 1/16, 1 % selected, 0.5 % playing,
//!   every 50th hovered) collected into a [`NoteBatch`], painted, and tessellated by egui.
//! - **arrangement**: 200 clips of 50 notes each (10 000 notes) with cached [`PreviewMesh`]es,
//!   painted with [`paint_clip`] and tessellated; building the previews is timed separately (it
//!   happens once per clip and zoom, not per frame).
//!
//! The budget is 4 ms per case; the bench fails above it.

use std::time::{Duration, Instant};

use egui::{Context, RawInput, Rect, pos2, vec2};
use komponist_model::{Note, NoteId, Tick};
use komponist_tokens::TrackPalette;
use komponist_ui::timeline::clip::{ClipModel, ClipState, PreviewMesh, PreviewSpec, body_height};
use komponist_ui::timeline::note::{NoteBatch, NoteState};
use komponist_ui::timeline::{TimeAxis, clip_rect, note_rect, paint_clip};

const NOTES: usize = 10_000;
const FRAMES: usize = 30;
const BUDGET: Duration = Duration::from_millis(4);

/// A deterministic pseudo-random note list: 10 000 notes over 160 bars, pitches 36–96.
fn notes(n: usize) -> Vec<Note> {
    let mut seed = 0x2545_f491_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut v: Vec<Note> = (0..n)
        .map(|i| Note {
            id: NoteId(i as u64),
            start: Tick::from_steps((i as i64 * 16 * 160) / n as i64),
            length: Tick::from_steps(1 + (next() % 8) as i64),
            pitch: 36 + (next() % 60) as u8,
            velocity: 1 + (next() % 127) as u8,
        })
        .collect();
    v.sort_by_key(|n| (n.start, n.pitch));
    v
}

fn context(ppp: f32) -> Context {
    let ctx = Context::default();
    komponist_ui::install(&ctx);
    ctx.set_pixels_per_point(ppp);
    // One frame so the font atlas exists.
    let _ = ctx.run_ui(RawInput::default(), |_| {});
    ctx
}

fn input() -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))),
        ..Default::default()
    }
}

/// Runs `paint` in a frame and tessellates its output; returns the time of both.
fn frame(ctx: &Context, mut paint: impl FnMut(&egui::Painter)) -> (Duration, usize) {
    let start = Instant::now();
    let out = ctx.run_ui(input(), |ui| paint(ui.painter()));
    let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
    let t = start.elapsed();
    let verts = prims
        .iter()
        .map(|p| match &p.primitive {
            egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
            egui::epaint::Primitive::Callback(_) => 0,
        })
        .sum();
    (t, verts)
}

/// The time of an empty frame (egui overhead), subtracted from the cases.
fn empty_frame(ctx: &Context) -> Duration {
    best(
        &(0..FRAMES)
            .map(|_| frame(ctx, |_| {}).0)
            .collect::<Vec<_>>(),
    )
}

fn best(ts: &[Duration]) -> Duration {
    *ts.iter().min().unwrap()
}

fn median(ts: &[Duration]) -> Duration {
    let mut v = ts.to_vec();
    v.sort();
    v[v.len() / 2]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn piano_roll(ppp: f32) -> (Duration, Duration, usize) {
    let ctx = context(ppp);
    let notes = notes(NOTES);
    let palette = TrackPalette::new(150);
    let axis = TimeAxis::per_step(0.0, 20.0);
    let key = 12.0;
    let rects: Vec<Rect> = notes
        .iter()
        .map(|n| {
            // Wrap the 160 bars into rows of 4 bars so everything is on screen.
            let x = axis.x(n.start);
            let row = (x / 1280.0).floor();
            let r = note_rect(&axis, n.start, n.length, f32::from(96 - n.pitch) * key, key);
            r.translate(vec2(-row * 1280.0, (row % 2.0) * 3.0))
        })
        .collect();
    let empty = empty_frame(&ctx);
    let mut times = Vec::with_capacity(FRAMES);
    let mut verts = 0;
    for _ in 0..FRAMES {
        let (t, v) = frame(&ctx, |p| {
            let mut batch = NoteBatch::with_capacity(p.pixels_per_point(), notes.len());
            for (i, (n, r)) in notes.iter().zip(&rects).enumerate() {
                let state = NoteState {
                    selected: i % 100 == 0,
                    playing: i % 200 == 7,
                    hovered: i % 50 == 3,
                    ..NoteState::REST
                };
                batch.add(*r, &palette, n.pitch, n.velocity, state);
            }
            batch.paint(p);
        });
        times.push(t.saturating_sub(empty));
        verts = v;
    }
    (best(&times), median(&times), verts)
}

fn arrangement(ppp: f32) -> (Duration, Duration, Duration, usize) {
    let ctx = context(ppp);
    let palette = TrackPalette::new(250);
    let px_per_bar = 72.0;
    let axis = TimeAxis::per_bar(0.0, px_per_bar, komponist_model::Meter::FOUR_FOUR);
    let lane_h = 56.0;
    let clip_len = Tick(2 * 3840);
    let per_clip = NOTES / 200;
    let clip_notes: Vec<Vec<Note>> = (0..200)
        .map(|c| {
            let mut v = notes(per_clip * 4);
            v.truncate(per_clip);
            for n in &mut v {
                n.start = Tick(n.start.0 % clip_len.0);
                n.pitch = n.pitch.wrapping_add(c as u8 % 12);
            }
            v.sort_by_key(|n| (n.start, n.pitch));
            v
        })
        .collect();
    // 20 tracks × 10 clips of 2 bars, 1440 px of timeline.
    let rects: Vec<Rect> = (0..200)
        .map(|i| {
            let (track, col) = (i / 10, i % 10);
            let lane = Rect::from_min_size(pos2(0.0, track as f32 * lane_h), vec2(1920.0, lane_h));
            let x0 = axis.x(Tick(col as i64 * clip_len.0));
            clip_rect(lane, x0, x0 + axis.width(clip_len))
        })
        .collect();
    let build_start = Instant::now();
    let previews: Vec<PreviewMesh> = clip_notes
        .iter()
        .zip(&rects)
        .map(|(n, r)| {
            PreviewMesh::midi(
                n,
                PreviewSpec {
                    length: clip_len,
                    loop_length: None,
                    px_per_tick: axis.px_per_tick,
                    body_height: body_height(r.height()),
                },
            )
        })
        .collect();
    let build = build_start.elapsed();
    let empty = empty_frame(&ctx);
    let mut times = Vec::with_capacity(FRAMES);
    let mut verts = 0;
    for _ in 0..FRAMES {
        let (t, v) = frame(&ctx, |p| {
            for (i, (r, preview)) in rects.iter().zip(&previews).enumerate() {
                let model = ClipModel {
                    preview: Some(preview),
                    ..ClipModel::new("Bass", &palette)
                };
                let state = ClipState {
                    selected: i % 17 == 0,
                    hovered: i == 5,
                    ..ClipState::REST
                };
                paint_clip(p, *r, &model, state);
            }
        });
        times.push(t.saturating_sub(empty));
        verts = v;
    }
    (best(&times), median(&times), build, verts)
}

fn main() {
    let mut failed = false;
    for ppp in [1.0, 2.0] {
        let (b, m, v) = piano_roll(ppp);
        println!(
            "piano roll  {ppp}×: {NOTES} notes, best {:.2} ms, median {:.2} ms, {v} vertices",
            ms(b),
            ms(m)
        );
        failed |= b > BUDGET;
        let (b, m, build, v) = arrangement(ppp);
        println!(
            "arrangement {ppp}×: 200 clips / {NOTES} notes, best {:.2} ms, median {:.2} ms, \
             {v} vertices; building the previews {:.2} ms",
            ms(b),
            ms(m),
            ms(build)
        );
        failed |= b > BUDGET;
    }
    assert!(!failed, "over the budget of {} ms", ms(BUDGET));
}
