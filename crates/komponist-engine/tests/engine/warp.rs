//! Generated sources only. Every Rig callback/render remains inside assert_no_alloc.
use super::audio::{Studio, frequency, paced, sine, wait_primed};
use crate::common::*;
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::export::{self, BitDepth, ExportError, ExportJob};
use komponist_engine::{Command, Program};
use komponist_model::{AudioTake, ClipId, DeviceId, Tempo, Tick, TickRange, edits};
use std::sync::atomic::AtomicBool;

struct Env(u32);
impl CompileEnv for Env {
    fn midi_port(&self, _: &str) -> Option<komponist_engine::program::MidiPort> {
        None
    }
    fn device_latency(&self, _: DeviceId) -> u32 {
        0
    }
    fn sample_rate(&self) -> f64 {
        f64::from(self.0)
    }
}
fn setup(
    name: &str,
    source_rate: u32,
    offset: u64,
    original: f64,
    project: f64,
) -> (Studio, ClipId) {
    let mut s = Studio::new(name);
    s.project = edits::set_project_prop(&s.project, edits::ProjectProp::Tempo(Tempo::new(project)))
        .unwrap();
    let track = s.track();
    let file = s.wav(
        "tone.wav",
        source_rate,
        2,
        source_rate as usize * 4 + offset as usize,
        sine(440.0, source_rate),
    );
    let c = s.clip(track, &file, 0, 960, offset);
    s.prop(c, edits::AudioClipProp::OriginalTempo(Tempo::new(original)));
    s.project = edits::set_audio_clip_warp(&s.project, c, true).unwrap();
    s.project = edits::set_clip_length(
        &s.project,
        c,
        Tick((original * 960.0 * 2.0 / 60.0).round() as i64),
    )
    .unwrap();
    (s, c)
}
fn play(program: &Program, rate: u32, from: Tick, frames: usize, block: usize) -> Rig {
    let mut r = Rig::new(rate as f32, 1024);
    r.set_program(program);
    r.send(Command::Locate(from));
    r.send(Command::Play);
    r.render(frames, block);
    r
}
fn pitch(x: &[f32], rate: u32, expected: f32) {
    let f = frequency(x, rate as f32);
    assert!((f - expected).abs() < 8.0, "pitch {f}, expected {expected}");
    assert!(x.iter().all(|v| v.is_finite()));
    assert!(x.iter().fold(0.0f32, |p, x| p.max(x.abs())) > 0.1);
}
fn job(path: std::path::PathBuf, rate: u32, range: std::ops::Range<u64>) -> ExportJob {
    ExportJob {
        path,
        sample_rate: rate,
        range,
        tail: 0,
        bit_depth: BitDepth::Float32,
        normalize: None,
        dither: false,
    }
}

#[test]
fn pitch_duration_rate_conversion_offsets_and_block_sizes_share_one_transform() {
    for (i, (source_rate, original, project, rate)) in [
        (44100, 90.0, 120.0, 48000),
        (48000, 180.0, 120.0, 44100),
        (48000, 40.0, 240.0, 48000),
        (48000, 240.0, 40.0, 48000),
        (96000, 120.0, 120.0, 96000),
    ]
    .into_iter()
    .enumerate()
    {
        let (s, _) = setup(
            &format!("warp-pitch-{i}"),
            source_rate,
            source_rate as u64 / 10,
            original,
            project,
        );
        let frames = (2.0 * original / project * f64::from(rate)).round() as usize;
        let program = compile::build(&s.project, None, &Env(rate));
        assert_eq!(
            program.tracks[0].audio.as_ref().unwrap().segments()[0].end,
            frames as i64
        );
        let a = play(&program, rate, Tick::ZERO, frames + 2048, 64);
        let b = play(
            &compile::build(&s.project, None, &Env(rate)),
            rate,
            Tick::ZERO,
            frames + 2048,
            1024,
        );
        assert_eq!(a.l, b.l, "block-dependent left, case {i}");
        assert_eq!(a.r, b.r, "block-dependent right, case {i}");
        pitch(&a.l[frames / 5..frames * 4 / 5], rate, 440.0);
        assert!(a.l[frames..].iter().all(|&v| v == 0.0));
    }
}

#[test]
fn seamless_splits_keep_spectral_state_and_gain_fades_remain_after_stretching() {
    let (mut s, c) = setup("warp-split", 44100, 4410, 180.0, 120.0);
    let original = play(&s.program(), 48000, Tick::ZERO, 150000, 512);
    s.project = edits::split_clips(&s.project, &[c], Tick(137)).unwrap().0;
    let p = s.program();
    let segments = p.tracks[0].audio.as_ref().unwrap().segments();
    assert!(!segments[0].edge_out && !segments[1].edge_in);
    let split = play(&p, 48000, Tick::ZERO, 150000, 64);
    assert_eq!(
        original.l, split.l,
        "split reset spectral history or lost chunk overhang"
    );
    let (mut s, c) = setup("warp-envelope", 48000, 0, 180.0, 120.0);
    let raw = play(&s.program(), 48000, Tick::ZERO, 144000, 1024);
    s.prop(c, edits::AudioClipProp::GainDb(-6.0));
    s.prop(c, edits::AudioClipProp::FadeIn(Tick(48)));
    s.prop(c, edits::AudioClipProp::FadeOut(Tick(96)));
    let faded = play(&s.program(), 48000, Tick::ZERO, 144000, 64);
    let gain = 10.0f32.powf(-6.0 / 20.0);
    for j in [500, 600, 1200, 10000, 140000, 143000] {
        let g = gain * (j as f32 / 1200.0).min(1.0) * ((143999 - j) as f32 / 2400.0).min(1.0);
        assert!(
            (faded.l[j] - raw.l[j] * g).abs() < 2e-5,
            "envelope frame {j}"
        );
    }
}

#[test]
fn a_real_native_frame_gap_keeps_the_warp_cut_and_source_offset() {
    let mut s = Studio::new("warp-seam-native-gap");
    let track = s.track();
    let file = s.wav("source.wav", 48000, 2, 96000, sine(440.0, 48000));
    let left = s.clip(track, &file, 0, 960, 0);
    let right = s.clip(track, &file, 960, 960, 24000);
    for clip in [left, right] {
        s.project = edits::set_audio_clip_warp(&s.project, clip, true).unwrap();
    }
    let joined = s.program();
    assert!(!joined.tracks[0].audio.as_ref().unwrap().segments()[1].edge_in);
    let continuous = play(&joined, 48000, Tick::ZERO, 48000, 512);
    s.prop(right, edits::AudioClipProp::SourceOffset(24003));
    let gapped = s.program();
    let segments = gapped.tracks[0].audio.as_ref().unwrap().segments();
    assert!(segments[0].edge_out && segments[1].edge_in);
    let sequential = play(&gapped, 48000, Tick::ZERO, 48000, 512);
    let located = play(&s.program(), 48000, Tick(960), 24000, 512);
    assert_eq!(&sequential.l[24000..], &located.l);
    assert_ne!(&sequential.l[24144..], &continuous.l[24144..]);
}

#[test]
fn repeated_unaligned_loops_and_locates_reprime_deterministically() {
    let (mut s, _) = setup("warp-loop", 44100, 441, 180.0, 120.0);
    s.project.loop_on = true;
    s.project.loop_range = TickRange::new(Tick(137), Tick(877));
    let frames = 740 * 25;
    let a = play(&s.program(), 48000, Tick(137), frames * 3, 64);
    let b = play(&s.program(), 48000, Tick(137), frames * 3, 1024);
    assert_eq!(a.l, b.l);
    assert_eq!(
        &a.l[144..frames - 144],
        &a.l[frames + 144..2 * frames - 144]
    );
    let p = s.program();
    let mut r = play(&p, 48000, Tick(137), 12001, 512);
    r.send(Command::Locate(Tick(137)));
    let range = r.render(frames, 64);
    assert_eq!(
        &r.l[range.start + 144..range.start + frames - 144],
        &a.l[144..frames - 144]
    );
}

#[test]
fn real_time_heads_overhang_and_underruns_do_not_call_the_native_api() {
    let (s, _) = setup("warp-head", 48000, 0, 180.0, 120.0);
    let reference = play(&s.program(), 48000, Tick::ZERO, 20000, 64);
    let p = s.program();
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&p);
    r.send(Command::Locate(Tick(960)));
    wait_primed(&mut r, &p);
    r.engine.stall_streaming(true);
    let before = komponist_stretch::thread_native_calls();
    r.send(Command::Locate(Tick::ZERO));
    r.send(Command::Play);
    let start = r.process(4800, 64);
    assert_eq!(&r.l[start], &reference.l[..4800]);
    assert_eq!(r.handle.telemetry().audio_underruns(), 0);
    r.process(4096, 1024);
    assert!(r.handle.telemetry().audio_underruns() > 0);
    assert_eq!(
        komponist_stretch::thread_native_calls(),
        before,
        "native API on audio callback"
    );
    r.engine.stall_streaming(false);
}

#[test]
fn export_uses_identical_chunks_and_failure_never_commits_a_silent_file() {
    let (s, _) = setup("warp-export", 44100, 4410, 180.0, 120.0);
    let reference = play(&s.program(), 48000, Tick::ZERO, 144000, 64);
    let p = s.program();
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&p);
    let path = s.dir.join("mix.wav");
    let result = export::export(
        &mut r.engine,
        &mut r.handle,
        &p,
        &job(path.clone(), 48000, 0..144000),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.frames, 144000);
    let wav = super::export::read(&path);
    assert_eq!(
        wav.samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| c[0])
            .collect::<Vec<_>>(),
        reference.l
    );
    let mut s = Studio::new("warp-export-bad");
    let t = s.track();
    let file = s.wav("bad.wav", 48000, 2, 96000, |_, _| f32::NAN);
    let c = s.clip(t, &file, 0, 960, 0);
    s.project = edits::set_audio_clip_warp(&s.project, c, true).unwrap();
    let p = s.program();
    let mut r = Rig::new(RATE, 512);
    r.set_program(&p);
    let path = s.dir.join("failed.wav");
    let result = export::export(
        &mut r.engine,
        &mut r.handle,
        &p,
        &job(path.clone(), 48000, 0..24000),
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(
        matches!(result, Err(ExportError::Stretch { .. })),
        "{result:?}"
    );
    assert!(!path.exists());
    assert!(!s.dir.join(".failed.wav.part").exists());
}

#[test]
fn export_never_reuses_a_failed_live_ring() {
    let mut s = Studio::new("warp-export-failed-live-ring");
    let track = s.track();
    let file = s.wav("bad.wav", 48000, 2, 48000, |_, _| f32::NAN);
    let clip = s.clip(track, &file, 0, 960, 0);
    s.project = edits::set_audio_clip_warp(&s.project, clip, true).unwrap();
    let program = s.program();
    let plan = program.tracks[0].audio.as_ref().unwrap();
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&program);
    r.process(64, 64);
    let stats = r.engine.stream_stats();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while stats.frames() < 24000 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(stats.frames() >= 24000);
    assert!(plan.stretch_failures() > 0);
    let out = s.dir.join("out.wav");
    let result = export::export(
        &mut r.engine,
        &mut r.handle,
        &program,
        &job(out.clone(), 48000, 0..24000),
        &AtomicBool::new(false),
        |_| {},
    );
    assert!(matches!(result, Err(ExportError::Stretch { clip: failed, .. }) if failed == clip));
    assert!(!out.exists());
}

#[test]
fn export_of_a_clean_later_range_ignores_invalid_input_near_the_clip_start() {
    let mut s = Studio::new("warp-export-clean-subrange");
    let track = s.track();
    let file = s.wav("partial.wav", 48000, 2, 192000, |i, _| {
        if i < 18000 { f32::NAN } else { 0.25 }
    });
    let clip = s.clip(track, &file, 0, 3840, 0);
    s.project = edits::set_audio_clip_warp(&s.project, clip, true).unwrap();
    let program = s.program();
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&program);
    let mut first = true;
    let out = s.dir.join("out.wav");
    export::export(
        &mut r.engine,
        &mut r.handle,
        &program,
        &job(out.clone(), 48000, 48000..72000),
        &AtomicBool::new(false),
        |_| {
            if std::mem::take(&mut first) {
                // Give the asynchronous worker time to prepare heads. Export must not use it.
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        },
    )
    .unwrap();
    assert!(super::export::read(&out).samples.iter().any(|&v| v > 0.1));
}

#[test]
fn a_decoder_error_during_warp_export_is_not_end_of_file() {
    let (s, clip) = setup("warp-export-truncated-source", 48000, 0, 120.0, 120.0);
    let program = s.program();
    let mut r = Rig::new(RATE, 1024);
    r.engine.stall_streaming(true);
    r.set_program(&program);
    let source = std::fs::OpenOptions::new()
        .write(true)
        .open(s.dir.join("tone.wav"))
        .unwrap();
    let out = s.dir.join("out.wav");
    let mut first = true;
    let result = export::export(
        &mut r.engine,
        &mut r.handle,
        &program,
        &job(out.clone(), 48000, 0..96000),
        &AtomicBool::new(false),
        |_| {
            if std::mem::take(&mut first) {
                // Keep the WAV header but remove samples after the decoder opened it.
                source.set_len(4096).unwrap();
            }
        },
    );
    assert!(matches!(
        result,
        Err(ExportError::Stretch {
            clip: failed,
            reason: komponist_engine::audio::StretchError::SourceRead,
        }) if failed == clip
    ));
    assert!(!out.exists());
}

#[test]
fn take_and_warp_changes_replace_worker_history_without_touching_fixed_or_repitch() {
    let (mut s, c) = setup("warp-edits", 48000, 0, 180.0, 120.0);
    let p = s.program();
    let mut r = play(&p, 48000, Tick::ZERO, 8000, 512);
    let other = s.wav("other.wav", 44100, 1, 44100 * 4, sine(880.0, 44100));
    s.project = edits::add_audio_take(
        &s.project,
        c,
        AudioTake {
            name: "High".into(),
            file: other,
            offset: 4410,
        },
    )
    .unwrap()
    .0;
    let changed = compile::build(&s.project, Some(&p), &Env(48000));
    r.set_program(&changed);
    let range = r.render(20000, 64);
    pitch(&r.l[range.start + 2000..range.end - 1000], 48000, 880.0);
    s.project =
        edits::set_project_prop(&s.project, edits::ProjectProp::Tempo(Tempo::new(180.0))).unwrap();
    let changed = compile::build(&s.project, Some(&changed), &Env(48000));
    r.set_program(&changed);
    let range = r.render(20000, 1024);
    pitch(&r.l[range.start + 2000..range.end - 1000], 48000, 880.0);
    assert!(
        changed.tracks[0].audio.as_ref().unwrap().segments()[0]
            .warp
            .is_some()
    );
    s.project = edits::set_audio_clip_warp(&s.project, c, false).unwrap();
    assert!(
        s.program().tracks[0].audio.as_ref().unwrap().segments()[0]
            .warp
            .is_none()
    );
}

#[test]
fn cached_warp_heads_apply_the_same_loop_fades_without_a_worker() {
    let (mut s, _) = setup("warp-head-loop", 48000, 0, 180.0, 120.0);
    s.project.loop_on = true;
    s.project.loop_range = TickRange::new(Tick::ZERO, Tick(137));
    let frames = 137 * 25 * 4;
    let reference = play(&s.program(), 48000, Tick::ZERO, frames, 1024);
    let p = s.program();
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&p);
    r.send(Command::Locate(Tick(960)));
    wait_primed(&mut r, &p);
    r.engine.stall_streaming(true);
    let before = komponist_stretch::thread_native_calls();
    r.send(Command::Locate(Tick::ZERO));
    r.send(Command::Play);
    let range = r.process(frames, 64);
    assert_eq!(&r.l[range], reference.l);
    assert_eq!(r.handle.telemetry().audio_underruns(), 0);
    assert_eq!(komponist_stretch::thread_native_calls(), before);
}

#[test]
fn real_time_loop_matches_offline_after_the_start_ramp() {
    let (mut s, _) = setup("warp-rt-loop", 44100, 4410, 180.0, 120.0);
    s.project.loop_on = true;
    s.project.loop_range = TickRange::new(Tick(137), Tick(877));
    let n = 740 * 25 * 3;
    let reference = play(&s.program(), 48000, Tick(137), n, 64);
    let p = s.program();
    let mut r = Rig::new(RATE, 512);
    r.set_program(&p);
    r.send(Command::Locate(Tick(137)));
    wait_primed(&mut r, &p);
    let before = komponist_stretch::thread_native_calls();
    r.send(Command::Play);
    let range = paced(&mut r, n);
    assert_eq!(&r.l[range.start + 144..range.end], &reference.l[144..]);
    assert_eq!(r.handle.telemetry().audio_underruns(), 0);
    assert_eq!(komponist_stretch::thread_native_calls(), before);
}

#[test]
fn a_failed_warp_is_counted_without_blocking_other_tracks_or_later_clips() {
    let mut s = Studio::new("warp-failure-mix");
    let t = s.track();
    let other = s.track();
    let bad = s.wav("bad.wav", 48000, 2, 48000, |_, _| f32::NAN);
    let good = s.wav("good.wav", 48000, 2, 96000, |_, _| 0.25);
    let bed = s.wav("bed.wav", 48000, 2, 96000, |_, _| 0.125);
    let c = s.clip(t, &bad, 0, 960, 0);
    s.project = edits::set_audio_clip_warp(&s.project, c, true).unwrap();
    s.clip(t, &good, 960, 960, 0);
    s.clip(other, &bed, 0, 1920, 0);
    let p = s.program();
    let r = play(&p, 48000, Tick::ZERO, 48000, 64);
    assert!(r.handle.telemetry().audio_underruns() > 0);
    assert!(p.tracks[0].audio.as_ref().unwrap().stretch_failures() > 0);
    for &v in &r.l[2000..22000] {
        assert!((v - 0.125).abs() < 1e-6);
    }
    for &v in &r.l[26000..46000] {
        assert!((v - 0.375).abs() < 1e-6);
    }
    let p = s.program();
    let mut r = Rig::new(RATE, 512);
    r.set_program(&p);
    let path = s.dir.join("good-range.wav");
    export::export(
        &mut r.engine,
        &mut r.handle,
        &p,
        &job(path, 48000, 24000..48000),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
}

#[test]
#[ignore = "paced worker load measurement; run separately to avoid contention"]
fn worker_load_eight_and_sixteen_stereo_voices_at_ratio_extrema() {
    for voices in [8, 16] {
        for (original, project) in [(40.0, 240.0), (180.0, 120.0), (240.0, 40.0)] {
            let mut s = Studio::new(&format!("warp-load-{voices}-{original}"));
            s.project =
                edits::set_project_prop(&s.project, edits::ProjectProp::Tempo(Tempo::new(project)))
                    .unwrap();
            let file = s.wav("source.wav", 48000, 2, 48000 * 12, sine(440.0, 48000));
            for _ in 0..voices {
                let t = s.track();
                let c = s.clip(t, &file, 0, (project * 960.0 * 2.0 / 60.0) as i64, 0);
                s.prop(c, edits::AudioClipProp::OriginalTempo(Tempo::new(original)));
                s.project = edits::set_audio_clip_warp(&s.project, c, true).unwrap();
            }
            let p = s.program();
            let mut r = Rig::new(RATE, 512);
            r.set_program(&p);
            wait_primed(&mut r, &p);
            let stats = r.engine.stream_stats();
            let (busy, frames) = (stats.busy(), stats.frames());
            r.send(Command::Play);
            let start = std::time::Instant::now();
            let mut done = 0;
            while done < 48000 {
                let n = 256.min(48000 - done);
                r.process_with(n, &[], 0);
                done += n;
                let deadline = start + std::time::Duration::from_secs_f64(done as f64 / 48000.0);
                std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
            }
            let elapsed = start.elapsed();
            let spent = stats.busy() - busy;
            let produced = stats.frames() - frames;
            let us256 = spent.as_secs_f64() * 1e6 / (produced as f64 / voices as f64 / 256.0);
            println!(
                "voices={voices} ratio={} worker_us_per_256={us256:.1} normalized_core_percent={:.2} wall_core_percent={:.2} underruns={}",
                original / project,
                us256 / 5333.333 * 100.0,
                spent.as_secs_f64() / elapsed.as_secs_f64() * 100.0,
                r.handle.telemetry().audio_underruns()
            );
            assert!(produced > 0);
            assert_eq!(r.handle.telemetry().audio_underruns(), 0);
        }
    }
}

#[test]
fn native_offset_and_trim_select_the_expected_source_clicks_after_conversion() {
    for (i, ratio) in [0.75, 1.5].into_iter().enumerate() {
        let mut s = Studio::new(&format!("warp-offset-{i}"));
        let t = s.track();
        let offset = 12345;
        let file = s.wav("clicks.wav", 44100, 2, 44100 * 4 + offset, |j, _| {
            if [offset + 22050, offset + 44100, offset + 66150].contains(&j) {
                0.8
            } else {
                0.0
            }
        });
        let c = s.clip(t, &file, 960, 1920, offset as u64);
        s.prop(
            c,
            edits::AudioClipProp::OriginalTempo(Tempo::new(120.0 * ratio)),
        );
        s.project = edits::set_audio_clip_warp(&s.project, c, true).unwrap();
        s.project = edits::set_clip_length(&s.project, c, Tick((3840.0 * ratio) as i64)).unwrap();
        let a = play(&s.program(), 48000, Tick::ZERO, 200000, 64);
        s.project = edits::trim_audio_clip_start(&s.project, c, Tick(1103)).unwrap();
        let b = play(&s.program(), 48000, Tick::ZERO, 200000, 1024);
        for n in 1..=3 {
            let expected = 24000 + (n as f64 * 0.5 * ratio * 48000.0).round() as usize;
            let peak = |data: &[f32]| {
                let lo = expected - 144;
                lo + data[lo..expected + 145]
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
                    .unwrap()
                    .0
            };
            let first = peak(&a.l);
            let trimmed = peak(&b.l);
            assert!(a.l[first].abs() > 0.02, "source offset missed click");
            assert!(b.l[trimmed].abs() > 0.02);
            assert!(first.abs_diff(expected) < 144);
            assert!(trimmed.abs_diff(expected) < 144);
        }
    }
}

#[test]
fn an_incompatible_continuation_head_is_an_underrun_and_recovery_fades_in_position() {
    let (mut s, c) = setup("warp-continuation-head", 48000, 0, 180.0, 120.0);
    s.project = edits::split_clips(&s.project, &[c], Tick(137)).unwrap().0;
    let reference = play(&s.program(), 48000, Tick::ZERO, 24000, 512);
    let p = s.program();
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&p);
    r.send(Command::Locate(Tick(960)));
    wait_primed(&mut r, &p);
    r.engine.stall_streaming(true);
    r.send(Command::Locate(Tick::ZERO));
    r.send(Command::Play);
    let range = r.process(5000, 64);
    assert_eq!(&r.l[range.start..range.start + 3425], &reference.l[..3425]);
    assert!(r.l[range.start + 3425..range.end].iter().all(|&x| x == 0.0));
    assert!(r.handle.telemetry().audio_underruns() > 0);
    r.engine.stall_streaming(false);
    let mut position = 5000;
    let mut heard = false;
    for _ in 0..100 {
        let range = r.process(512, 512);
        if r.l[range.clone()].iter().any(|x| x.abs() > 0.01) {
            for j in 144..512 {
                assert!(
                    (r.l[range.start + j] - reference.l[position + j]).abs() < 1e-5,
                    "recovery position {position}"
                );
            }
            heard = true;
            break;
        }
        position += 512;
        if position + 512 >= reference.l.len() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(heard);
}

#[test]
fn isolated_warp_stems_sum_to_the_same_float_mix() {
    let (mut s, _) = setup("warp-stems", 44100, 12345, 180.0, 120.0);
    let t = s.track();
    let f = s.wav("bass.wav", 48000, 2, 48000 * 4, sine(110.0, 48000));
    s.clip(t, &f, 0, 5760, 0);
    let p = s.program();
    let mut sum = vec![0.0; 144000 * 2];
    for i in 0..p.tracks.len() {
        let mut stem = p.clone();
        stem.tracks = vec![p.tracks[i].clone()];
        let mut r = Rig::new(RATE, 1024);
        r.set_program(&stem);
        let path = s.dir.join(format!("stem-{i}.wav"));
        export::export(
            &mut r.engine,
            &mut r.handle,
            &stem,
            &job(path.clone(), 48000, 0..144000),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        for (dest, sample) in sum.iter_mut().zip(super::export::read(&path).samples) {
            *dest += sample;
        }
    }
    let mut r = Rig::new(RATE, 1024);
    r.set_program(&p);
    let path = s.dir.join("mix.wav");
    export::export(
        &mut r.engine,
        &mut r.handle,
        &p,
        &job(path.clone(), 48000, 0..144000),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(sum, super::export::read(&path).samples);
}

#[test]
fn seamless_warp_splits_tolerate_native_and_timeline_frame_quantization() {
    for (i, (source_rate, rate, original, project)) in [
        (8000, 384000, 40.0, 240.0),
        (44100, 48000, 40.0, 240.0),
        (384000, 8000, 40.0, 240.0),
        (8000, 384000, 240.0, 40.0),
    ]
    .into_iter()
    .enumerate()
    {
        let (mut s, c) = setup(
            &format!("warp-split-quantization-{i}"),
            source_rate,
            0,
            original,
            project,
        );
        let p = compile::build(&s.project, None, &Env(rate));
        let frames = p.tracks[0].audio.as_ref().unwrap().segments()[0].end as usize;
        let original = play(&p, rate, Tick::ZERO, frames, 1024);
        assert!(original.l.iter().any(|s| s.abs() > 0.1), "silent case {i}");
        s.project = edits::split_clips(&s.project, &[c], Tick(137)).unwrap().0;
        let p = compile::build(&s.project, None, &Env(rate));
        let segments = p.tracks[0].audio.as_ref().unwrap().segments();
        assert!(
            !segments[0].edge_out && !segments[1].edge_in,
            "lost seamless split, case {i}"
        );
        let split = play(&p, rate, Tick::ZERO, frames, 64);
        assert_eq!(original.l, split.l, "split changed DSP output, case {i}");
    }
}
