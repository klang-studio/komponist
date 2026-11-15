//! Frame time of the app with the arrangement showing 16 tracks with 200 clips and 10 000 notes
//! (WP-40 "Done when", WP-61, ARCHITECTURE §9.2: UI frame ≤ 8 ms). Measured in release builds:
//!
//! ```text
//! cargo test --release -p komponist-app --test arrangement_perf -- --ignored --nocapture
//! ```
//!
//! The whole UI frame (`App::show`: shell, transport, arrangement with every clip visible,
//! placeholders of the other views) plus egui's tessellation, on one thread, at 1× and 2×: the
//! first frame (clip previews built) and the median and worst of the following ones (previews
//! cached).

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, RawInput, Rect, pos2, vec2};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::{Action, App, TransportAction, UiAction};
use komponist_model::fixtures::demo_project;
use komponist_model::{Clip, ClipContent, ClipId, MidiClip, Note, NoteId, Project, Take, Tick};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const TRACKS: usize = 16;
const CLIPS: usize = 200;
const NOTES_PER_CLIP: usize = 50;
const BAR: i64 = 3840;
const FRAMES: usize = 60;
const BUDGET: Duration = Duration::from_millis(8);

/// The demo's six tracks plus ten, with 200 one-bar clips of 50 notes (10 000 notes) dealt round
/// the tracks (12 or 13 per track).
fn big_project() -> Project {
    let mut p = demo_project();
    while p.tracks.len() < TRACKS {
        let t = Arc::clone(&p.tracks[p.tracks.len() % 6]);
        let mut t = komponist_model::Track::clone(&t);
        t.id = komponist_model::TrackId(p.alloc_id());
        // Own devices (the engine plays every track).
        for d in &mut t.channel.devices {
            Arc::make_mut(d).id = komponist_model::DeviceId(p.alloc_id());
        }
        p.tracks.push(Arc::new(t));
    }
    let template = Clip::clone(&p.tracks[2].clips[0]);
    let mut seed = 0x2545_f491_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut clips: Vec<Vec<Arc<Clip>>> = vec![Vec::new(); TRACKS];
    for ci in 0..CLIPS {
        let mut notes: Vec<Note> = (0..NOTES_PER_CLIP)
            .map(|n| Note {
                id: NoteId(p.alloc_id()),
                start: Tick((n as i64 * BAR) / NOTES_PER_CLIP as i64),
                length: Tick::from_steps(1 + (next() % 4) as i64),
                pitch: 40 + (next() % 40) as u8,
                velocity: 1 + (next() % 127) as u8,
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.pitch));
        let mut c = template.clone();
        c.id = ClipId(p.alloc_id());
        c.start = Tick((ci / TRACKS) as i64 * BAR);
        c.length = Tick(BAR);
        c.loop_length = None;
        c.content = ClipContent::Midi(MidiClip {
            takes: vec![Take {
                name: String::new(),
                notes: Arc::new(notes),
            }],
            active_take: 0,
        });
        clips[ci % TRACKS].push(Arc::new(c));
    }
    for (ti, clips) in clips.into_iter().enumerate() {
        let mut t = komponist_model::Track::clone(&p.tracks[ti]);
        t.clips = clips;
        p.tracks[ti] = Arc::new(t);
    }
    p
}

fn input(frame: usize) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))),
        time: Some(frame as f64 / 60.0),
        ..Default::default()
    }
}

/// One frame: the app's UI pass and its tessellation.
fn frame(ctx: &Context, app: &mut App, n: usize) -> Duration {
    let start = Instant::now();
    let mut out = ctx.run_ui(input(n), |ui| app.show(ui));
    out.textures_delta.clear();
    let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
    let t = start.elapsed();
    assert!(!prims.is_empty());
    t
}

/// What the frame shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    /// The arrangement alone, nothing selected.
    Idle,
    /// The arrangement alone, every clip selected.
    AllSelected,
    /// Playing on the null backend (16 hexa tracks in real time on the audio thread), the
    /// dock open with one clip in the piano roll: playhead, meters and both canvases move.
    Playing,
}

fn measure(ppp: f32, case: Case) -> (Duration, Duration, Duration) {
    let ctx = Context::default();
    komponist_ui::install(&ctx);
    ctx.set_pixels_per_point(ppp);
    let mut app = App::headless(big_project());
    // 56 px per bar: all 13 bars of every track in the 1154 px of timeline.
    app.ui_mut().arrangement.px_per_bar = 56.0;
    app.ui_mut().layout.dock_open = case == Case::Playing;
    if case == Case::Playing {
        let clip = app.project().tracks[2].clips[0].id;
        app.dispatch(Action::Ui(UiAction::OpenClip(clip)));
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
    }
    if case == Case::AllSelected {
        let clips = app
            .project()
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(|c| c.id))
            .collect();
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips,
            mode: SelectMode::Replace,
        })));
    }
    // Fonts and icons.
    ctx.run_ui(input(0), |_| {}).textures_delta.clear();
    let first = frame(&ctx, &mut app, 1);
    let mut times: Vec<Duration> = (2..2 + FRAMES).map(|n| frame(&ctx, &mut app, n)).collect();
    times.sort();
    (first, times[FRAMES / 2], times[FRAMES - 1])
}

#[test]
fn the_big_project_has_16_tracks_200_clips_and_10_000_notes() {
    let p = big_project();
    let clips: usize = p.tracks.iter().map(|t| t.clips.len()).sum();
    let notes: usize = p
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| match &c.content {
            ClipContent::Midi(m) => m.active().notes.len(),
            ClipContent::Step(_) => 0,
        })
        .sum();
    assert_eq!((p.tracks.len(), clips, notes), (TRACKS, CLIPS, 10_000));
}

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn frame_time_with_200_clips_and_10_000_notes() {
    let mut worst_median = Duration::ZERO;
    for ppp in [1.0, 2.0] {
        for case in [Case::Idle, Case::AllSelected, Case::Playing] {
            let (first, median, max) = measure(ppp, case);
            println!(
                "{ppp}x, {case:?}: first frame {:.2} ms, median {:.2} ms, worst {:.2} ms",
                first.as_secs_f64() * 1e3,
                median.as_secs_f64() * 1e3,
                max.as_secs_f64() * 1e3,
            );
            worst_median = worst_median.max(median);
        }
    }
    if cfg!(debug_assertions) {
        println!("(debug build: the budget is checked in release builds only)");
    } else {
        assert!(
            worst_median < BUDGET,
            "median frame {worst_median:?} over {BUDGET:?}"
        );
    }
}
