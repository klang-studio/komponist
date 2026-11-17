//! The timeline paint functions (WP-22), rendered with `egui_kittest` (wgpu) at 1× and 2×.
//!
//! - One snapshot per state of the design system: 12 track headers, 12 clips, 11 notes
//!   (`tests/snapshots/timeline-{header,clip,note}-<state>-{1x,2x}.png`).
//! - Ruler and grid, and an S01-like arrangement and piano roll from the demo project
//!   "Night Drive" for the side-by-side check with reference screen S01 (internal notes).
//! - AccessKit: the gallery cards expose every painted header, clip and note with its German
//!   name and selection state.
//! - Pixels: straight note edges are crisp at fractional positions.

use egui::{Color32, Rangef, Rect, Sense, Ui, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use komponist_model::fixtures::demo_project;
use komponist_model::format::fmt_pos;
use komponist_model::{ClipContent, Grid, Meter, Tick, TickRange};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, size};
use komponist_ui::snapshot::{self, SCALES, Snapshots};
use komponist_ui::timeline::clip::{ClipModel, ClipState, PreviewMesh, body_height};
use komponist_ui::timeline::gallery;
use komponist_ui::timeline::note::{NoteBatch, NoteState};
use komponist_ui::timeline::track_header::HeaderModel;
use komponist_ui::timeline::{
    GridSpec, RulerKind, RulerModel, TimeAxis, clip_rect, grid_lines, note_rect, paint_clip,
    paint_grid, paint_playhead, paint_ruler, paint_track_header,
};
use komponist_ui::{State, px};

const M: Meter = Meter::FOUR_FOUR;

/// Allocates `size` and returns its rect.
fn area(ui: &mut Ui, size: egui::Vec2) -> Rect {
    ui.allocate_exact_size(size, Sense::hover()).0
}

#[test]
fn header_states() {
    let mut shots = Snapshots::new();
    let demos = gallery::header_demos();
    assert_eq!(demos.len(), 12);
    for d in &demos {
        let size = vec2(
            size::TRACK_HEADER,
            d.rows.len() as f32 * size::track::DEFAULT,
        );
        shots.add(&format!("timeline-header-{}", d.slug), None, |ui| {
            let r = area(ui, size);
            gallery::paint_header_demo(ui, r, d);
        });
    }
    shots.finish();
}

#[test]
fn header_heights() {
    // Smallest and largest track height: contents stay centred, bar and meter stretch.
    let palette = TrackPalette::new(150);
    let meter = gallery::demo_meter(0.5, 0.6);
    let mut model = HeaderModel::new("Lead", "Hexa", &palette);
    model.meter = Some(&meter);
    snapshot::snapshot("timeline-header-heights", None, |ui| {
        ui.horizontal_top(|ui| {
            for h in [size::track::MIN, size::track::MAX] {
                let r = area(ui, vec2(size::TRACK_HEADER, h));
                paint_track_header(ui.painter(), r, &model, State::REST);
            }
        });
    });
}

#[test]
fn clip_states() {
    let mut shots = Snapshots::new();
    let demos = gallery::clip_demos();
    assert_eq!(demos.len(), 12);
    for d in &demos {
        shots.add(&format!("timeline-clip-{}", d.slug), None, |ui| {
            let r = area(ui, gallery::clip_demo_size(ui, d));
            gallery::paint_clip_demo(ui, r, d);
        });
    }
    shots.finish();
}

#[test]
fn note_states() {
    let mut shots = Snapshots::new();
    let demos = gallery::note_demos();
    assert_eq!(demos.len(), 11);
    for d in &demos {
        shots.add(&format!("timeline-note-{}", d.slug), None, |ui| {
            let r = area(ui, gallery::note_demo_size(ui, d));
            gallery::paint_note_demo(ui, r, d);
        });
    }
    shots.finish();
}

/// Bars of S01: 16 bars of 72 px, loop over bars 9–12, playhead at 10.2.120.
const BAR_W: f32 = 72.0;
fn playhead() -> Tick {
    Tick(9 * 3840 + 960 + 480)
}

#[test]
fn rulers() {
    let mut shots = Snapshots::new();
    let w = 16.0 * BAR_W;
    shots.add("timeline-ruler-arrangement", None, |ui| {
        let r = area(ui, vec2(w, size::RULER));
        let model = RulerModel {
            axis: TimeAxis::per_bar(r.min.x, BAR_W, M),
            meter: M,
            kind: RulerKind::Arrangement {
                loop_range: Some(TickRange::new(Tick(8 * 3840), Tick(12 * 3840))),
            },
            playhead: Some(playhead()),
            recording: false,
        };
        paint_ruler(ui.painter(), r, &model);
    });
    shots.add("timeline-ruler-recording-zoomed-out", None, |ui| {
        // 12 px per bar: numbers thin out, no beat ticks; the cap is red.
        let r = area(ui, vec2(w, size::RULER));
        let model = RulerModel {
            axis: TimeAxis::per_bar(r.min.x, 12.0, M),
            meter: M,
            kind: RulerKind::Arrangement { loop_range: None },
            playhead: Some(Tick(40 * 3840)),
            recording: true,
        };
        paint_ruler(ui.painter(), r, &model);
    });
    shots.add("timeline-ruler-editor", None, |ui| {
        // The piano roll of S01: bars 9–12 over 1326 px, clip "Lead A" over bars 9–10.
        let r = area(ui, vec2(1326.0, 18.0));
        let cw = 1326.0 / 64.0;
        let model = RulerModel {
            axis: TimeAxis::per_step(r.min.x - 8.0 * 16.0 * cw, cw),
            meter: M,
            kind: RulerKind::Editor {
                clip: TickRange::new(Tick(8 * 3840), Tick(10 * 3840)),
                clip_color: color32(TrackPalette::new(150).base),
            },
            playhead: None,
            recording: false,
        };
        paint_ruler(ui.painter(), r, &model);
    });
    shots.finish();
}

#[test]
fn grids() {
    let mut shots = Snapshots::new();
    shots.add("timeline-grid-arrangement", None, |ui| {
        let r = area(ui, vec2(16.0 * BAR_W, 56.0));
        ui.painter().rect_filled(r, 0.0, color32(color::SURFACE_1));
        let axis = TimeAxis::per_bar(r.min.x, BAR_W, M);
        let lines = grid_lines(&axis, M, r.x_range(), &GridSpec::new(Grid::Bar, 8.0));
        paint_grid(ui.painter(), r, &lines);
    });
    shots.add("timeline-grid-editor", None, |ui| {
        let r = area(ui, vec2(640.0, 60.0));
        ui.painter().rect_filled(r, 0.0, color32(color::SURFACE_2));
        let axis = TimeAxis::per_step(r.min.x, 20.0);
        let lines = grid_lines(&axis, M, r.x_range(), &GridSpec::new(Grid::Sixteenth, 8.0));
        paint_grid(ui.painter(), r, &lines);
    });
    shots.finish();
}

/// The arrangement of S01 from the demo project: ruler, headers, lanes with bar lines, clips with
/// cached previews, playhead; Lead selected with its first clip.
fn arrangement(ui: &mut Ui) {
    let project = demo_project();
    let lane_w = 16.0 * BAR_W;
    let ruler_h = size::RULER;
    let lane_h = size::track::DEFAULT;
    let total = vec2(
        size::TRACK_HEADER + lane_w,
        ruler_h + project.tracks.len() as f32 * lane_h,
    );
    let r = area(ui, total);
    let p = ui.painter();
    p.rect_filled(r, 0.0, color32(color::SURFACE_1));
    let lanes_x = r.min.x + size::TRACK_HEADER;
    let axis = TimeAxis::per_bar(lanes_x, BAR_W, M);
    let ruler = Rect::from_min_size(pos2(lanes_x, r.min.y), vec2(lane_w, ruler_h));
    p.rect_filled(
        Rect::from_min_size(r.min, vec2(size::TRACK_HEADER, ruler_h)),
        0.0,
        color32(color::SURFACE_2),
    );
    paint_ruler(
        p,
        ruler,
        &RulerModel {
            axis,
            meter: M,
            kind: RulerKind::Arrangement {
                loop_range: project.loop_on.then_some(project.loop_range),
            },
            playhead: Some(playhead()),
            recording: false,
        },
    );
    let any_solo = project.tracks.iter().any(|t| t.channel.solo);
    for (i, t) in project.tracks.iter().enumerate() {
        let top = r.min.y + ruler_h + i as f32 * lane_h;
        let palette = TrackPalette::new(t.hue.0);
        let meter = gallery::demo_meter(0.55, 0.63);
        let selected = t.name == "Lead";
        let instrument = t
            .channel
            .devices
            .first()
            .map(|d| d.plugin.name.as_str())
            .unwrap_or("");
        let model = HeaderModel {
            mute: t.channel.mute,
            solo: t.channel.solo,
            arm: t.arm,
            monitor: t.monitor,
            implied_mute: any_solo && !t.channel.solo,
            meter: (!t.channel.mute).then_some(&meter),
            ..HeaderModel::new(&t.name, instrument, &palette)
        };
        let hdr = Rect::from_min_size(pos2(r.min.x, top), vec2(size::TRACK_HEADER, lane_h));
        paint_track_header(p, hdr, &model, State::REST.with_selected(selected));
        let lane = Rect::from_min_size(pos2(lanes_x, top), vec2(lane_w, lane_h));
        let lines = grid_lines(&axis, M, lane.x_range(), &GridSpec::new(Grid::Bar, 8.0));
        paint_grid(p, lane, &lines);
        px::hline(
            p,
            lane.x_range(),
            lane.max.y - 1.0,
            color32(color::SURFACE_0),
        );
        for (k, c) in t.clips.iter().enumerate() {
            let cr = clip_rect(lane, axis.x(c.start), axis.x(c.end()));
            let preview = PreviewMesh::from_clip(c, axis.px_per_tick, body_height(cr.height()));
            let model = ClipModel {
                muted: c.mute || model.dimmed(),
                loop_mark: c.loop_length.is_some() && matches!(c.content, ClipContent::Midi(_)),
                audio: None,
                preview: Some(&preview),
                ..ClipModel::new(&c.name, &palette)
            };
            let state = ClipState {
                selected: selected && k == 0,
                ..ClipState::REST
            };
            paint_clip(p, cr, &model, state);
        }
    }
    paint_playhead(
        p,
        axis.x(playhead()),
        Rangef::new(r.min.y + ruler_h, r.max.y),
    );
}

/// The piano roll of S01: clip "Lead A" (bars 9–10) over bars 9–12 at key height 12, two notes
/// selected, the notes under the playhead playing.
fn piano_roll(ui: &mut Ui) {
    let project = demo_project();
    let lead = project.tracks.iter().find(|t| t.name == "Lead").unwrap();
    let clip = &lead.clips[0];
    let ClipContent::Midi(m) = &clip.content else {
        panic!("Lead A is a MIDI clip")
    };
    let notes = &m.active().notes;
    let palette = TrackPalette::new(lead.hue.0);
    let w = 1326.0;
    let cw = w / 64.0;
    let key = size::key::DEFAULT;
    let top_pitch = 74_u8;
    let rows = 20.0;
    let r = area(ui, vec2(w, 18.0 + rows * key));
    let p = ui.painter();
    let ruler = Rect::from_min_size(r.min, vec2(w, 18.0));
    let grid = Rect::from_min_max(pos2(r.min.x, ruler.max.y), r.max);
    // Tick 0 of the clip sits at the left edge.
    let axis = TimeAxis::per_step(r.min.x - clip.start.0 as f32 / 240.0 * cw, cw);
    paint_ruler(
        p,
        ruler,
        &RulerModel {
            axis,
            meter: M,
            kind: RulerKind::Editor {
                clip: TickRange::new(clip.start, clip.end()),
                clip_color: color32(palette.base),
            },
            playhead: None,
            recording: false,
        },
    );
    p.rect_filled(grid, 0.0, color32(color::SURFACE_2));
    let lines = grid_lines(
        &axis,
        M,
        grid.x_range(),
        &GridSpec::new(Grid::Sixteenth, 8.0),
    );
    paint_grid(p, grid, &lines);
    let ph = playhead();
    let mut batch = NoteBatch::new(ui.pixels_per_point());
    for (i, n) in notes.iter().enumerate() {
        if n.pitch > top_pitch || n.pitch + rows as u8 <= top_pitch {
            continue;
        }
        let start = clip.start + n.start;
        let row_top = grid.min.y + f32::from(top_pitch - n.pitch) * key;
        let nr = note_rect(&axis, start, n.length, row_top, key);
        let state = NoteState {
            selected: i == 3 || i == 4,
            playing: start <= ph && ph < start + n.length,
            ..NoteState::REST
        };
        batch.add(nr, &palette, n.pitch, n.velocity, state);
    }
    batch.paint(p);
    paint_playhead(p, axis.x(ph), grid.y_range());
}

#[test]
fn s01_side_by_side() {
    let mut shots = Snapshots::new();
    shots.add("timeline-s01-arrangement", None, arrangement);
    shots.add("timeline-s01-piano-roll", None, piano_roll);
    shots.finish();
}

fn pixel(img: &egui::ColorImage, x: usize, y: usize) -> Color32 {
    img.pixels[y * img.size[0] + x]
}

#[test]
fn note_edges_are_crisp() {
    let palette = TrackPalette::new(150);
    let fill = color32(palette.note(100));
    let bg = color32(color::SURFACE_1);
    for ppp in SCALES.into_iter().chain([1.5]) {
        let img = snapshot::render(vec2(80.0, 40.0), ppp, |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("note"),
            ));
            p.rect_filled(
                Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 40.0)),
                0.0,
                bg,
            );
            let mut b = NoteBatch::new(ppp);
            b.add(
                Rect::from_min_size(pos2(10.3, 10.4), vec2(40.0, 10.0)),
                &palette,
                60,
                100,
                NoteState::REST,
            );
            b.paint(&p);
        });
        let dev = |v: f32| (px::snap(v, ppp) * ppp).round() as usize;
        let (left, right) = (dev(10.3), dev(50.3));
        let (top, bottom) = (dev(10.4), dev(20.4));
        let mid_y = (top + bottom) / 2;
        let mid_x = (left + right) / 2;
        assert_eq!(pixel(&img, left, mid_y), fill, "left edge at {ppp}×");
        assert_eq!(pixel(&img, left - 1, mid_y), bg, "outside left at {ppp}×");
        assert_eq!(pixel(&img, right - 1, mid_y), fill, "right edge at {ppp}×");
        assert_eq!(pixel(&img, right, mid_y), bg, "outside right at {ppp}×");
        assert_eq!(pixel(&img, mid_x, top), fill, "top edge at {ppp}×");
        assert_eq!(pixel(&img, mid_x, top - 1), bg, "above at {ppp}×");
        assert_eq!(
            pixel(&img, mid_x, bottom - 1),
            fill,
            "bottom edge at {ppp}×"
        );
        assert_eq!(pixel(&img, mid_x, bottom), bg, "below at {ppp}×");
        // The rounded corner pixel is blended, neither fill nor background.
        let corner = pixel(&img, left, top);
        assert_ne!(corner, fill, "corner at {ppp}×");
    }
}

#[test]
fn cards_expose_painted_objects() {
    let mut h = snapshot::builder::<()>(1.0)
        .with_size(vec2(1600.0, 900.0))
        .build_ui(|ui| {
            gallery::header_card(ui);
            gallery::clip_card(ui);
            gallery::note_card(ui);
        });
    h.run();
    h.check_accessibility();

    let lead = h.get_all_by_label("Spur Lead, Hexa").count();
    assert!(lead >= 6, "{lead} Lead headers");
    h.get_by_label("Spur Strings, Plugin fehlt: Vital");
    h.get_by_label("Spur Pad, stumm durch Solo");
    let selected = h
        .get_all_by_label("Spur Bass, Hexa")
        .next()
        .expect("Bass header");
    assert_eq!(
        selected.accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True)
    );

    // Clips: one pass is two bars in the samples.
    h.get_by_label("Clip Beat 1, Takt 1\u{2013}2");
    h.get_by_label("Clip Lead A, Takt 1\u{2013}2, stumm");
    h.get_by_label("Clip Aufnahme, Takt 1\u{2013}2");
    h.get_by_label("Clip Lead A, Takt 1\u{2013}5");
    h.get_by_label("Clip MIDI-Clip, Takt 1\u{2013}2");

    // Notes: the box is one bar; the name sample starts at 10 / 25 of a beat.
    let pos = fmt_pos(Tick(384), M);
    assert_eq!(pos, "1.1.096");
    h.get_by_label(&format!("Note C#4, {pos}"));
    h.get_by_label("Note C4, 1.1.096, stumm");
    assert!(h.get_all_by_label_contains("Note C4").count() > 10);
}

/// Audio proposal 02: all waveform states, wide/narrow Re-Pitch, and selected controls.
#[test]
fn audio_clip_states_in_both_languages() {
    use komponist_i18n::Lang;
    use komponist_ui::timeline::{AudioFileState, AudioLook, WaveMesh, WaveSpec};
    let samples: Vec<f32> = (0..48_000)
        .map(|i| ((i as f32 * 0.08).sin()) * if i % 12000 < 400 { 0.95 } else { 0.25 })
        .collect();
    let peaks = komponist_audiofile::PeakData::from_planar(48_000, &[samples]);
    let spec = WaveSpec {
        width: 280.0,
        px_per_tick: 0.02,
        body_height: 36.0,
        offset: 0,
        frames: 48_000,
        gain: 1.0,
        fade_in: 30.0,
        fade_out: 60.0,
    };
    let wave = WaveMesh::from_peaks(&peaks, &spec);
    let flat = WaveMesh::flat(280.0, 0.02, 36.0);
    let palette = TrackPalette::new(150);
    let mut results = egui_kittest::SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let name = if lang == Lang::De {
            "timeline-audio-states"
        } else {
            "timeline-audio-states-en"
        };
        let paint = |ui: &mut Ui| {
            komponist_ui::set_lang(ui.ctx(), lang);
            let canvas = area(ui, vec2(568.0, 392.0));
            for (i, (file, selected, muted, repitch, width)) in [
                (AudioFileState::Ready, false, false, false, 280.0),
                (AudioFileState::Ready, true, false, false, 280.0),
                (AudioFileState::Ready, false, true, false, 280.0),
                (AudioFileState::Ready, false, false, true, 280.0),
                (AudioFileState::Ready, false, false, true, 70.0),
                (AudioFileState::Missing, false, false, false, 560.0),
                (AudioFileState::Pending, false, false, false, 560.0),
            ]
            .into_iter()
            .enumerate()
            {
                let rect =
                    Rect::from_min_size(canvas.min + vec2(0.0, i as f32 * 56.0), vec2(width, 50.0));
                let message = match file {
                    AudioFileState::Missing => lang.audio_clip_missing("verse.wav"),
                    AudioFileState::Pending => lang.audio_clip_peaks().to_owned(),
                    _ => String::new(),
                };
                let audio = AudioLook {
                    takes: None,
                    file,
                    wave: if file == AudioFileState::Ready {
                        &wave
                    } else {
                        &flat
                    },
                    repitch: repitch.then_some(120.0),
                    repitch_label: lang.audio_clip_repitch_badge(),
                    message: &message,
                    gain_db: if selected { -3.0 } else { 0.0 },
                    fade_in: if selected { 30.0 } else { 0.0 },
                    fade_out: if selected { 60.0 } else { 0.0 },
                };
                let mut model = ClipModel::new("Verse", &palette);
                model.audio = Some(&audio);
                model.muted = muted;
                paint_clip(
                    ui.painter(),
                    rect,
                    &model,
                    if selected {
                        ClipState::SELECTED
                    } else {
                        ClipState::REST
                    },
                );
            }
        };
        for ppp in SCALES {
            // Upload atlas changes as each pass runs. Deferring all texture deltas until the
            // fourth (English 2×) render can lose earlier glyph/white-pixel atlas regions.
            let mut h = snapshot::builder(ppp)
                .with_size(vec2(600.0, 420.0))
                .wgpu()
                .build_ui(|ui| {
                    snapshot::padded(ui, paint);
                });
            h.run();
            if std::env::var_os("KOMPONIST_WAVE_PREVIEW").is_some() {
                h.render()
                    .expect("waveform preview")
                    .save(
                        std::env::temp_dir()
                            .join(format!("komponist-wave-quality-{name}-{ppp}x.png")),
                    )
                    .expect("waveform preview PNG");
            }
            h.snapshot(snapshot::file_stem(name, ppp));
            results.extend_harness(&mut h);
        }
    }
    results.unwrap();
}
