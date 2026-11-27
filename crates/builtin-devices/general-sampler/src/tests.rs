use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_no_alloc::assert_no_alloc;
use komponist_device_core::InternalProcessor;
use komponist_model::params::ParamId;
use komponist_model::{Meter, Tick, TickRange};
use komponist_processor::{Event, EventKind, ProcessContext, Processor, StereoIo, TransportInfo};

use komponist_sampler_shared::{SampleLink, SamplerHandle, SyncStatus, testkit};

use super::{SPEC, Settings, dsp_with_handle, read_settings, sync_settings, write_settings};

#[allow(dead_code)]
#[path = "../../sampler-shared/core/src/baseline_support.rs"]
mod baseline_support;
mod lfo_baseline;

#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const RATE: f32 = 16_000.0;
const BLOCK: usize = 256;

struct Fixture(PathBuf);

impl Fixture {
    fn new(name: &str, samples: &[f32]) -> Self {
        Self::at_rate(name, samples, RATE as u32)
    }

    fn at_rate(name: &str, samples: &[f32], sample_rate: u32) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kp-general-sampler-{name}-{}.wav",
            std::process::id()
        ));
        testkit::write_wav(&path, samples, sample_rate).unwrap();
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct TestSampler {
    processor: InternalProcessor,
    handle: SamplerHandle,
}

fn ready(
    source: &Path,
    changes: &[(&str, f64)],
    settings: &Settings,
    project_bpm: f64,
) -> TestSampler {
    let mut state = write_settings(None, settings);
    for &(param, value) in changes {
        let id = SPEC.wire(&ParamId::new(param)).unwrap();
        state = SPEC.with_value(Some(&state), id, value).unwrap();
    }
    let values = SPEC
        .decode_state(Some(&state))
        .unwrap_or_else(|_| SPEC.defaults());
    let (dsp, handle) = dsp_with_handle();
    handle.set_general_settings(&read_settings(Some(&state)));
    let mut processor = InternalProcessor::from_dsp(&SPEC, &values, dsp);
    processor.activate(RATE, BLOCK).unwrap();
    let (source_bpm, enabled) = sync_settings(Some(&state));
    handle.prepare_general_sync(source_bpm, project_bpm, enabled);
    handle.load_link(0, SampleLink::new(source, None), None);
    assert!(handle.wait_idle(Duration::from_secs(5)));
    // The first render will carry a frame-zero note, with no silent warmup block.
    TestSampler { processor, handle }
}

fn note_on(offset: u32, key: u8, note_id: u32) -> Event {
    Event {
        offset,
        kind: EventKind::NoteOn {
            channel: 0,
            key,
            note_id,
            velocity: 127,
        },
    }
}

fn note_off(offset: u32, key: u8, note_id: u32) -> Event {
    Event {
        offset,
        kind: EventKind::NoteOff {
            channel: 0,
            key,
            note_id,
        },
    }
}

fn render(sampler: &mut TestSampler, bpm: f64, events: &[Event], frames: usize) -> Vec<f32> {
    let context = ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: bpm,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: true,
        deadline: None,
    };
    let input = [0.0; BLOCK];
    let (mut left, mut right) = ([0.0; BLOCK], [0.0; BLOCK]);
    let mut io = StereoIo {
        input: [&input[..frames], &input[..frames]],
        output: [&mut left[..frames], &mut right[..frames]],
    };
    assert_no_alloc(|| sampler.processor.process(&context, events, &mut io));
    left[..frames].to_vec()
}

fn render_tone(path: &Path, key: u8) -> Vec<f32> {
    let bpm = 80.0;
    let mut processor = ready(
        path,
        &[("mode", 0.0), ("sync", 1.0), ("source_bpm", 120.0)],
        &Settings::default(),
        bpm,
    );
    let mut output = render(&mut processor, bpm, &[note_on(0, key, 1)], BLOCK);
    for _ in 0..100 {
        output.extend(render(&mut processor, bpm, &[], BLOCK));
    }
    output
}

fn energy(samples: &[f32]) -> f32 {
    samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32
}

fn tone_power(samples: &[f32], hz: f32) -> f32 {
    let (sin, cos) = samples
        .iter()
        .enumerate()
        .map(|(i, sample)| {
            let (sin, cos) = (i as f32 * hz * std::f32::consts::TAU / RATE).sin_cos();
            (sample * sin, sample * cos)
        })
        .fold((0.0, 0.0), |(s, c), (sin, cos)| (s + sin, c + cos));
    sin * sin + cos * cos
}

#[test]
fn offline_first_note_uses_prepared_tempo_and_transposition_preserves_its_duration() {
    let source = (0..RATE as usize)
        .map(|i| 0.4 * (i as f32 * 400.0 * std::f32::consts::TAU / RATE).sin())
        .collect::<Vec<_>>();
    let fixture = Fixture::new("tempo", &source);

    let root = render_tone(&fixture.0, 60);
    let octave = render_tone(&fixture.0, 72);
    let exact_end = (RATE * 1.5) as usize;
    for output in [&root, &octave] {
        assert!(energy(&output[..BLOCK]) > 1e-4, "the first note must sound");
        assert!(energy(&output[exact_end - 2400..exact_end - 800]) > 1e-3);
        assert!(energy(&output[exact_end + 32..]) < 1e-8);
    }
    let (lo, hi) = (RATE as usize / 3, RATE as usize * 2 / 3);
    assert!(tone_power(&root[lo..hi], 400.0) > 8.0 * tone_power(&root[lo..hi], 800.0));
    assert!(tone_power(&octave[lo..hi], 800.0) > 8.0 * tone_power(&octave[lo..hi], 400.0));
}

#[test]
fn overlapping_gates_follow_note_ids() {
    let source: Vec<_> = (0..2048)
        .map(|i| 0.15 + i as f32 * 0.003)
        .map(|x| x.min(0.65))
        .collect();
    let fixture = Fixture::new("overlap", &source);
    let mut processor = ready(
        &fixture.0,
        &[("amp_release", 0.0)],
        &Settings::default(),
        120.0,
    );
    let events = [
        note_on(0, 60, 1),
        note_on(16, 60, 2),
        note_off(48, 60, 2),
        note_off(80, 60, 1),
    ];
    let output = render(&mut processor, 120.0, &events, 128);
    processor.processor.reset();
    let reference = render(&mut processor, 120.0, &[note_on(0, 60, 1)], 128);
    assert!(output[32] > reference[32] + 0.1);
    assert!((output[64] - reference[64]).abs() < 0.001);
    assert!((reference[64] - reference[48]).abs() > 0.02);
    assert!(output[112].abs() < 1e-4);
}

#[test]
fn slices_mode_routes_notes_to_single_slices() {
    let split = Fixture::new("slices", &[vec![0.2; 1600], vec![0.55; 1600]].concat());
    let mut sliced = ready(
        &split.0,
        &[("root", 48.0), ("mode", 2.0)],
        &Settings::from_markers([0.1]),
        120.0,
    );
    let output = render(&mut sliced, 120.0, &[note_on(0, 49, 1)], 64);
    assert!(output[32] > 0.35, "the second slice is the louder half");
    sliced.processor.reset();
    let output = render(&mut sliced, 120.0, &[note_on(0, 50, 2)], 64);
    assert_eq!(energy(&output), 0.0, "keys without slices are silent");
}

#[test]
fn slices_use_their_own_direction_and_both_fades() {
    let fixture = Fixture::new(
        "slice-fades",
        &[
            vec![0.3; 128],
            (0..128).map(|i| 0.2 + i as f32 * 0.004).collect(),
        ]
        .concat(),
    );
    let mut settings = Settings::from_markers([128.0 / f64::from(RATE)]);
    settings.slices[0].options = super::SliceOptions {
        reverse: Some(true),
        fade_in_ms: Some(2.0),
        fade_out_ms: Some(3.0),
        ..Default::default()
    };
    let mut sliced = ready(&fixture.0, &[("mode", 2.0)], &settings, 120.0);
    let first = render(&mut sliced, 120.0, &[note_on(0, 60, 1)], 128);
    assert!((first[0] - first[127]).abs() < 0.001 && first[0] > 0.15);
    sliced.processor.reset();
    let second = render(&mut sliced, 120.0, &[note_on(0, 61, 2)], 128);
    assert!(second[0].abs() < 0.001 && second[127].abs() < 0.001);
    assert!(second[16] < second[32] && second[112] < second[80]);
    assert!(second[32] > second[80] * 1.5, "the louder end plays first");
}

#[test]
fn only_gate_slices_release_on_note_off_or_mouse_release() {
    let fixture = Fixture::new("slice-gate", &[0.3; 3200]);
    let mut settings = Settings::from_markers([0.1]);
    settings.slices[0].options.gate = true;
    let mut sliced = ready(
        &fixture.0,
        &[("mode", 2.0), ("amp_release", 0.0)],
        &settings,
        120.0,
    );
    let first = render(
        &mut sliced,
        120.0,
        &[note_on(0, 60, 1), note_off(32, 60, 1)],
        64,
    );
    assert!(first[63] > 0.15);
    sliced.processor.reset();
    let second = render(
        &mut sliced,
        120.0,
        &[note_on(0, 61, 2), note_off(32, 61, 2)],
        64,
    );
    assert!(second[16] > 0.15 && second[63].abs() < 0.001);
    sliced.processor.reset();

    let handle = sliced.handle.clone();
    handle.preview_slice(0.15);
    assert!(render(&mut sliced, 120.0, &[], 64)[63] > 0.1);
    handle.preview_slice(0.15);
    handle.release_slice_preview();
    assert!(render(&mut sliced, 120.0, &[], 256)[255].abs() < 0.001);
    handle.preview_slice(0.15);
    handle.release_slice_preview();
    assert!(render(&mut sliced, 120.0, &[], 64)[63].abs() < 0.001);

    handle.preview_slice(0.05);
    handle.release_slice_preview();
    assert!(render(&mut sliced, 120.0, &[], 64)[63] > 0.1);
}

#[test]
fn slice_preview_can_play_a_slice_outside_the_midi_mapping() {
    let sample = Fixture::new(
        "slice-preview",
        &[vec![0.2; 1600], vec![0.55; 256], vec![0.4; 1344]].concat(),
    );
    let mut sliced = ready(
        &sample.0,
        &[("root", 127.0), ("mode", 2.0), ("start", 0.1)],
        &Settings::from_markers([0.1]),
        120.0,
    );
    let handle = sliced.handle.clone();
    handle.preview_slice(0.15);
    assert!(render(&mut sliced, 120.0, &[], 64)[32] > 0.35);
    handle.preview_slice(0.05);
    assert!((0.1..0.2).contains(&render(&mut sliced, 120.0, &[], 128)[127]));
    sliced.processor.reset();
    handle.preview_slice(0.01);
    assert_eq!(energy(&render(&mut sliced, 120.0, &[], 64)), 0.0);
}

#[test]
fn retiming_a_playing_sampler_keeps_the_old_voice_alive() {
    let fixture = Fixture::new("tempo-change", &vec![0.3; RATE as usize]);
    let mut processor = ready(
        &fixture.0,
        &[("sync", 1.0), ("source_bpm", 120.0)],
        &Settings::default(),
        80.0,
    );
    let handle = processor.handle.clone();
    let start = render(&mut processor, 80.0, &[note_on(0, 60, 1)], BLOCK);
    assert!(energy(&start) > 1e-4);
    let change = render(&mut processor, 100.0, &[], BLOCK);
    assert!(energy(&change) > 1e-4);
    assert!(handle.wait_idle(Duration::from_secs(5)));
    assert_eq!(handle.sync_status(), Some(SyncStatus::Ready));
    let old_held = render(&mut processor, 100.0, &[], BLOCK);
    assert!(
        energy(&old_held) > 1e-4,
        "a tempo update must not cut the old voice"
    );
}

#[test]
fn a_source_that_cannot_be_prepared_reports_unavailable() {
    let fixture = Fixture::at_rate("unsupported-rate", &[0.25; 4000], 4000);
    let processor = ready(
        &fixture.0,
        &[("sync", 1.0), ("source_bpm", 120.0)],
        &Settings::default(),
        80.0,
    );
    assert_eq!(
        processor.handle.sync_status(),
        Some(SyncStatus::Unavailable)
    );
}
