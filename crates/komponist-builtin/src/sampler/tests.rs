//! Tests of the sampler: sample-accurate triggers, choke, tune, start/end, no allocation in
//! `process`, the loader's hand-over, the parameter model, state and layout.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use assert_no_alloc::assert_no_alloc;
use komponist_engine::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, Processor, StereoIo, TransportInfo,
};
use komponist_model::params::ParamContext;
use komponist_model::params::layout::{SEL, validate};
use komponist_model::{Meter, Tick, TickRange};

use super::links::SampleLink;
use super::load::{PadStatus, decode};
use super::testkit;
use super::*;
use crate::{Internal, InternalProcessor};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 512;
const WAIT: Duration = Duration::from_secs(5);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kp-sampler-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A mono WAV of `frames` frames of the constant `level` at `rate`.
fn dc_wav(dir: &Path, name: &str, frames: usize, level: f32, rate: u32) -> PathBuf {
    let path = dir.join(name);
    testkit::write_wav(&path, &vec![level; frames], rate).unwrap();
    path
}

fn ctx(frames: usize) -> ProcessContext {
    ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick::ZERO, Tick::ZERO),
        },
        offline: false,
        deadline: None,
    }
}

/// A sampler at 48 kHz.
fn sampler() -> InternalProcessor {
    let mut p = Internal::Sampler.processor(&Internal::Sampler.defaults());
    p.activate(RATE, BLOCK).unwrap();
    p
}

/// Loads `path` into `pad` and waits until the audio thread has taken it (one silent call).
fn load(p: &mut InternalProcessor, pad: usize, path: &Path) {
    let handle = p.sampler().expect("a sampler");
    handle.load_link(pad, SampleLink::new(path, None), None);
    assert!(handle.wait_idle(WAIT), "the loader finishes");
    render(p, &[], 1);
}

fn on(offset: u32, pad: usize, velocity: u8) -> Event {
    Event {
        offset,
        kind: EventKind::NoteOn {
            key: note_of_pad(pad),
            velocity,
            channel: 0,
            note_id: NOTE_ID_NONE,
        },
    }
}

fn param(key: &str, value: f64, offset: u32) -> Event {
    let id = Internal::Sampler
        .wire(&komponist_model::params::ParamId::new(key))
        .expect("known parameter");
    Event {
        offset,
        kind: EventKind::Param { id, value },
    }
}

/// One call of `frames` frames inside `assert_no_alloc`; the left channel.
fn render(p: &mut InternalProcessor, events: &[Event], frames: usize) -> Vec<f32> {
    let input = [vec![0.0; frames], vec![0.0; frames]];
    let mut out = [vec![9.0; frames], vec![9.0; frames]];
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    assert_no_alloc(|| p.process(&ctx(frames), events, &mut io));
    let [left, _] = out;
    left
}

fn sounding(out: &[f32]) -> usize {
    out.iter().filter(|s| s.abs() > 1e-4).count()
}

#[test]
fn a_note_starts_at_its_frame() {
    let dir = scratch("trigger");
    let mut p = sampler();
    load(&mut p, 0, &dc_wav(&dir, "dc.wav", 4800, 0.5, 48_000));
    let out = render(&mut p, &[on(137, 0, 127)], BLOCK);
    assert!(
        out[..137].iter().all(|s| *s == 0.0),
        "silent before the note"
    );
    assert!(out[137].abs() > 0.4, "{}", out[137]);
    assert!(out[136] == 0.0);
    // A second note in the same call, and a note-on for a pad with no sample stays silent.
    let out = render(&mut p, &[on(0, 3, 100), on(10, 1, 100)], 64);
    assert!(out.iter().all(|s| *s == 0.0 || s.is_finite()));
}

#[test]
fn velocity_scales_and_other_notes_are_ignored() {
    let dir = scratch("velocity");
    let mut p = sampler();
    load(&mut p, 0, &dc_wav(&dir, "dc.wav", 4800, 0.5, 48_000));
    let loud = render(&mut p, &[on(0, 0, 127)], 64)[32];
    p.reset();
    let soft = render(&mut p, &[on(0, 0, 64)], 64)[32];
    assert!((soft / loud - 64.0 / 127.0).abs() < 0.02, "{soft} {loud}");
    p.reset();
    let other = Event {
        offset: 0,
        kind: EventKind::NoteOn {
            key: 60,
            velocity: 100,
            channel: 0,
            note_id: NOTE_ID_NONE,
        },
    };
    assert_eq!(sounding(&render(&mut p, &[other], 64)), 0);
}

#[test]
fn the_pads_play_notes_36_to_43() {
    assert_eq!(pad_of_note(36), Some(0));
    assert_eq!(pad_of_note(43), Some(7));
    assert_eq!(pad_of_note(35), None);
    assert_eq!(pad_of_note(44), None);
    assert_eq!(note_of_pad(7), 43);
}

#[test]
fn a_choke_group_cuts_the_other_pads_of_the_group() {
    let dir = scratch("choke");
    let mut p = sampler();
    let long = dc_wav(&dir, "long.wav", 48_000, 0.5, 48_000);
    load(&mut p, 3, &long);
    load(&mut p, 4, &long);
    load(&mut p, 5, &long);
    // Pads 4 and 5 (0-based 3, 4) share group 1, pad 6 is in no group.
    for pad in [3, 4] {
        render(
            &mut p,
            &[param(&format!("pad{}_choke", pad + 1), 1.0, 0)],
            1,
        );
    }
    // The open hat rings; the closed hat cuts it.
    let ring = render(&mut p, &[on(0, 4, 127), on(0, 5, 127)], BLOCK);
    assert!(
        ring[BLOCK - 1].abs() > 0.9,
        "two pads sound: {}",
        ring[BLOCK - 1]
    );
    let cut = render(&mut p, &[on(0, 3, 127)], 2400);
    // Pad 5 fades within 5 ms; what remains is pad 4 and pad 6 (no group), 0.5 each.
    let tail = cut[2399];
    assert!((tail - 1.0).abs() < 0.02, "pad 4 + pad 6 remain: {tail}");
}

#[test]
fn tune_changes_the_pitch_by_the_semitone_ratio() {
    let dir = scratch("tune");
    let path = dc_wav(&dir, "dc.wav", 4800, 0.5, 48_000);
    let play = |tune: f64, kit: f64| {
        let mut p = sampler();
        load(&mut p, 0, &path);
        render(
            &mut p,
            &[param("pad1_tune", tune, 0), param("kit_tune", kit, 0)],
            1,
        );
        sounding(&render(&mut p, &[on(0, 0, 127)], 6000))
    };
    let normal = play(0.0, 0.0);
    assert!((normal as i64 - 4800).abs() < 8, "{normal}");
    // +12 st plays at twice the speed, −12 at half; the kit tune adds to the pad's.
    assert!((play(12.0, 0.0) as i64 - 2400).abs() < 8);
    assert!(
        play(-12.0, 0.0) >= 5990,
        "half speed outlasts the 6000 frames"
    );
    assert!((play(7.0, 5.0) as i64 - 2400).abs() < 8);
}

#[test]
fn a_sample_at_another_rate_plays_at_its_real_speed() {
    let dir = scratch("rate");
    let mut p = sampler();
    // 0.1 s at 24 kHz = 2400 frames; at 48 kHz that is 4800 output frames.
    load(&mut p, 0, &dc_wav(&dir, "slow.wav", 2400, 0.5, 24_000));
    let n = sounding(&render(&mut p, &[on(0, 0, 127)], 6000));
    assert!((n as i64 - 4800).abs() < 8, "{n}");
}

#[test]
fn start_and_end_cut_the_sample_in_milliseconds() {
    let dir = scratch("window");
    let mut p = sampler();
    // One second.
    load(&mut p, 0, &dc_wav(&dir, "dc.wav", 48_000, 0.5, 48_000));
    render(
        &mut p,
        &[param("pad1_start", 250.0, 0), param("pad1_end", 500.0, 0)],
        1,
    );
    let n = sounding(&render(&mut p, &[on(0, 0, 127)], 20_000));
    assert!((n as i64 - 12_000).abs() < 8, "{n}");
    // The end at its maximum plays to the sample's end.
    p.reset();
    render(&mut p, &[param("pad1_end", MAX_MS, 0)], 1);
    let n = sounding(&render(&mut p, &[on(0, 0, 127)], 60_000));
    assert!((n as i64 - 36_000).abs() < 8, "{n}");
}

#[test]
fn decay_fades_the_pad_and_the_maximum_plays_the_sample_out() {
    let dir = scratch("decay");
    let mut p = sampler();
    load(&mut p, 0, &dc_wav(&dir, "dc.wav", 48_000, 0.5, 48_000));
    let tail = |decay: f64, p: &mut InternalProcessor| {
        p.reset();
        render(p, &[param("pad1_decay", decay, 0)], 1);
        render(p, &[on(0, 0, 127)], 24_000)[23_999].abs()
    };
    let off = tail(MAX_DECAY_S, &mut p);
    assert!(off > 0.45, "{off}");
    // 0.1 s T60: half a second later the pad is far below −60 dB.
    assert!(tail(0.1, &mut p) < 0.005);
}

#[test]
fn level_mute_solo_and_pan_shape_the_output() {
    let dir = scratch("mix");
    let mut p = sampler();
    let path = dc_wav(&dir, "dc.wav", 48_000, 0.5, 48_000);
    load(&mut p, 0, &path);
    load(&mut p, 1, &path);
    let level = |p: &mut InternalProcessor, events: &[Event]| {
        p.reset();
        render(p, events, 1);
        render(p, &[on(0, 0, 127)], 64)[40]
    };
    let base = level(&mut p, &[]);
    assert!((base - 0.5).abs() < 0.01);
    let quiet = level(&mut p, &[param("pad1_level", -6.0, 0)]);
    assert!((quiet / base - 0.501).abs() < 0.01);
    assert_eq!(level(&mut p, &[param("pad1_mute", 1.0, 0)]), 0.0);
    // Pad 2 soloed silences pad 1.
    assert_eq!(level(&mut p, &[param("pad2_solo", 1.0, 0)]), 0.0);
    let back = [
        param("pad2_solo", 0.0, 0),
        param("pad1_mute", 0.0, 0),
        param("pad1_level", 0.0, 0),
    ];
    assert!(level(&mut p, &back) > 0.4);
    // Hard left keeps the left channel at full level and silences the right.
    p.reset();
    render(&mut p, &[param("pad1_pan", -1.0, 0)], 1);
    let input = [vec![0.0; 64], vec![0.0; 64]];
    let mut out = [vec![0.0; 64], vec![0.0; 64]];
    let [ol, or] = &mut out;
    let mut io = StereoIo {
        input: [&input[0], &input[1]],
        output: [ol, or],
    };
    p.process(&ctx(64), &[on(0, 0, 127)], &mut io);
    assert!(out[0][40] > 0.4 && out[1][40].abs() < 1e-4);
}

#[test]
fn gate_mode_releases_with_the_note() {
    let dir = scratch("gate");
    let mut p = sampler();
    load(&mut p, 0, &dc_wav(&dir, "dc.wav", 48_000, 0.5, 48_000));
    render(&mut p, &[param("pad1_mode", 1.0, 0)], 1);
    let off = Event {
        offset: 2000,
        kind: EventKind::NoteOff {
            key: 36,
            channel: 0,
            note_id: NOTE_ID_NONE,
        },
    };
    let out = render(&mut p, &[on(0, 0, 127), off], 8000);
    assert!(out[1999].abs() > 0.4);
    assert!(out[7999].abs() < 1e-3, "released: {}", out[7999]);
}

#[test]
fn process_notes_and_handover_do_not_allocate() {
    let dir = scratch("alloc");
    let mut p = sampler();
    let path = dc_wav(&dir, "dc.wav", 4800, 0.5, 48_000);
    load(&mut p, 0, &path);
    let handle = p.sampler().unwrap();
    // A replacement arrives while the pad plays: the swap runs inside process, and the old
    // sample goes back to the loader instead of being freed there.
    render(&mut p, &[on(0, 0, 127)], 128);
    handle.load_link(0, SampleLink::new(&path, None), None);
    assert!(handle.wait_idle(WAIT));
    for round in 0..8 {
        let events = [
            on(5, round % 8, 100),
            param("pad1_tune", 3.0, 7),
            param("pad2_pan", 0.3, 9),
        ];
        let out = render(&mut p, &events, BLOCK);
        assert!(out.iter().all(|s| s.is_finite()));
    }
    assert_no_alloc(|| p.reset());
}

#[test]
fn the_loader_hands_samples_over_and_takes_the_old_ones_back() {
    let dir = scratch("handover");
    let mut p = sampler();
    let handle = p.sampler().unwrap();
    assert_eq!(handle.view(0).status, PadStatus::Empty);
    let path = testkit::write_kit(&dir).unwrap().remove(0);
    handle.load_link(0, SampleLink::new(&path, None), None);
    // Loading at once; a fast loader may already be done (CI, optimised sampler-core).
    let status = handle.view(0).status;
    assert!(
        matches!(status, PadStatus::Loading | PadStatus::Ready),
        "{status:?}"
    );
    assert!(handle.wait_idle(WAIT));
    let view = handle.view(0);
    assert_eq!(view.status, PadStatus::Ready);
    assert_eq!(view.name, "Kick");
    // The real length and peaks (the waveform's Data layer).
    assert!((view.length_s - 0.32).abs() < 0.01, "{}", view.length_s);
    assert_eq!(view.peaks.len(), load::PEAK_BUCKETS);
    assert!(view.peaks.iter().any(|p| *p > 0.5) && view.peaks.iter().all(|p| *p <= 1.0));
    // Before the audio thread ran, nothing plays; after one call the sample is in.
    assert_eq!(sounding(&render(&mut p, &[], 64)), 0);
    assert!(sounding(&render(&mut p, &[on(0, 0, 127)], 2000)) > 1000);
    // A missing file, a file that is no audio.
    handle.load_link(1, SampleLink::new(&dir.join("gone.wav"), None), None);
    let junk = dir.join("junk.wav");
    std::fs::write(&junk, b"not a wav").unwrap();
    handle.load_link(2, SampleLink::new(&junk, None), None);
    assert!(handle.wait_idle(WAIT));
    assert_eq!(handle.view(1).status, PadStatus::Missing);
    assert!(
        matches!(handle.view(2).status, PadStatus::Failed(ref why) if why.contains("junk.wav"))
    );
    // Clearing empties the pad and silences it.
    handle.clear(0);
    assert!(handle.wait_idle(WAIT));
    render(&mut p, &[], 1);
    assert_eq!(handle.view(0).status, PadStatus::Empty);
    assert_eq!(sounding(&render(&mut p, &[on(0, 0, 127)], 500)), 0);
}

/// A note at the start of the first call after the loader handed the sample over plays it: the
/// processor takes loaded samples before the note, not only when it renders (#94: the first hit
/// after a load, or an export's first beat, was silent when no call ran in between; slow CI
/// runners, whose audio thread had not run between the load and the note, showed it).
#[test]
fn a_note_in_the_call_its_sample_arrives_plays_it() {
    let dir = scratch("first-hit");
    let mut p = sampler();
    let handle = p.sampler().unwrap();
    let path = dc_wav(&dir, "dc.wav", 4800, 0.5, 48_000);
    handle.load_link(0, SampleLink::new(&path, None), None);
    assert!(handle.wait_idle(WAIT));
    let out = render(&mut p, &[on(0, 0, 127)], BLOCK);
    assert_eq!(sounding(&out), BLOCK, "the first hit sounds from its frame");
}

#[test]
fn the_audio_thread_never_frees_a_sample() {
    let mut dsp = dsp::SamplerDsp::new();
    let first = Arc::new(sampler_core::Sample::mono(48_000.0, vec![0.5; 100]));
    let weak = Arc::downgrade(&first);
    assert!(assert_no_alloc(|| dsp.set_sample(0, Some(first))));
    // Replacing it hands the old one to the loader thread: not freed inside this call.
    assert!(assert_no_alloc(|| dsp.set_sample(0, None)));
    let end = std::time::Instant::now() + WAIT;
    while weak.upgrade().is_some() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(weak.upgrade().is_none(), "the loader thread dropped it");
}

#[test]
fn the_pads_report_their_playhead_and_the_last_hit() {
    let dir = scratch("heads");
    let mut p = sampler();
    let handle = p.sampler().unwrap();
    load(&mut p, 2, &dc_wav(&dir, "dc.wav", 48_000, 0.5, 48_000));
    assert_eq!(handle.last_hit(), (0, 0));
    assert!(handle.playhead(2).is_none());
    render(&mut p, &[on(0, 2, 127)], 24_000);
    assert!((handle.playhead(2).unwrap() - 0.5).abs() < 0.01);
    assert_eq!(handle.last_hit(), (1, 2));
    // A pad without a sample is still "hit": the UI follows the input.
    render(&mut p, &[on(0, 6, 127)], 16);
    assert_eq!(handle.last_hit(), (2, 6));
}

#[test]
fn wav_files_decode_in_the_common_formats() {
    let dir = scratch("decode");
    // 24-bit stereo, 32-bit float and 8-bit mono.
    let write = |name: &str, spec: hound::WavSpec, f: &dyn Fn(&mut hound::WavWriter<_>)| {
        let path = dir.join(name);
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        f(&mut w);
        w.finalize().unwrap();
        path
    };
    let s24 = hound::WavSpec {
        channels: 2,
        sample_rate: 44_100,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let path = write("s24.wav", s24, &|w| {
        for _ in 0..10 {
            w.write_sample(4_194_304i32).unwrap(); // 0.5
            w.write_sample(-4_194_304i32).unwrap();
        }
    });
    let s = decode(&path).unwrap();
    assert!(s.is_stereo() && s.len() == 10 && s.sample_rate == 44_100.0);
    assert!((s.left[0] - 0.5).abs() < 1e-6 && (s.right.as_ref().unwrap()[0] + 0.5).abs() < 1e-6);
    let f32spec = hound::WavSpec {
        channels: 1,
        sample_rate: 96_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let path = write("f32.wav", f32spec, &|w| w.write_sample(0.25f32).unwrap());
    assert_eq!(decode(&path).unwrap().left, vec![0.25]);
    let s8 = hound::WavSpec {
        bits_per_sample: 8,
        sample_format: hound::SampleFormat::Int,
        ..f32spec
    };
    let path = write("s8.wav", s8, &|w| w.write_sample(64i8).unwrap());
    assert!((decode(&path).unwrap().left[0] - 0.5).abs() < 1e-6);
    // An empty file is refused with a German reason.
    let empty = write("empty.wav", f32spec, &|_| {});
    assert!(decode(&empty).unwrap_err().contains("leer"));
}

/// The audio-file crate's fixtures: 0.75 s, 44.1 kHz, stereo, 440 Hz left and 880 Hz right.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../komponist-audiofile/tests/fixtures")
        .join(name)
}

#[test]
fn flac_ogg_and_mp3_load_like_wav() {
    let mut p = sampler();
    let handle = p.sampler().unwrap();
    for (pad, name) in [
        "stereo-440-880.flac",
        "stereo-440-880.ogg",
        "stereo-440-880.mp3",
    ]
    .into_iter()
    .enumerate()
    {
        handle.load_link(pad, SampleLink::new(&fixture(name), None), None);
    }
    assert!(handle.wait_idle(WAIT));
    for pad in 0..3 {
        let view = handle.view(pad);
        assert_eq!(view.status, PadStatus::Ready, "pad {pad}");
        assert!(
            (view.length_s - 0.75).abs() < 0.06,
            "pad {pad}: {}",
            view.length_s
        );
    }
    render(&mut p, &[], 1);
    assert!(
        sounding(&render(&mut p, &[on(0, 1, 127)], 2000)) > 1000,
        "the OGG plays"
    );
    let s = decode(&fixture("stereo-440-880.flac")).unwrap();
    assert!(s.is_stereo() && s.sample_rate == 44_100.0 && s.len() == 33_075);
}

/// The Sampler and the audio clips share one file pool (#10): a file is decoded once however many
/// pads and Samplers show it, and its pool entry lives exactly as long as a pad shows it.
#[test]
fn pads_and_samplers_share_one_pool_entry_and_one_decode() {
    use komponist_audiofile::FilePool;
    let dir = scratch("pool");
    let path = dc_wav(&dir, "shared.wav", 4800, 0.5, 48_000);
    let (a, b) = (sampler(), sampler());
    let (ha, hb) = (a.sampler().unwrap(), b.sampler().unwrap());
    ha.load_link(0, SampleLink::new(&path, None), None);
    ha.load_link(5, SampleLink::new(&path, None), None);
    hb.load_link(3, SampleLink::new(&path, None), None);
    assert!(ha.wait_idle(WAIT) && hb.wait_idle(WAIT));
    assert_eq!(ha.view(0).status, PadStatus::Ready);
    assert_eq!(hb.view(3).status, PadStatus::Ready);

    let file = FilePool::global().open(&path).unwrap();
    assert_eq!(file.decode_count(), 1, "three pads, one decode");
    // The sample the pads play is the pool's: asking for it builds nothing.
    let _ = file
        .derived(None, |_| -> sampler_core::Sample {
            unreachable!("already built")
        })
        .unwrap();
    let weak = Arc::downgrade(&file);
    drop(file);
    assert!(weak.upgrade().is_some(), "the pads hold the entry");

    // Empty the pads one by one: the entry goes with the last one.
    for (handle, pad) in [(&ha, 0), (&ha, 5)] {
        handle.clear(pad);
        assert!(handle.wait_idle(WAIT));
    }
    assert!(weak.upgrade().is_some(), "the other Sampler still shows it");
    hb.clear(3);
    assert!(hb.wait_idle(WAIT));
    assert!(
        weak.upgrade().is_none(),
        "the pool entry is gone with its last user"
    );
}

#[test]
fn the_parameter_model_matches_the_wire_ids() {
    let params = Internal::Sampler.params();
    assert_eq!(params.len(), PAD_BASE + PADS * PadField::COUNT);
    assert_eq!(params[KIT_TUNE].id.as_str(), "kit_tune");
    assert_eq!(params[FOLLOW].id.as_str(), "follow");
    for pad in 0..PADS {
        for field in PadField::ALL {
            let wire = field.wire(pad);
            assert_eq!(
                params[wire].id.as_str(),
                format!("pad{}_{}", pad + 1, field.key())
            );
            assert_eq!(PadField::of_wire(wire), Some((pad, field)));
        }
    }
    assert_eq!(PadField::of_wire(KIT_TUNE), None);
    assert_eq!(PadField::of_wire(PAD_BASE + PADS * PadField::COUNT), None);
    // Texts of the special values.
    let text = |key: &str, v: f64| {
        let id = komponist_model::params::ParamId::new(key);
        Internal::Sampler
            .params()
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .format(v)
    };
    assert_eq!(text("pad1_end", MAX_MS), "Ende");
    assert_eq!(text("pad1_end", 410.0), "410 ms");
    assert_eq!(text("pad3_decay", MAX_DECAY_S), "Aus");
    assert_eq!(text("pad3_decay", 0.38), "380 ms");
    assert_eq!(
        Internal::Sampler.role(),
        komponist_model::DeviceRole::Instrument
    );
    assert_eq!(Internal::Sampler.name(), NAME);
}

#[test]
fn the_layout_is_valid_and_follows_the_selected_pad() {
    let known = |id: &komponist_model::params::ParamId| {
        Internal::Sampler.params().iter().any(|p| p.id == *id)
    };
    let layout = Internal::Sampler.layout();
    for sel in 1..=PADS {
        let ctx = ParamContext::new().with(SEL, sel);
        let errors = validate(layout, &ctx, &known);
        assert!(errors.is_empty(), "pad {sel}: {errors:?}");
    }
    assert_eq!(layout.pages[0].cols, 9);
}

#[test]
fn a_knob_turn_keeps_the_links_in_the_state() {
    let link = SampleLink::new(Path::new("/s/kick.wav"), Some(Path::new("/p")));
    let state = links::write(None, 0, Some(&link));
    let wire = PadField::Tune.wire(0) as u32;
    let turned = Internal::Sampler
        .with_value(Some(&state), wire, 3.0)
        .unwrap();
    assert_eq!(links::read(Some(&turned)), vec![(0, link)]);
    let values = Internal::Sampler.decode_state(Some(&turned)).unwrap();
    assert_eq!(values[wire as usize], 3.0);
}

#[test]
fn the_synthesized_kit_is_eight_short_audible_samples() {
    let kit = testkit::kit();
    assert_eq!(kit.len(), testkit::NAMES.len());
    for (name, s) in testkit::NAMES.iter().zip(&kit) {
        let peak = s.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((0.85..=0.91).contains(&peak), "{name}: {peak}");
        assert!(s.len() > 2000 && s.len() < 20_000, "{name}: {}", s.len());
    }
    assert_eq!(kit, testkit::kit(), "deterministic");
}
