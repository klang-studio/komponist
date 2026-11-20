//! WAV export through the public API (PLAN WP-53 "Done when", engine part): length and sample
//! alignment, the tail, normalisation, dither noise, cancel, and the engine after an export.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use komponist_engine::export::{
    BitDepth, ExportError, ExportJob, ExportReport, NORMALIZE_TARGET_DB, Progress, TrackExport,
    export, export_track,
};
use komponist_engine::testing::{Logged, TestSine};
use komponist_engine::{Command, EngineEvent, Program, TransportState};
use komponist_model::{Tick, TickRange};

use crate::common::*;

/// A fresh, empty directory `name` below Cargo's temporary directory for integration tests.
pub fn dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("export")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Names of the entries of `dir`, sorted (hidden ones included).
pub fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// A job writing `path`: `range` plus `tail` at `rate`, 32-bit float, no normalisation, no dither.
fn job(path: PathBuf, rate: f32, range: Range<u64>, tail: u64) -> ExportJob {
    ExportJob {
        path,
        sample_rate: rate as u32,
        range,
        tail,
        bit_depth: BitDepth::Float32,
        normalize: None,
        dither: false,
    }
}

/// Runs `job` on the rig's engine (which plays `program`) and collects the progress reports.
pub fn run(
    rig: &mut Rig,
    program: &Program,
    job: &ExportJob,
) -> (Result<ExportReport, ExportError>, Vec<Progress>) {
    let cancel = AtomicBool::new(false);
    let mut reports = Vec::new();
    let result = export(
        &mut rig.engine,
        &mut rig.handle,
        program,
        job,
        &cancel,
        |p| reports.push(p),
    );
    (result, reports)
}

/// A WAV file: its spec, its samples as full scale ±1.0 (integers divided by 2^(bits − 1)) and,
/// for integer files, the raw codes. Interleaved.
pub struct Wav {
    pub spec: hound::WavSpec,
    pub frames: u32,
    pub samples: Vec<f32>,
    pub codes: Vec<i32>,
}

pub fn read(path: &Path) -> Wav {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    let frames = reader.duration();
    let (samples, codes) = match spec.sample_format {
        hound::SampleFormat::Float => (
            reader.samples::<f32>().map(Result::unwrap).collect(),
            Vec::new(),
        ),
        hound::SampleFormat::Int => {
            let codes: Vec<i32> = reader.samples::<i32>().map(Result::unwrap).collect();
            let full = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            (codes.iter().map(|&c| c as f32 / full).collect(), codes)
        }
    };
    Wav {
        spec,
        frames,
        samples,
        codes,
    }
}

/// Sample peak in dBFS.
pub fn peak_db(samples: &[f32]) -> f32 {
    20.0 * samples.iter().fold(0.0_f32, |m, x| m.max(x.abs())).log10()
}

/// A rig at `rate` with a test sine in slot 0, `program` queued.
fn sine_rig(rate: f32, program: &Program) -> Rig {
    let mut rig = Rig::new(rate, 2048);
    rig.insert(0, TestSine::new());
    rig.set_program(program);
    rig
}

/// Statistics of an error signal in LSB: mean, RMS, lag-1 autocorrelation (per channel).
fn stats(error: &[f64]) -> (f64, f64, f64) {
    let n = error.len() as f64;
    let mean = error.iter().sum::<f64>() / n;
    let power = error.iter().map(|e| e * e).sum::<f64>() / n;
    // Interleaved stereo: the previous sample of the same channel is two back.
    let lag = error.windows(3).map(|w| w[0] * w[2]).sum::<f64>() / n;
    (mean, power.sqrt(), lag / power)
}

#[test]
fn the_file_holds_range_plus_tail_sample_exactly() {
    let dir = dir("length");
    for rate in [44_100.0_f32, 48_000.0, 96_000.0] {
        // At 124 bpm a tick is 22.2 / 24.2 / 48.4 samples, so the start (5 frames after bar 2)
        // lies between ticks and needs a pre-roll that starts the first note.
        let p = program(
            124.0,
            vec![track(
                1,
                0,
                vec![
                    note(BAR, 480, 60),
                    note(BAR + 1000, 2000, 67),
                    note(2 * BAR + 77, 960, 72),
                ],
            )],
        );
        let start = sample(&p, BAR, rate) as u64 + 5;
        let end = sample(&p, 3 * BAR + 500, rate) as u64;
        let tail = 12_345;
        let path = dir.join(format!("{rate}.wav"));
        let mut rig = sine_rig(rate, &p);
        let (result, progress) = run(&mut rig, &p, &job(path.clone(), rate, start..end, tail));
        let report = result.unwrap();
        let frames = end - start + tail;
        assert_eq!(report.frames, frames);
        let wav = read(&path);
        assert_eq!(u64::from(wav.frames), frames, "{rate} Hz");
        assert_eq!(wav.samples.len() as u64, 2 * frames);
        assert_eq!(
            (
                wav.spec.channels,
                wav.spec.sample_rate,
                wav.spec.bits_per_sample
            ),
            (2, rate as u32, 32)
        );
        // Bit-identical to a plain render from tick 0, frame for frame.
        let mut reference = sine_rig(rate, &p);
        reference.send(Command::Play);
        reference.render((end + tail) as usize, 2048);
        for (i, frame) in wav.samples.chunks(2).enumerate() {
            let at = start as usize + i;
            assert_eq!(
                (frame[0], frame[1]),
                (reference.l[at], reference.r[at]),
                "{rate} Hz, file frame {i}"
            );
        }
        assert_ne!(wav.samples[0], 0.0, "the pre-roll started the first note");
        // Progress: rising to 1.0, the peak so far, the last one equal to the report's.
        assert!(progress.windows(2).all(|w| w[0].fraction < w[1].fraction));
        assert!(progress.windows(2).all(|w| w[0].peak_db <= w[1].peak_db));
        let last = progress.last().unwrap();
        assert_eq!(last.fraction, 1.0);
        assert_eq!(last.peak_db, report.peak_db);
        assert!((report.peak_db - peak_db(&wav.samples)).abs() < 1e-4);
        assert_eq!(report.gain_db, 0.0);
    }
}

#[test]
fn the_tail_starts_no_note_and_cuts_the_sounding_ones() {
    let dir = dir("tail");
    let rate = RATE;
    // Note 64 sounds across the range end (bar 2), note 67 starts after it. Loop over the range
    // and metronome are on: the export plays neither.
    let mut p = program(
        120.0,
        vec![track(
            1,
            0,
            vec![
                note(0, 960, 60),
                note(BAR - 480, 960, 64),
                note(BAR + 100, 480, 67),
            ],
        )],
    );
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(0), Tick(BAR));
    p.metronome = true;
    let end = sample(&p, BAR, rate) as u64;
    let path = dir.join("tail.wav");
    let mut rig = sine_rig(rate, &p);
    let (result, _) = run(&mut rig, &p, &job(path.clone(), rate, 0..end, rate as u64));
    result.unwrap();
    let wav = read(&path);
    let left: Vec<f32> = wav.samples.iter().step_by(2).copied().collect();
    assert_eq!(left.len() as u64, end + rate as u64);
    let end = end as usize;
    assert_eq!(onsets(&left), [0, sample(&p, BAR - 480, rate) as usize]);
    assert!(left[end - 64..end].iter().any(|&x| x != 0.0));
    assert!(left[end..].iter().all(|&x| x == 0.0), "the tail is silent");
}

#[test]
fn export_owns_its_staging_and_preserves_existing_sibling_files() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.wav");
    let sibling = dir.path().join(".out.wav.part");
    std::fs::write(&output, b"previous export").unwrap();
    std::fs::write(&sibling, b"unrelated file").unwrap();
    let program = program(120.0, vec![track(1, 0, vec![note(0, 480, 60)])]);
    let mut rig = sine_rig(RATE, &program);
    let render = job(output.clone(), RATE, 0..48_000, 0);
    let cancel = AtomicBool::new(false);
    let result = export(
        &mut rig.engine,
        &mut rig.handle,
        &program,
        &render,
        &cancel,
        |_| {
            cancel.store(true, Ordering::Relaxed);
        },
    );
    assert!(matches!(result, Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read(&output).unwrap(), b"previous export");
    assert_eq!(std::fs::read(&sibling).unwrap(), b"unrelated file");
    assert_eq!(files(dir.path()), [".out.wav.part", "out.wav"]);
    rig.engine.settle();

    run(&mut rig, &program, &render).0.unwrap();
    assert_eq!(std::fs::read(&sibling).unwrap(), b"unrelated file");
    assert_eq!(files(dir.path()), [".out.wav.part", "out.wav"]);
    assert_eq!(read(&output).frames, 48_000);
}

#[test]
fn unnormalized_wav_grows_in_staging_before_render_completion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.wav");
    std::fs::write(&path, b"previous export").unwrap();
    let p = program(120.0, vec![track(1, 0, vec![note(0, 480, 69)])]);
    let mut rig = sine_rig(RATE, &p);
    let mut wrote_before_completion = false;
    let report = export(
        &mut rig.engine,
        &mut rig.handle,
        &p,
        &job(path.clone(), RATE, 13..64_009, 56),
        &AtomicBool::new(false),
        |progress| {
            if !wrote_before_completion && (0.25..0.75).contains(&progress.fraction) {
                assert_eq!(std::fs::read(&path).unwrap(), b"previous export");
                let entries = std::fs::read_dir(dir.path()).unwrap();
                let staged = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path().join("audio.wav"))
                    .find_map(|wav| std::fs::metadata(wav).ok())
                    .expect("private staged WAV while rendering");
                assert!(
                    staged.len() > 8192,
                    "rendered samples must already be on disk"
                );
                wrote_before_completion = true;
            }
        },
    )
    .unwrap();
    assert!(wrote_before_completion);
    assert_eq!(files(dir.path()), ["out.wav"]);
    assert_eq!(u64::from(read(&path).frames), report.frames);
}

#[test]
fn track_export_restores_the_other_live_tracks_after_success_and_cancel() {
    use komponist_engine::compile::{self, NoEnv};
    use komponist_model::edits::{NewClip, NewClipContent, NewNote, NewTrack, add_clip, add_track};
    use komponist_model::{PluginFormat, PluginRef, Project};

    let empty_track = NewTrack {
        name: None,
        hue: None,
        index: None,
        arm_exclusive: false,
        instrument: None,
    };
    let (project, target) = add_track(&Project::new("Track render"), empty_track.clone()).unwrap();
    let (project, neighbour) = add_track(
        &project,
        NewTrack {
            instrument: Some(PluginRef {
                format: PluginFormat::Builtin,
                id: "test-sine".into(),
                name: "Sine".into(),
                vendor: String::new(),
                version: String::new(),
                module: "".into(),
            }),
            ..empty_track
        },
    )
    .unwrap();
    let notes = NewClip {
        start: Tick::ZERO,
        length: Tick(480),
        name: "MIDI".into(),
        content: NewClipContent::Midi {
            notes: vec![NewNote {
                start: Tick::ZERO,
                length: Tick(480),
                pitch: 60,
                velocity: 127,
            }],
            take_name: String::new(),
        },
    };
    let (project, _) = add_clip(&project, target, notes.clone()).unwrap();
    let (project, _) = add_clip(&project, neighbour, notes).unwrap();
    let live = compile::build(&project, None, &NoEnv);
    let slot = live.track(neighbour).unwrap().chain[0].slot;
    let scoped = TrackExport::new(&project, target).unwrap();
    let dir = tempfile::tempdir().unwrap();
    for (depth, normalize, should_cancel) in [
        (BitDepth::Float32, None, true),
        (BitDepth::Float32, None, false),
        (BitDepth::Int16, None, false),
        (BitDepth::Int24, None, false),
        (BitDepth::Float32, Some(NORMALIZE_TARGET_DB), false),
    ] {
        let mut rig = Rig::new(RATE, 2048);
        rig.insert(slot.0, TestSine::new());
        rig.set_program(&live);
        let path = dir.path().join("track.wav");
        let job = ExportJob {
            bit_depth: depth,
            normalize,
            ..job(path.clone(), RATE, 0..12_000, 16_000)
        };
        let cancel = AtomicBool::new(false);
        let result = export_track(
            &mut rig.engine,
            &mut rig.handle,
            &live,
            (&scoped, &project),
            &job,
            &cancel,
            |_| cancel.store(should_cancel, Ordering::Relaxed),
        );
        if should_cancel {
            assert!(matches!(result, Err(ExportError::Cancelled)));
            assert!(!path.exists());
        } else {
            // A silent track keeps its whole requested range and 0.1 s of tail padding.
            let frames = 12_000 + 4_800;
            assert_eq!(result.unwrap().frames, frames);
            let wav = read(&path);
            assert_eq!(u64::from(wav.frames), frames);
            assert!(wav.samples.iter().all(|&sample| sample == 0.0));
            let header = if depth == BitDepth::Int16 { 44 } else { 68 };
            assert_eq!(
                std::fs::metadata(&path).unwrap().len(),
                header + frames * 2 * u64::from(depth.bits() / 8)
            );
        }
        // No app-level SetProgram command: the neighbouring instrument must audible immediately.
        rig.send(Command::Play);
        let played = rig.process(512, 512);
        assert!(rig.l[played].iter().any(|sample| sample.abs() > 0.1));
    }
}

#[test]
fn the_normalised_peak_is_minus_one_dbfs() {
    let dir = dir("normalise");
    let rate = RATE;
    // One note at full velocity: peak 0.5 = −6.02 dBFS.
    let p = program(120.0, vec![track(1, 0, vec![note(0, 960, 69)])]);
    let end = sample(&p, BAR, rate) as u64;
    for depth in [BitDepth::Int16, BitDepth::Int24, BitDepth::Float32] {
        for dither in [false, true] {
            let path = dir.join(format!("{depth:?}-{dither}.wav"));
            let mut rig = sine_rig(rate, &p);
            let job = ExportJob {
                bit_depth: depth,
                normalize: Some(NORMALIZE_TARGET_DB),
                dither,
                ..job(path.clone(), rate, 0..end, 1000)
            };
            let report = run(&mut rig, &p, &job).0.unwrap();
            assert!((report.peak_db + 6.0206).abs() < 1e-3, "{}", report.peak_db);
            assert!((report.gain_db - 5.0206).abs() < 1e-3, "{}", report.gain_db);
            let wav = read(&path);
            assert_eq!(wav.spec.bits_per_sample, depth.bits());
            let peak = peak_db(&wav.samples);
            assert!(
                (peak - NORMALIZE_TARGET_DB).abs() < 0.01,
                "{depth:?}, dither {dither}: {peak} dBFS"
            );
        }
    }
}

#[test]
fn streamed_dither_matches_a_continuous_rng_before_and_after_normalization() {
    use klang_dsp::rng::Rng;
    use komponist_engine::export::{normalize_gain, quantize, tpdf};

    let dir = tempfile::tempdir().unwrap();
    let p = program(120.0, vec![track(1, 0, vec![note(0, 1920, 78)])]);
    let source = job(dir.path().join("source.wav"), RATE, 13..70_001, 50);
    let mut rig = sine_rig(RATE, &p);
    run(&mut rig, &p, &source).0.unwrap();
    let rendered = read(&source.path);
    let peak = rendered
        .samples
        .iter()
        .fold(0.0_f32, |peak, s| peak.max(s.abs()));

    for normalize in [None, Some(NORMALIZE_TARGET_DB)] {
        let mut rig = sine_rig(RATE, &p);
        let output = ExportJob {
            path: dir.path().join("encoded.wav"),
            bit_depth: BitDepth::Int24,
            dither: true,
            normalize,
            ..source.clone()
        };
        let report = run(&mut rig, &p, &output).0.unwrap();
        assert_eq!(u64::from(rendered.frames), report.frames);
        let gain = normalize.map_or(1.0, |db| normalize_gain(peak, db));
        let mut rng = Rng::new(0x5eed_d17e);
        let expected = rendered
            .samples
            .iter()
            .map(|&sample| quantize(sample * gain, 24, tpdf(&mut rng)))
            .collect::<Vec<_>>();
        assert!(expected.len() > 1 << 16);
        assert_eq!(read(&output.path).codes, expected);
    }
    assert_eq!(files(dir.path()), ["encoded.wav", "source.wav"]);
}

#[test]
fn dither_noise_on_silence_is_half_an_lsb() {
    let dir = dir("silence");
    let rate = RATE;
    let p = program(120.0, vec![track(1, 0, Vec::new())]);
    for (depth, dither) in [
        (BitDepth::Int16, true),
        (BitDepth::Int24, true),
        (BitDepth::Int16, false),
    ] {
        let path = dir.join(format!("{depth:?}-{dither}.wav"));
        let mut rig = sine_rig(rate, &p);
        let job = ExportJob {
            bit_depth: depth,
            dither,
            ..job(path.clone(), rate, 0..48_000, 48_000)
        };
        run(&mut rig, &p, &job).0.unwrap();
        let wav = read(&path);
        assert_eq!(wav.codes.len(), 192_000);
        if !dither {
            assert!(wav.codes.iter().all(|&c| c == 0), "digital silence stays");
            continue;
        }
        // TPDF of ±1 LSB, rounded: codes −1, 0, 1 with probabilities 1/8, 3/4, 1/8, so the
        // error has variance 1/4 (RMS 0.5 LSB), mean 0, and is white.
        assert!(wav.codes.iter().all(|c| (-1..=1).contains(c)));
        let error: Vec<f64> = wav.codes.iter().map(|&c| f64::from(c)).collect();
        let (mean, rms, lag) = stats(&error);
        let ones = wav.codes.iter().filter(|&&c| c != 0).count() as f64 / 192_000.0;
        assert!(mean.abs() < 0.005, "{depth:?} mean {mean}");
        assert!((rms - 0.5).abs() < 0.01, "{depth:?} rms {rms}");
        assert!((ones - 0.25).abs() < 0.01, "{depth:?} P(±1) {ones}");
        assert!(lag.abs() < 0.02, "{depth:?} lag-1 correlation {lag}");
    }
}

#[test]
fn dither_noise_on_a_sine_is_half_an_lsb() {
    let dir = dir("sine");
    let rate = RATE;
    // A sustained A4 at −6 dBFS over one bar: 96 000 frames.
    let p = program(120.0, vec![track(1, 0, vec![note(0, 2 * BAR, 69)])]);
    let end = sample(&p, BAR, rate) as u64;
    let reference = dir.join("float.wav");
    let mut rig = sine_rig(rate, &p);
    run(&mut rig, &p, &job(reference.clone(), rate, 0..end, 0))
        .0
        .unwrap();
    let signal = read(&reference).samples;
    // Every export renders the same samples, so the float file is the exact input of the
    // quantiser. Error in LSB: TPDF-dithered rounding has RMS 0.5 (variance 1/6 + 1/12) whatever
    // the signal, and is white and uncorrelated with it. Plain rounding is near 1/√12 ≈ 0.289
    // but depends on the signal (this sine repeats every 1 200 samples: measured 0.30).
    for (depth, dither, expected_rms, tolerance) in [
        (BitDepth::Int16, true, 0.5, 0.01),
        (BitDepth::Int24, true, 0.5, 0.01),
        (BitDepth::Int16, false, 1.0 / 12.0_f64.sqrt(), 0.03),
    ] {
        let path = dir.join(format!("{depth:?}-{dither}.wav"));
        let job = ExportJob {
            bit_depth: depth,
            dither,
            ..job(path.clone(), rate, 0..end, 0)
        };
        run(&mut rig, &p, &job).0.unwrap();
        let wav = read(&path);
        let full = f64::from(1_u32 << (depth.bits() - 1));
        let error: Vec<f64> = wav
            .codes
            .iter()
            .zip(&signal)
            .map(|(&c, &x)| f64::from(c) - f64::from(x) * full)
            .collect();
        let (mean, rms, lag) = stats(&error);
        assert!(mean.abs() < 0.01, "{depth:?}/{dither} mean {mean}");
        let worst = error.iter().fold(0.0_f64, |m, e| m.max(e.abs()));
        assert!(
            worst <= if dither { 1.5 } else { 0.5 },
            "{depth:?}/{dither} error {worst}"
        );
        assert!(
            (rms - expected_rms).abs() < tolerance,
            "{depth:?}/{dither} rms {rms}, expected {expected_rms}"
        );
        if dither {
            let signal_power = signal.iter().map(|&x| f64::from(x).powi(2)).sum::<f64>();
            let cross = error
                .iter()
                .zip(&signal)
                .map(|(e, &x)| e * f64::from(x))
                .sum::<f64>();
            let correlation = cross / (signal_power * rms * rms * error.len() as f64).sqrt();
            assert!(
                correlation.abs() < 0.01,
                "correlation with the signal {correlation}"
            );
            assert!(lag.abs() < 0.02, "lag-1 correlation {lag}");
        }
    }
}

#[test]
fn a_cancelled_export_leaves_no_file() {
    let dir = dir("cancel");
    let rate = RATE;
    let p = program(120.0, vec![track(1, 0, vec![note(0, 960, 69)])]);
    let end = sample(&p, 2 * BAR, rate) as u64;
    let target = dir.join("song.wav");
    // Cancel before the render, after the first render call, and once the render is complete
    // (the writer then has created its temporary file and must remove it).
    let points: [fn(&Progress) -> bool; 3] = [|_| true, |_| true, |p| p.fraction >= 1.0];
    for (case, cancel_on) in points.into_iter().enumerate() {
        for existing in [false, true] {
            if existing {
                std::fs::write(&target, b"old").unwrap();
            }
            let mut rig = sine_rig(rate, &p);
            let cancel = AtomicBool::new(case == 0);
            let mut calls = 0;
            let result = export(
                &mut rig.engine,
                &mut rig.handle,
                &p,
                &job(target.clone(), rate, 0..end, 48_000),
                &cancel,
                |progress| {
                    calls += 1;
                    if cancel_on(&progress) {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
            );
            assert!(matches!(result, Err(ExportError::Cancelled)), "case {case}");
            assert_eq!(calls, [0, 1, 118][case], "case {case}");
            if existing {
                assert_eq!(files(&dir), ["song.wav"]);
                assert_eq!(std::fs::read(&target).unwrap(), b"old");
                std::fs::remove_file(&target).unwrap();
            } else {
                assert!(files(&dir).is_empty(), "case {case}: {:?}", files(&dir));
            }
            // The engine is restored as after a completed export.
            rig.process(1024, 1024);
            assert_eq!(rig.handle.telemetry().transport(), TransportState::Stopped);
            assert_eq!(rig.handle.telemetry().playhead(), Tick::ZERO);
        }
    }
}

#[test]
fn a_failed_publication_cleans_staging_and_restores_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let p = program(120.0, vec![track(1, 0, vec![note(0, 960, 69)])]);
    let path = dir.path().join("existing.wav");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), b"preserve").unwrap();
    for normalize in [None, Some(NORMALIZE_TARGET_DB)] {
        let mut rig = sine_rig(RATE, &p);
        let request = ExportJob {
            normalize,
            ..job(path.clone(), RATE, 0..6000, 5000)
        };
        let (result, progress) = run(&mut rig, &p, &request);
        assert!(matches!(result, Err(ExportError::Io(_))));
        assert_eq!(progress.last().unwrap().fraction, 1.0);
        assert_eq!(std::fs::read(path.join("keep.txt")).unwrap(), b"preserve");
        assert_eq!(files(dir.path()), ["existing.wav"]);
        rig.process(512, 512);
        assert_eq!(rig.handle.telemetry().transport(), TransportState::Stopped);
    }
}

#[test]
fn invalid_jobs_change_nothing() {
    let dir = dir("invalid");
    let p = program(120.0, vec![track(1, 0, vec![note(0, 960, 69)])]);
    let mut rig = sine_rig(RATE, &p);
    rig.send(Command::Play);
    let path = dir.join("x.wav");
    #[allow(clippy::reversed_empty_ranges)]
    let backwards = job(path.clone(), RATE, 1000..999, 0);
    assert!(matches!(
        run(&mut rig, &p, &backwards).0,
        Err(ExportError::InvalidRange)
    ));
    let other_rate = job(path.clone(), 44_100.0, 0..1000, 0);
    match run(&mut rig, &p, &other_rate).0 {
        Err(ExportError::SampleRate { engine, job }) => assert_eq!((engine, job), (RATE, 44_100)),
        other => panic!("{other:?}"),
    }
    let too_long = job(path.clone(), RATE, 0..u64::from(u32::MAX), 0);
    match run(&mut rig, &p, &too_long).0 {
        Err(ExportError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput),
        other => panic!("{other:?}"),
    }
    let no_name = job(PathBuf::from("/"), RATE, 0..1000, 0);
    assert!(matches!(
        run(&mut rig, &p, &no_name).0,
        Err(ExportError::Io(_))
    ));
    let no_dir = job(dir.join("missing").join("x.wav"), RATE, 0..1000, 0);
    match run(&mut rig, &p, &no_dir).0 {
        Err(ExportError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }
    assert!(files(&dir).is_empty());
    // None of them touched the engine: it plays on from where it was.
    rig.process(1024, 1024);
    assert_eq!(rig.handle.telemetry().transport(), TransportState::Playing);
    assert_eq!(rig.frames(), 1024);
    assert_eq!(onsets(&rig.l), [0]);
    assert_eq!(rig.garbage, 0);
}

#[test]
fn the_engine_plays_normally_after_an_export() {
    let dir = dir("after");
    // The device runs at 48 kHz, the export at 44.1 kHz. Loop over bar 1, one note on beat 2.
    let mut p = program(120.0, vec![track(1, 0, vec![note(960, 480, 69)])]);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(0), Tick(BAR));
    let mut rig = Rig::new(RATE, 2048);
    let (sine, mut log) = TestSine::logged(1 << 16);
    rig.insert(0, sine);
    rig.set_program(&p);
    rig.send(Command::Locate(Tick(1920)));
    rig.process(1024, 1024);
    assert_eq!(rig.handle.telemetry().playhead(), Tick(1920));
    drain(&mut log);

    // Before: (detach,) set the export rate. Export the loop range at 44.1 kHz.
    let export_rate = 44_100;
    rig.engine.set_sample_rate(export_rate as f32).unwrap();
    let range = ExportJob::frames_of(&p.tempo_map, p.loop_range, export_rate);
    assert_eq!(range, 0..88_200);
    let job = ExportJob {
        bit_depth: BitDepth::Int24,
        normalize: Some(NORMALIZE_TARGET_DB),
        dither: true,
        ..job(
            dir.join("loop.wav"),
            export_rate as f32,
            range,
            ExportJob::tail_frames(0.5, export_rate),
        )
    };
    run(&mut rig, &p, &job).0.unwrap();
    let wav = read(&dir.join("loop.wav"));
    assert_eq!(wav.frames, 88_200 + 22_050);
    let left: Vec<f32> = wav.samples.iter().step_by(2).copied().collect();
    // The note on beat 2 (22 050 to 33 075), no second lap of the loop; around it only dither.
    let lsb = 1.0 / 8_388_608.0;
    let sounding: Vec<usize> = (0..left.len())
        .filter(|&i| left[i].abs() > 2.0 * lsb)
        .collect();
    assert_eq!(sounding.first(), Some(&22_050));
    assert!(sounding.last().is_some_and(|&i| i < 33_075));
    let calls: Vec<bool> = drain(&mut log)
        .iter()
        .filter_map(|e| match e {
            Logged::Call { offline, .. } => Some(*offline),
            _ => None,
        })
        .collect();
    assert!(!calls.is_empty() && calls.iter().all(|&offline| offline));
    assert_eq!(rig.handle.telemetry().master_meter().take(), (0.0, 0.0));

    // After: restore the device rate, re-attach. The first callback applies the queued restore:
    // stopped at the old playhead, the original program back.
    rig.engine.set_sample_rate(RATE).unwrap();
    rig.process(1024, 1024);
    let telemetry = rig.handle.telemetry().clone();
    assert_eq!(telemetry.transport(), TransportState::Stopped);
    assert_eq!(telemetry.playhead(), Tick(1920));
    // Both replaced programs came back: the rig's own and the export program.
    assert_eq!(rig.garbage, 2);
    assert!(rig.take_events().is_empty());

    // It plays as before: the note on beat 2 at 48 kHz, and the loop wraps again.
    rig.send(Command::Locate(Tick::ZERO));
    rig.send(Command::Play);
    let played = rig.process(2 * 96_000 + 100, 512);
    assert_eq!(onsets(&rig.l[played]), [24_000, 120_000]);
    assert!(
        rig.take_events()
            .iter()
            .any(|e| matches!(e, EngineEvent::LoopWrapped { to } if *to == Tick::ZERO))
    );
    let calls = drain(&mut log);
    assert!(
        calls
            .iter()
            .all(|e| !matches!(e, Logged::Call { offline: true, .. }))
    );
}
