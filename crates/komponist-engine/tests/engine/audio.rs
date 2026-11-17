//! Audio clips in the engine (issue #11, ARCHITECTURE §6.9.3): sample-exact start and offset,
//! gain, fades and edge fades, Re-Pitch and rate conversion, locate and loop wrap (also through the
//! heads), the underrun behaviour, routing with delay compensation, the export against a realtime
//! run, and missing files.
//!
//! Projects are built with the model's edits from small generated WAV files (32-bit float, so the
//! samples are exact) and compiled with `compile::build`. Offline checks use `Engine::render`
//! (which runs the streamer's work itself); the realtime checks use `Engine::process` while the
//! engine's read-ahead thread runs, paced like a device. Every call runs inside `assert_no_alloc`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use klang_dsp::units::db_to_gain;
use komponist_engine::compile::{self, NoEnv};
use komponist_engine::export::{BitDepth, ExportError, ExportJob, export};
use komponist_engine::testing::TestGain;
use komponist_engine::{Command, Program, SourceStatus};
use komponist_model::edits::{
    self, AudioClipProp, NewAudioTrack, NewClip, NewClipContent, NewDevice,
};
use komponist_model::{
    AudioFile, AudioInput, ChannelRef, ClipId, ClipTimeMode, DeviceRole, PluginFormat, PluginRef,
    Project, Tempo, Tick, TickRange, TrackId, TrackOutput,
};

use crate::common::*;

/// One beat in ticks and in samples at 120 bpm and 48 kHz.
const BEAT: i64 = 960;
const BEAT_FRAMES: usize = 24_000;
/// 3 ms at 48 kHz.
const EDGE: usize = 144;

/// A project under construction and the folder of its generated files.
struct Studio {
    project: Project,
    dir: PathBuf,
}

impl Studio {
    fn new(name: &str) -> Studio {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("audio")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Studio {
            project: Project::new("Audio"),
            dir,
        }
    }

    fn track(&mut self) -> TrackId {
        let spec = NewAudioTrack {
            name: None,
            hue: None,
            index: None,
            input: AudioInput::None,
        };
        let (project, id) = edits::add_audio_track(&self.project, spec).unwrap();
        self.project = project;
        id
    }

    /// Writes a 32-bit float WAV of `frames` frames, `f(frame, channel)`, and returns its link.
    fn wav(
        &self,
        name: &str,
        rate: u32,
        channels: u16,
        frames: usize,
        f: impl Fn(usize, usize) -> f32,
    ) -> AudioFile {
        let path = self.dir.join(name);
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..frames {
            for c in 0..usize::from(channels) {
                writer.write_sample(f(i, c)).unwrap();
            }
        }
        writer.finalize().unwrap();
        AudioFile::new(&path, None, frames as u64, rate, channels)
    }

    #[track_caller]
    fn clip(
        &mut self,
        track: TrackId,
        file: &AudioFile,
        start: i64,
        length: i64,
        offset: u64,
    ) -> ClipId {
        let spec = NewClip {
            start: Tick(start),
            length: Tick(length),
            name: "Clip".into(),
            content: NewClipContent::Audio {
                file: file.clone(),
                offset,
            },
        };
        let (project, id) = edits::add_clip(&self.project, track, spec).unwrap();
        self.project = project;
        let clip = self.project.find_clip(id).unwrap().1;
        assert_eq!(clip.length, Tick(length), "the file is long enough");
        id
    }

    #[track_caller]
    fn prop(&mut self, clip: ClipId, prop: AudioClipProp) {
        self.project = edits::set_audio_clip_prop(&self.project, clip, prop).unwrap();
    }

    fn program(&self) -> Program {
        compile::build(&self.project, None, &NoEnv)
    }
}

/// A distinctive, exactly representable test signal: left in -1 ..< 1, right its negative.
fn noise(i: usize, c: usize) -> f32 {
    let v = ((i * 37) % 1999) as f32 / 1000.0 - 0.999;
    if c == 0 { v } else { -v }
}

#[track_caller]
fn close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= 1e-5,
        "{what}: {actual} != {expected}"
    );
}

/// Rig playing `program` offline from `from` for `frames`.
fn offline(program: &Program, from: i64, frames: usize) -> Rig {
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(program);
    rig.send(Command::Locate(Tick(from)));
    rig.send(Command::Play);
    rig.render(frames, 512);
    rig
}

/// Waits (running stopped blocks like a device would) until the audio tracks are primed and their
/// heads exist.
fn wait_primed(rig: &mut Rig, program: &Program) {
    let start = Instant::now();
    loop {
        rig.process(512, 512);
        let heads = program
            .tracks
            .iter()
            .filter_map(|t| t.audio.as_ref())
            .all(|p| p.heads_ready());
        if heads && rig.handle.telemetry().audio_pending() == 0 {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never primed");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `frames` of realtime playback in blocks of 512, paced like a device (10 ms per block).
fn paced(rig: &mut Rig, frames: usize) -> std::ops::Range<usize> {
    let start = rig.frames();
    let mut done = 0;
    while done < frames {
        let n = 512.min(frames - done);
        rig.process_with(n, &[], 0);
        std::thread::sleep(Duration::from_millis(10));
        done += n;
    }
    start..rig.frames()
}

// ------------------------------------------------------------------------------------------------
// Start, offset, gain, fades
// ------------------------------------------------------------------------------------------------

#[test]
fn a_clip_starts_on_its_sample_and_plays_the_file_from_its_offset() {
    let mut s = Studio::new("start");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, BEAT, BEAT, 1000);
    let rig = offline(&s.program(), 0, 60_000);
    let start = BEAT_FRAMES;
    // Silence before, the 3 ms edge fade in, then the file at 1000 + j, exactly.
    assert!(rig.l[..start].iter().all(|&x| x == 0.0));
    assert_eq!(rig.l[start], 0.0);
    for j in [1, 50, 143] {
        close(
            rig.l[start + j],
            noise(1000 + j, 0) * j as f32 / EDGE as f32,
            "edge",
        );
    }
    for j in (EDGE..BEAT_FRAMES - EDGE).step_by(7) {
        close(rig.l[start + j], noise(1000 + j, 0), "left");
        close(rig.r[start + j], noise(1000 + j, 1), "right");
    }
    // The edge fade out ends on zero at the last frame; silence behind.
    close(rig.l[start + BEAT_FRAMES - 1], 0.0, "last frame");
    close(
        rig.l[start + BEAT_FRAMES - 1 - 72],
        noise(1000 + BEAT_FRAMES - 1 - 72, 0) * 72.0 / EDGE as f32,
        "fade out",
    );
    assert!(rig.l[start + BEAT_FRAMES..].iter().all(|&x| x == 0.0));
}

#[test]
fn mono_files_play_on_both_channels() {
    let mut s = Studio::new("mono");
    let track = s.track();
    let file = s.wav("mono.wav", 48_000, 1, 48_000, |i, _| noise(i, 0));
    s.clip(track, &file, 0, BEAT, 0);
    let rig = offline(&s.program(), 0, 2000);
    close(rig.l[500], noise(500, 0), "left");
    close(rig.r[500], noise(500, 0), "right");
}

#[test]
fn gain_and_fades_scale_the_clip() {
    let mut s = Studio::new("fades");
    let track = s.track();
    let file = s.wav("dc.wav", 48_000, 2, 96_000, |_, _| 0.8);
    let clip = s.clip(track, &file, 0, 2 * BEAT, 0);
    s.prop(clip, AudioClipProp::GainDb(-6.0));
    // 48 ticks = 1200 frames of fade in, 96 ticks = 2400 frames of fade out.
    s.prop(clip, AudioClipProp::FadeIn(Tick(48)));
    s.prop(clip, AudioClipProp::FadeOut(Tick(96)));
    let program = s.program();
    let seg = &program.tracks[0].audio.as_ref().unwrap().segments()[0];
    assert_eq!((seg.fade_in, seg.fade_out), (1200, 2400));
    let rig = offline(&program, 0, 52_000);
    let gain = db_to_gain(-6.0);
    let level = 0.8 * gain;
    // Fade in: j / 1200, with the edge fade on top in the first 144 frames.
    close(
        rig.l[100],
        level * (100.0 / 1200.0) * (100.0 / 144.0),
        "fade in + edge",
    );
    close(rig.l[600], level * 0.5, "half way in");
    close(rig.l[1200], level, "fade in done");
    close(rig.l[20_000], level, "middle");
    // Fade out: (len - 1 - j) / 2400.
    let len = 2 * BEAT_FRAMES;
    close(rig.l[len - 1 - 1200], level * 0.5, "half way out");
    close(
        rig.l[len - 1 - 50],
        level * (50.0 / 2400.0) * (50.0 / 144.0),
        "fade out + edge",
    );
    close(rig.l[len - 1], 0.0, "end");
    close(rig.r[600], level * 0.5, "right");
}

#[test]
fn the_lowest_gain_is_silence() {
    let mut s = Studio::new("silent_gain");
    let track = s.track();
    let file = s.wav("dc.wav", 48_000, 2, 48_000, |_, _| 0.5);
    let clip = s.clip(track, &file, 0, BEAT, 0);
    s.prop(
        clip,
        AudioClipProp::GainDb(komponist_model::CLIP_GAIN_DB_MIN),
    );
    let rig = offline(&s.program(), 0, 4000);
    assert!(rig.l.iter().all(|&x| x == 0.0));
}

#[test]
fn a_cut_edge_fades_out_and_the_clip_that_cuts_fades_in() {
    let mut s = Studio::new("cut");
    let track = s.track();
    let a = s.wav("a.wav", 48_000, 2, 96_000, |_, _| 0.5);
    let b = s.wav("b.wav", 48_000, 2, 96_000, |_, _| 0.25);
    s.clip(track, &a, BEAT, 2 * BEAT, 0);
    // The second clip covers the end of the first (the model cuts it back to two beats' start).
    s.clip(track, &b, 2 * BEAT, BEAT, 0);
    let rig = offline(&s.program(), 0, 80_000);
    let cut = 2 * BEAT_FRAMES;
    close(rig.l[cut - 1000], 0.5, "first clip");
    close(
        rig.l[cut - 1 - 71],
        0.5 * 71.0 / 144.0,
        "fade out before the cut",
    );
    close(rig.l[cut - 1], 0.0, "last frame of the first");
    close(rig.l[cut], 0.0, "first frame of the second");
    close(
        rig.l[cut + 71],
        0.25 * 71.0 / 144.0,
        "fade in after the cut",
    );
    close(rig.l[cut + 1000], 0.25, "second clip");
}

#[test]
fn a_split_clip_plays_through_the_cut_without_a_seam() {
    let mut s = Studio::new("split");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 120_000, noise);
    let clip = s.clip(track, &file, 0, 4 * BEAT, 500);
    let (project, _) = edits::split_clips(&s.project, &[clip], Tick(2 * BEAT)).unwrap();
    s.project = project;
    let program = s.program();
    let segments = program.tracks[0].audio.as_ref().unwrap().segments();
    assert_eq!(segments.len(), 2);
    assert!(
        !segments[0].edge_out && !segments[1].edge_in,
        "no edge fades at the join"
    );
    let rig = offline(&program, 0, 2 * BEAT_FRAMES + 2000);
    let at = 2 * BEAT_FRAMES;
    for j in at - 10..at + 10 {
        close(rig.l[j], noise(500 + j, 0), "across the cut");
    }
}

// ------------------------------------------------------------------------------------------------
// Re-Pitch and rate conversion
// ------------------------------------------------------------------------------------------------

/// Frequency in Hz of a sine in `x`, from the rising zero crossings.
fn frequency(x: &[f32], rate: f32) -> f32 {
    let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    (crossings as f32 - 1.0).max(0.0) / ((x.len() as f32) / rate)
}

fn sine(freq: f32, rate: u32) -> impl Fn(usize, usize) -> f32 {
    move |i, _| 0.5 * (std::f32::consts::TAU * freq * i as f32 / rate as f32).sin()
}

#[test]
fn re_pitch_plays_at_project_over_original_tempo() {
    let mut s = Studio::new("repitch");
    let track = s.track();
    let file = s.wav("sine.wav", 48_000, 2, 96_000, sine(480.0, 48_000));
    let clip = s.clip(track, &file, 0, 2 * BEAT, 0);
    let fixed = offline(&s.program(), 0, 60_000);
    let region = 1000..40_000 - 1000;
    let f = frequency(&fixed.l[region.clone()], RATE);
    assert!((f - 480.0).abs() < 3.0, "Fixed plays at its own speed: {f}");
    // Original tempo 100, project 120: speed 1.2.
    s.prop(clip, AudioClipProp::TimeMode(ClipTimeMode::RePitch));
    s.prop(clip, AudioClipProp::OriginalTempo(Tempo::new(100.0)));
    let program = s.program();
    assert!((program.tracks[0].audio.as_ref().unwrap().segments()[0].speed - 1.2).abs() < 1e-9);
    let rig = offline(&program, 0, 60_000);
    let f = frequency(&rig.l[region], RATE);
    assert!(
        (f - 576.0).abs() < 4.0,
        "Re-Pitch speeds the pitch up by 1.2: {f}"
    );
    let peak = rig.l[2000..30_000]
        .iter()
        .fold(0.0f32, |p, x| p.max(x.abs()));
    assert!((0.48..0.52).contains(&peak), "amplitude kept: {peak}");
    // The clip keeps its length in ticks: it ends at two beats.
    assert!(rig.l[2 * BEAT_FRAMES + 200..].iter().all(|&x| x == 0.0));
}

#[test]
fn files_at_another_rate_are_converted_to_the_engine_rate() {
    let mut s = Studio::new("rate");
    let track = s.track();
    let file = s.wav("sine44.wav", 44_100, 2, 88_200, sine(441.0, 44_100));
    s.clip(track, &file, 0, 2 * BEAT, 0);
    let rig = offline(&s.program(), 0, 60_000);
    let f = frequency(&rig.l[1000..39_000], RATE);
    assert!((f - 441.0).abs() < 3.0, "pitch is kept: {f}");
    let peak = rig.l[1000..39_000]
        .iter()
        .fold(0.0f32, |p, x| p.max(x.abs()));
    assert!((0.49..0.51).contains(&peak), "level is kept: {peak}");
    // The clip is two beats long at the engine rate, not at the file's.
    assert!(rig.l[2 * BEAT_FRAMES + 200..].iter().all(|&x| x == 0.0));
    assert!(rig.l[2 * BEAT_FRAMES - 3000].abs() > 0.0 || rig.l[2 * BEAT_FRAMES - 3001].abs() > 0.0);
}

// ------------------------------------------------------------------------------------------------
// Locate and loop
// ------------------------------------------------------------------------------------------------

#[test]
fn playback_from_the_middle_of_a_clip_starts_at_the_right_source_position() {
    let mut s = Studio::new("locate");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, BEAT, 2 * BEAT, 1000);
    // Beat 2 + 1000 frames: 1000 + 24000 + 1000 into the file.
    let from = Tick(2 * BEAT + 40);
    let rig = offline(&s.program(), from.0, 3000);
    let source = 1000 + BEAT_FRAMES + 1000;
    for j in (0..3000).step_by(11) {
        close(rig.l[j], noise(source + j, 0), "mid-clip");
    }
}

#[test]
fn a_locate_while_playing_lands_on_the_same_position() {
    let mut s = Studio::new("locate_live");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, 0, 4 * BEAT, 0);
    let program = s.program();
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(&program);
    rig.send(Command::Play);
    rig.render(2000, 512);
    rig.send(Command::Locate(Tick(3 * BEAT)));
    let range = rig.render(2000, 512);
    // Offline renders do the streamer's work in line: the new position plays at once, and the
    // 3 ms fade-in of a jump is not applied offline.
    // (The first 3 ms also hold the old position's fade-out tail.)
    for j in (EDGE..2000).step_by(13) {
        close(
            rig.l[range.start + j],
            noise(3 * BEAT_FRAMES + j, 0),
            "after the locate",
        );
    }
}

#[test]
fn the_loop_wraps_without_a_gap_and_with_edge_fades() {
    let mut s = Studio::new("loop");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, 0, 4 * BEAT, 0);
    s.project.loop_on = true;
    s.project.loop_range = TickRange::new(Tick(BEAT), Tick(3 * BEAT));
    let rig = offline(&s.program(), BEAT, 4 * BEAT_FRAMES + 5000);
    let lap = 2 * BEAT_FRAMES;
    for k in 0..2 {
        let base = k * lap;
        // Source position = loop start + j, whichever lap.
        for j in (EDGE..lap - EDGE).step_by(17) {
            if k == 0 || j > EDGE {
                close(rig.l[base + j], noise(BEAT_FRAMES + j, 0), "lap");
            }
        }
    }
    // At the wrap: fade out over the last 144 frames of the lap, fade in over the first 144.
    close(rig.l[lap - 1], 0.0, "last frame before the wrap");
    close(
        rig.l[lap - 1 - 72],
        noise(BEAT_FRAMES + lap - 1 - 72, 0) * 72.0 / EDGE as f32,
        "fade out",
    );
    close(rig.l[lap], 0.0, "first frame after the wrap");
    close(
        rig.l[lap + 72],
        noise(BEAT_FRAMES + 72, 0) * 72.0 / EDGE as f32,
        "fade in",
    );
}

#[test]
fn a_jump_to_a_segment_start_plays_from_its_head_before_the_streamer_catches_up() {
    let mut s = Studio::new("head");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, BEAT, BEAT, 2000);
    let program = s.program();
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(&program);
    wait_primed(&mut rig, &program);
    let (underruns, telemetry) = (
        rig.handle.telemetry().audio_underruns(),
        rig.handle.telemetry().clone(),
    );
    // Hold the read-ahead, jump to the clip start: only the head can play now.
    rig.engine.stall_streaming(true);
    rig.send(Command::Locate(Tick(BEAT)));
    rig.send(Command::Play);
    let range = rig.process(512, 512);
    assert_eq!(
        telemetry.audio_underruns(),
        underruns,
        "the head covers the start"
    );
    for j in 1..512 {
        let edge = (j as f32 / EDGE as f32).min(1.0);
        close(rig.l[range.start + j], noise(2000 + j, 0) * edge, "head");
    }
    // Further than the head (100 ms): silence, counted, no block.
    rig.process(8192, 512);
    assert!(telemetry.audio_underruns() > underruns);
    rig.engine.stall_streaming(false);
}

#[test]
fn what_the_streamer_cannot_deliver_is_silence_and_counted_and_playback_recovers_in_position() {
    let mut s = Studio::new("underrun");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, 0, 4 * BEAT, 0);
    let program = s.program();
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(&program);
    wait_primed(&mut rig, &program);
    let telemetry = rig.handle.telemetry().clone();
    rig.engine.stall_streaming(true);
    // Mid-clip: no head covers it.
    rig.send(Command::Locate(Tick(2 * BEAT)));
    rig.send(Command::Play);
    let started = Instant::now();
    let range = rig.process(2048, 512);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "never blocks"
    );
    assert!(rig.l[range.clone()].iter().all(|&x| x == 0.0), "silence");
    assert!(telemetry.audio_underruns() >= 4);
    // The disk comes back: the sound continues at the position the playhead is at.
    rig.engine.stall_streaming(false);
    let mut position = 2 * BEAT_FRAMES + 2048;
    let mut heard = false;
    for _ in 0..200 {
        let range = rig.process(512, 512);
        for j in 0..512 {
            let x = rig.l[range.start + j];
            if x != 0.0 {
                close(x, noise(position + j, 0), "recovered in position");
                heard = true;
            }
        }
        position += 512;
        std::thread::sleep(Duration::from_millis(5));
        if heard {
            break;
        }
    }
    assert!(heard, "the stream recovers");
}

#[test]
fn realtime_playback_equals_the_offline_render_across_a_loop() {
    let mut s = Studio::new("realtime_loop");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 120_000, noise);
    s.clip(track, &file, 0, 4 * BEAT, 300);
    s.project.loop_on = true;
    s.project.loop_range = TickRange::new(Tick(0), Tick(2 * BEAT));
    // One program per engine: a plan's ring belongs to the engine that plays it.
    let reference = offline(&s.program(), 0, 4 * BEAT_FRAMES);
    let program = s.program();
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(&program);
    wait_primed(&mut rig, &program);
    let telemetry = rig.handle.telemetry().clone();
    rig.send(Command::Locate(Tick(0)));
    rig.send(Command::Play);
    let range = paced(&mut rig, 4 * BEAT_FRAMES);
    assert_eq!(telemetry.audio_underruns(), 0);
    // After the 3 ms fade-in of the start (offline renders have none), the two are the same.
    for i in EDGE..range.len() {
        assert_eq!(rig.l[range.start + i], reference.l[i], "frame {i}");
        assert_eq!(rig.r[range.start + i], reference.r[i], "frame {i}");
    }
}

// ------------------------------------------------------------------------------------------------
// Export, routing, files
// ------------------------------------------------------------------------------------------------

fn export_job(path: PathBuf, frames: u64) -> ExportJob {
    ExportJob {
        path,
        sample_rate: 48_000,
        range: 0..frames,
        tail: 0,
        bit_depth: BitDepth::Float32,
        normalize: None,
        dither: false,
    }
}

#[test]
fn the_export_equals_a_realtime_run() {
    let mut s = Studio::new("export");
    let t1 = s.track();
    let t2 = s.track();
    let a = s.wav("a.wav", 48_000, 2, 96_000, noise);
    let b = s.wav("b.wav", 44_100, 2, 88_200, sine(300.0, 44_100));
    // Gain, fades, a Re-Pitch clip and a rate conversion in one song.
    let c1 = s.clip(t1, &a, BEAT, 2 * BEAT, 700);
    s.prop(c1, AudioClipProp::GainDb(-3.0));
    s.prop(c1, AudioClipProp::FadeIn(Tick(100)));
    let c2 = s.clip(t2, &b, 2 * BEAT, 2 * BEAT, 0);
    s.prop(c2, AudioClipProp::TimeMode(ClipTimeMode::RePitch));
    s.prop(c2, AudioClipProp::OriginalTempo(Tempo::new(110.0)));
    let program = s.program();
    let frames = 5 * BEAT_FRAMES;
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(&program);
    wait_primed(&mut rig, &program);
    let dir = s.dir.clone();
    let path = dir.join("out.wav");
    let cancel = AtomicBool::new(false);
    let job = export_job(path.clone(), frames as u64);
    export(
        &mut rig.engine,
        &mut rig.handle,
        &program,
        &job,
        &cancel,
        |_| {},
    )
    .unwrap();
    let mut reader = hound::WavReader::open(&path).unwrap();
    let exported: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(exported.len(), 2 * frames);
    assert!(exported.iter().any(|&x| x.abs() > 0.1));
    // The export restored the program and the position: play the song in realtime.
    rig.process(1024, 512);
    rig.send(Command::Stop);
    rig.send(Command::Locate(Tick(0)));
    let telemetry = rig.handle.telemetry().clone();
    wait_primed(&mut rig, &program);
    rig.send(Command::Play);
    let range = paced(&mut rig, frames);
    assert_eq!(telemetry.audio_underruns(), 0);
    for i in 0..frames {
        assert_eq!(rig.l[range.start + i], exported[2 * i], "left {i}");
        assert_eq!(rig.r[range.start + i], exported[2 * i + 1], "right {i}");
    }
}

#[test]
fn an_audio_track_routes_through_buses_and_delay_compensation() {
    let mut s = Studio::new("routing");
    let direct = s.track();
    let late = s.track();
    let impulse = s.wav("impulse.wav", 48_000, 2, 48_000, |i, _| {
        if i == 1000 { 1.0 } else { 0.0 }
    });
    s.clip(direct, &impulse, BEAT, BEAT, 0);
    s.clip(late, &impulse, BEAT, BEAT, 0);
    // `late` runs through an effect that reports 200 frames; `direct` goes through a group bus.
    let spec = NewDevice {
        role: DeviceRole::Effect,
        plugin: PluginRef {
            format: PluginFormat::Clap,
            id: "test.fx".into(),
            name: "fx".into(),
            vendor: "Test".into(),
            version: "1.0".into(),
            module: Box::default(),
        },
        state: None,
        sidechain: None,
    };
    let (project, device) =
        edits::insert_device(&s.project, ChannelRef::Track(late), 0, spec).unwrap();
    s.project = project;
    let (project, group) = edits::add_bus(&s.project, edits::NewBus::group(vec![direct])).unwrap();
    s.project = project;
    assert_eq!(
        s.project.track(direct).unwrap().output,
        TrackOutput::Bus(group)
    );
    struct Latency(komponist_model::DeviceId);
    impl compile::CompileEnv for Latency {
        fn midi_port(&self, _: &str) -> Option<komponist_engine::MidiPort> {
            None
        }
        fn device_latency(&self, device: komponist_model::DeviceId) -> u32 {
            if device == self.0 { 200 } else { 0 }
        }
    }
    let env = Latency(device);
    let program = compile::build(&s.project, None, &env);
    let mut rig = Rig::new(RATE, 512);
    for (_, slot) in compile::slot_changes(None, &program).added {
        rig.insert(slot.0, TestGain::with_latency(1.0, 200));
    }
    rig.set_program(&program);
    rig.send(Command::Play);
    rig.render(BEAT_FRAMES * 2, 512);
    let hits: Vec<usize> = (0..rig.l.len()).filter(|&i| rig.l[i] != 0.0).collect();
    // The direct track is delayed by 200 frames to line up with the track behind the effect.
    assert_eq!(hits, [BEAT_FRAMES + 1000 + 200]);
    close(rig.l[hits[0]], 2.0, "both impulses arrive together");
}

#[test]
fn playing_clips_never_allocate_on_the_audio_thread() {
    let mut s = Studio::new("alloc");
    let t1 = s.track();
    let t2 = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    let b = s.wav("b.wav", 44_100, 1, 44_100, sine(200.0, 44_100));
    s.clip(t1, &file, 0, 4 * BEAT, 0);
    let c = s.clip(t2, &b, BEAT, BEAT, 0);
    s.prop(c, AudioClipProp::TimeMode(ClipTimeMode::RePitch));
    s.project.loop_on = true;
    s.project.loop_range = TickRange::new(Tick(0), Tick(3 * BEAT));
    let program = s.program();
    let mut rig = Rig::new(RATE, 333);
    rig.set_program(&program);
    wait_primed(&mut rig, &program);
    // Every call of the rig runs inside `assert_no_alloc`, which aborts on an allocation or a
    // free: play, loop wraps, locates, stop and a program swap, with odd block sizes.
    rig.send(Command::Play);
    paced(&mut rig, 20_000);
    rig.send(Command::Locate(Tick(BEAT + 100)));
    paced(&mut rig, 5000);
    s.project.loop_range = TickRange::new(Tick(BEAT), Tick(2 * BEAT));
    rig.set_program(&s.program());
    paced(&mut rig, 5000);
    rig.send(Command::Stop);
    rig.process(2000, 333);
    rig.send(Command::Locate(Tick(0)));
    rig.send(Command::Play);
    paced(&mut rig, 5000);
    rig.render(3000, 333);
}

#[test]
fn a_missing_file_plays_silence_reports_its_status_and_stops_an_export() {
    let mut s = Studio::new("missing");
    let track = s.track();
    let file = s.wav("gone.wav", 48_000, 2, 48_000, noise);
    let clip = s.clip(track, &file, 0, BEAT, 0);
    std::fs::remove_file(&file.abs).unwrap();
    let program = s.program();
    let source = program.clip_source(track, clip).unwrap();
    assert_eq!(source.status(), SourceStatus::Pending);
    let mut rig = Rig::new(RATE, 512);
    rig.set_program(&program);
    rig.send(Command::Play);
    rig.render(4000, 512);
    assert!(rig.l.iter().all(|&x| x == 0.0));
    assert_eq!(source.status(), SourceStatus::Missing);
    assert_eq!(program.audio_sources().len(), 1);
    let cancel = AtomicBool::new(false);
    let job = export_job(s.dir.join("never.wav"), 4000);
    let result = export(
        &mut rig.engine,
        &mut rig.handle,
        &program,
        &job,
        &cancel,
        |_| {},
    );
    assert!(matches!(result, Err(ExportError::MissingAudio(p)) if p == file.abs));
    assert!(!s.dir.join("never.wav").exists());
}

#[test]
fn export_revalidates_a_file_that_was_ready_before_it_disappeared() {
    let mut s = Studio::new("removed-after-ready");
    let track = s.track();
    let file = s.wav("gone.wav", 48_000, 2, 48_000, noise);
    let clip = s.clip(track, &file, 0, BEAT, 0);
    let program = s.program();
    let source = program.clip_source(track, clip).unwrap();
    assert_eq!(source.open_now(), SourceStatus::Ready);
    std::fs::remove_file(&file.abs).unwrap();
    let mut rig = Rig::new(RATE, 512);
    let job = export_job(s.dir.join("never.wav"), 4000);
    let result = export(
        &mut rig.engine,
        &mut rig.handle,
        &program,
        &job,
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(matches!(result, Err(ExportError::MissingAudio(p)) if p == file.abs));
    assert_eq!(source.status(), SourceStatus::Missing);
    assert!(!job.path.exists());
}

#[test]
fn a_recompile_keeps_the_plan_while_the_clips_and_the_tempo_stay() {
    let mut s = Studio::new("incremental");
    let track = s.track();
    let file = s.wav("noise.wav", 48_000, 2, 96_000, noise);
    s.clip(track, &file, 0, BEAT, 0);
    let first = s.program();
    // A fader move changes the track, not its clips: same plan, same ring, same open file.
    s.project = edits::set_track_prop(
        &s.project,
        &[track],
        edits::TrackProp::Channel(edits::ChannelProp::VolumeDb(-3.0)),
    )
    .unwrap();
    let second = compile::build(&s.project, Some(&first), &NoEnv);
    let (a, b) = (
        first.tracks[0].audio.as_ref().unwrap(),
        second.tracks[0].audio.as_ref().unwrap(),
    );
    assert!(std::sync::Arc::ptr_eq(a, b));
    // Another clip: a new plan that shares the file.
    s.clip(track, &file, 2 * BEAT, BEAT, 0);
    let third = compile::build(&s.project, Some(&second), &NoEnv);
    let c = third.tracks[0].audio.as_ref().unwrap();
    assert!(!std::sync::Arc::ptr_eq(a, c));
    assert!(std::sync::Arc::ptr_eq(&a.sources()[0], &c.sources()[0]));
    assert_eq!(c.segments().len(), 2);
}

// ------------------------------------------------------------------------------------------------
// Performance
// ------------------------------------------------------------------------------------------------

/// `tracks` audio tracks, each with one clip of 8 s from one of four 20 s stereo files (two at 48
/// kHz, two at 44.1 kHz, so half of the voices convert), looped over bars 1–4 (8 s): the audio
/// thread's cost per 256-frame block and the read-ahead thread's load, paced like a device.
/// `KOMPONIST_BENCH_AUDIO_TRACKS` sets the track counts (default `16,64`).
///
/// ```text
/// cargo test --release -p komponist-engine --test engine audio::bench -- --ignored --nocapture
/// ```
#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn bench_audio_streams() {
    const BLOCK: usize = 256;
    const SECONDS: usize = 20;
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    let counts: Vec<usize> = std::env::var("KOMPONIST_BENCH_AUDIO_TRACKS")
        .ok()
        .map(|s| s.split(',').filter_map(|w| w.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![16, 64]);
    for tracks in counts {
        let mut s = Studio::new("bench");
        let files: Vec<AudioFile> = (0..4)
            .map(|k| {
                let rate = if k % 2 == 0 { 48_000 } else { 44_100 };
                s.wav(
                    &format!("f{k}.wav"),
                    rate,
                    2,
                    rate as usize * 20,
                    move |i, c| noise(i + k * 101, c) * 0.1,
                )
            })
            .collect();
        for k in 0..tracks {
            let track = s.track();
            s.clip(
                track,
                &files[k % 4],
                0,
                8 * 2 * BEAT,
                (k as u64 * 997) % 5000,
            );
        }
        s.project.loop_on = true;
        s.project.loop_range = TickRange::new(Tick(0), Tick(8 * 2 * BEAT));
        let program = s.program();
        let mut rig = Rig::new(RATE, BLOCK);
        rig.set_program(&program);
        wait_primed(&mut rig, &program);
        rig.send(Command::Play);
        let stats = rig.engine.stream_stats();
        let telemetry = rig.handle.telemetry().clone();
        let (frames0, busy0) = (stats.frames(), stats.busy());
        let blocks = SECONDS * 48_000 / BLOCK;
        rig.l.reserve(blocks * BLOCK + 4096);
        rig.r.reserve(blocks * BLOCK + 4096);
        let (mut total, mut worst) = (Duration::ZERO, Duration::ZERO);
        let (mut late, mut worst_at) = (0, 0);
        let began = Instant::now();
        for b in 0..blocks {
            let due = began + period * b as u32;
            let t0 = Instant::now();
            rig.process_with(BLOCK, &[], 0);
            let t = t0.elapsed();
            total += t;
            late += usize::from(t > period);
            if t > worst {
                (worst, worst_at) = (t, b);
            }
            if let Some(wait) = due
                .checked_add(period)
                .and_then(|d| d.checked_duration_since(Instant::now()))
            {
                std::thread::sleep(wait);
            }
        }
        let wall = began.elapsed();
        let busy = stats.busy() - busy0;
        let frames = stats.frames() - frames0;
        let mean = total / blocks as u32;
        eprintln!(
            "{tracks} streamed stereo clips (half converted from 44.1 kHz): audio thread mean {mean:.1?} ({:.2} %), worst {worst:.1?} ({:.1} %, block {worst_at}) per {BLOCK} frames, {late} of {blocks} blocks longer than the period; \
             read-ahead thread busy {:.1} % of {:.1?}, {:.0} frames/s per voice (realtime 48000); underruns {}",
            mean.as_secs_f64() / period.as_secs_f64() * 100.0,
            worst.as_secs_f64() / period.as_secs_f64() * 100.0,
            busy.as_secs_f64() / wall.as_secs_f64() * 100.0,
            wall,
            frames as f64 / tracks as f64 / wall.as_secs_f64(),
            telemetry.audio_underruns(),
        );
    }
}
