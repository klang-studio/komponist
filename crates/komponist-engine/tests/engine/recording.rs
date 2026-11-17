//! Synthetic audio capture only. Every engine callback is guarded against allocation/free.
use crate::common::*;
use komponist_engine::compile::{self, NoEnv};
use komponist_engine::recording::{
    self, RecordingConfig, RecordingError, RecordingFormat, RecordingResult, RecordingSession,
    RecordingTrack,
};
use komponist_engine::{AudioInputBuffer, Command, LiveInput};
use komponist_model::edits::{self, ChannelProp, NewAudioTrack, NewBus, TrackProp};
use komponist_model::{
    AudioInput, HardwareInput, MonitorMode, Project, Tick, TickRange, TrackId, TrackOutput,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
static NEXT: AtomicU64 = AtomicU64::new(1);
fn directory() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "komponist-record-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn hardware() -> AudioInput {
    AudioInput::Hardware(HardwareInput {
        first: 0,
        stereo: true,
    })
}
fn project(mode: MonitorMode, arm: bool) -> (Project, TrackId) {
    let mut p = Project::new("Recording");
    p.count_in_bars = 0;
    let (p, track) = edits::add_audio_track(
        &p,
        NewAudioTrack {
            input: hardware(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = edits::set_track_prop(&p, &[track], TrackProp::MonitorMode(mode)).unwrap();
    (
        edits::set_track_prop(&p, &[track], TrackProp::Arm(arm)).unwrap(),
        track,
    )
}
fn prepare(
    p: &Project,
    tracks: &[TrackId],
    format: RecordingFormat,
) -> (Box<recording::Recorder>, RecordingSession, PathBuf) {
    let directory = directory();
    let (rec, session) = recording::prepare(RecordingConfig {
        directory: directory.clone(),
        sample_rate: 48000,
        format,
        tracks: tracks
            .iter()
            .map(|&id| RecordingTrack {
                track: id,
                name: "Take".into(),
                channels: 2,
                input: p.track(id).unwrap().audio.input,
            })
            .collect(),
    })
    .unwrap();
    (rec, session, directory)
}
fn feed(
    rig: &mut Rig,
    data: &[f32],
    output_latency: u32,
    input_latency: u32,
    discontinuous: bool,
) -> (Vec<f32>, Vec<f32>) {
    let frames = data.len() / 2;
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    let live = LiveInput {
        midi: &[],
        output_latency,
    };
    let audio = AudioInputBuffer {
        data,
        channels: 2,
        latency: input_latency,
        discontinuous,
    };
    assert_no_alloc::assert_no_alloc(|| {
        rig.engine.process_with_audio(&mut l, &mut r, &live, &audio)
    });
    (l, r)
}
fn finished(session: &mut RecordingSession) -> Result<RecordingResult, RecordingError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = session.poll() {
            return result;
        }
        assert!(Instant::now() < deadline, "writer did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn hardware_monitoring_off_auto_and_on_follow_arm_in_stopped_and_playing_states() {
    for playing in [false, true] {
        for (mode, arm, heard) in [
            (MonitorMode::Off, true, false),
            (MonitorMode::Auto, false, false),
            (MonitorMode::Auto, true, true),
            (MonitorMode::On, false, true),
        ] {
            let (p, track) = project(mode, arm);
            let program = compile::build(&p, None, &NoEnv);
            let meter = program.meter(track).unwrap().clone();
            let mut rig = Rig::new(RATE, 256);
            rig.set_program(&program);
            if playing {
                rig.send(Command::Play);
            }
            let input: Vec<_> = (0..256).flat_map(|_| [0.25, 0.5]).collect();
            let (l, r) = feed(&mut rig, &input, 0, 0, false);
            assert!(
                (l[255] - if heard { 0.25 } else { 0.0 }).abs() < 1e-6,
                "{mode:?}/{arm}/{playing}"
            );
            assert!((r[255] - if heard { 0.5 } else { 0.0 }).abs() < 1e-6);
            assert_eq!(meter.take_input(), (0.25, 0.5));
            assert_eq!(meter.take_input(), (0.0, 0.0));
        }
    }
}
#[test]
fn hardware_recording_corrects_both_latencies_and_drains_the_tail_after_stop() {
    let (p, track) = project(MonitorMode::Off, true);
    let (rec, mut session, _) = prepare(&p, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let input: Vec<_> = (0..512)
        .flat_map(|i| [i as f32 / 1000.0, -(i as f32) / 1000.0])
        .collect();
    let (l, r) = feed(&mut rig, &input, 6, 4, false);
    assert!(l.iter().chain(&r).all(|v| *v == 0.0));
    rig.send(Command::Stop);
    let tail: Vec<_> = (512..576)
        .flat_map(|i| [i as f32 / 1000.0, -(i as f32) / 1000.0])
        .collect();
    feed(&mut rig, &tail, 6, 4, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes.len(), 1);
    let take = &result.takes[0];
    assert_eq!((take.frames, take.start), (512, Tick::ZERO));
    let mut reader = hound::WavReader::open(&take.path).unwrap();
    let samples: Vec<f32> = reader.samples().map(Result::unwrap).collect();
    assert_eq!(samples[0], 0.010);
    assert_eq!(samples[samples.len() - 2], 0.521);
    assert_eq!(samples[samples.len() - 1], -0.521);
}
#[test]
fn cycle_recording_pads_first_and_last_partial_passes_to_the_common_loop_window() {
    let (mut p, track) = project(MonitorMode::Off, true);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick::ZERO, Tick(16));
    let (rec, mut session, _) = prepare(&p, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Locate(Tick(4)));
    rig.send(Command::Play);
    rig.send(Command::Record { from: Tick::ZERO });
    let input: Vec<_> = (0..600).flat_map(|_| [0.25, 0.5]).collect();
    feed(&mut rig, &input, 0, 0, false);
    let checkpoint = session.checkpoint().unwrap();
    assert_eq!(checkpoint[0].window_frames, Some(400));
    rig.send(Command::StopRecording);
    feed(&mut rig, &[0.0; 2], 0, 0, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes.len(), 2);
    for take in &result.takes {
        assert_eq!((take.frames, take.start), (400, Tick::ZERO));
    }
    let read = |i: usize| {
        hound::WavReader::open(&result.takes[i].path)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    };
    let first = read(0);
    let last = read(1);
    assert!(first[..200].iter().all(|s| *s == 0.0));
    assert_eq!(&first[200..204], &[0.25, 0.5, 0.25, 0.5]);
    assert!(last[600..].iter().all(|s| *s == 0.0));
}
#[test]
fn count_in_is_not_recorded_and_pcm24_headers_are_finalized() {
    let (mut p, track) = project(MonitorMode::Off, true);
    p.count_in_bars = 1;
    let (rec, mut session, _) = prepare(&p, &[track], RecordingFormat::Pcm24);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    feed(&mut rig, &vec![0.9; 96000 * 2], 0, 0, false);
    feed(&mut rig, &vec![0.5; 256 * 2], 0, 0, false);
    rig.send(Command::Stop);
    feed(&mut rig, &[0.0; 2], 0, 0, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes[0].frames, 256);
    let mut wav = hound::WavReader::open(&result.takes[0].path).unwrap();
    assert_eq!(wav.spec().bits_per_sample, 24);
    assert_eq!(wav.spec().sample_format, hound::SampleFormat::Int);
    assert!(wav.samples::<i32>().all(|s| s.unwrap() == 4194304));
}
#[test]
fn checkpoint_and_relocation_keep_capture_running_and_a_failed_move_keeps_old_paths() {
    let (p, track) = project(MonitorMode::Off, true);
    let (rec, mut session, dir) = prepare(&p, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    feed(&mut rig, &[0.25; 128], 0, 0, false);
    let checkpoint = session.checkpoint().unwrap();
    assert_eq!(checkpoint[0].frames, 64);
    let reader = hound::WavReader::open(&checkpoint[0].path).unwrap();
    assert_eq!(reader.duration(), 64);
    drop(reader);
    let blocked = dir.join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    assert!(session.relocate(&blocked).is_err());
    feed(&mut rig, &[0.25; 128], 0, 0, false);
    let checkpoint = session.checkpoint().unwrap();
    assert_eq!(checkpoint[0].frames, 128);
    assert!(checkpoint[0].path.starts_with(&dir));
    let destination = directory().join("Takes");
    let moved = session.relocate(&destination).unwrap();
    assert!(moved[0].path.starts_with(&destination));
    assert!(!checkpoint[0].path.exists());
    feed(&mut rig, &[0.25; 128], 0, 0, false);
    rig.send(Command::Stop);
    feed(&mut rig, &[0.0; 2], 0, 0, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes[0].frames, 192);
    assert!(result.takes[0].path.starts_with(destination));
    assert!(session.progress()[0].waveform.len() <= 4096);
}
#[test]
fn interrupted_input_and_rate_changes_fail_explicitly_and_cancel_removes_files() {
    for (case, expected) in [
        (0, RecordingError::Discontinuity),
        (1, RecordingError::InputUnavailable),
        (2, RecordingError::SampleRateChanged),
        (3, RecordingError::Cancelled),
        (4, RecordingError::SourceChanged),
    ] {
        let (p, track) = project(MonitorMode::Off, true);
        let (rec, mut session, dir) = prepare(&p, &[track], RecordingFormat::Float32);
        let mut rig = Rig::new(RATE, 256);
        rig.set_program(&compile::build(&p, None, &NoEnv));
        rig.send(Command::SetRecorder(rec));
        rig.send(Command::Record { from: Tick::ZERO });
        feed(&mut rig, &[0.25; 128], 0, 0, false);
        match case {
            0 => {
                feed(&mut rig, &[0.25; 128], 0, 0, true);
            }
            1 => {
                let mut l = [0.0; 64];
                let mut r = [0.0; 64];
                assert_no_alloc::assert_no_alloc(|| {
                    rig.engine.process(
                        &mut l,
                        &mut r,
                        &LiveInput {
                            midi: &[],
                            output_latency: 0,
                        },
                    )
                });
            }
            2 => {
                let _ = rig.engine.set_sample_rate(44100.0);
                feed(&mut rig, &[0.25; 128], 0, 0, false);
            }
            3 => session.cancel(),
            _ => {
                rig.send(Command::AbortAudioRecording);
                feed(&mut rig, &[0.25; 128], 0, 0, false);
            }
        }
        assert_eq!(finished(&mut session).unwrap_err(), expected);
        if case == 3 {
            assert_eq!(std::fs::read_dir(dir).unwrap().count(), 0);
        }
    }
}
#[test]
fn bus_recording_reads_post_fader_pan_mute_without_hardware_latency_correction() {
    for muted in [false, true] {
        let (mut p, source) = project(MonitorMode::On, false);
        let (p2, bus) = edits::add_bus(&p, NewBus::group(vec![source])).unwrap();
        p = p2;
        p = edits::set_track_prop(
            &p,
            &[bus],
            TrackProp::Channel(ChannelProp::VolumeDb(-6.0206)),
        )
        .unwrap();
        p = edits::set_track_prop(&p, &[bus], TrackProp::Channel(ChannelProp::Pan(1.0))).unwrap();
        p = edits::set_track_prop(&p, &[bus], TrackProp::Channel(ChannelProp::Mute(muted)))
            .unwrap();
        let (p, record) = edits::add_audio_track(
            &p,
            NewAudioTrack {
                input: AudioInput::Bus(bus),
                ..Default::default()
            },
        )
        .unwrap();
        let p = edits::set_track_prop(&p, &[record], TrackProp::Arm(true)).unwrap();
        assert_eq!(p.track(source).unwrap().output, TrackOutput::Bus(bus));
        let program = compile::build(&p, None, &NoEnv);
        let from = program.tracks.iter().position(|t| t.id == bus).unwrap();
        let to = program.tracks.iter().position(|t| t.id == record).unwrap();
        assert!(program.routing.node(from).post);
        assert!(program.routing.node(to).waits > 0);
        let (rec, mut session, _) = prepare(&p, &[record], RecordingFormat::Float32);
        let mut rig = Rig::new(RATE, 256);
        rig.engine.set_workers(2, None);
        rig.set_program(&program);
        rig.send(Command::SetRecorder(rec));
        rig.send(Command::Record { from: Tick::ZERO });
        feed(&mut rig, &[0.5; 512], 60, 40, false);
        rig.send(Command::Stop);
        feed(&mut rig, &[0.0; 2], 60, 40, false);
        let result = finished(&mut session).unwrap();
        assert_eq!(result.takes[0].frames, 256);
        let data = hound::WavReader::open(&result.takes[0].path)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert!(data[data.len() - 2].abs() < 1e-6);
        assert!((data[data.len() - 1] - if muted { 0.0 } else { 0.25 }).abs() < 1e-5);
    }
}

#[test]
fn mono_pcm24_records_the_selected_input_channel_and_saturates_at_full_scale() {
    let (p, track) = project(MonitorMode::Off, true);
    let input = AudioInput::Hardware(HardwareInput {
        first: 1,
        stereo: false,
    });
    let p = edits::set_track_prop(&p, &[track], TrackProp::AudioInput(input)).unwrap();
    let (rec, mut session) = recording::prepare(RecordingConfig {
        directory: directory(),
        sample_rate: 48000,
        format: RecordingFormat::Pcm24,
        tracks: vec![RecordingTrack {
            track,
            name: "Mono".into(),
            channels: 1,
            input,
        }],
    })
    .unwrap();
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let data: Vec<_> = (0..64).flat_map(|_| [1.5, -1.5]).collect();
    feed(&mut rig, &data, 0, 0, false);
    rig.send(Command::Stop);
    feed(&mut rig, &[0.0; 2], 0, 0, false);
    let result = finished(&mut session).unwrap();
    let mut reader = hound::WavReader::open(&result.takes[0].path).unwrap();
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.duration(), 64);
    assert!(reader.samples::<i32>().all(|s| s.unwrap() == -8388608));
}

#[test]
fn bus_latency_is_compensated_once_and_its_delayed_tail_is_drained() {
    use komponist_engine::compile::CompileEnv;
    use komponist_model::{Device, DeviceId, DeviceRole, PluginFormat, PluginRef};
    use std::sync::Arc;
    struct Env;
    impl CompileEnv for Env {
        fn midi_port(&self, _: &str) -> Option<komponist_engine::MidiPort> {
            None
        }
        fn device_latency(&self, _: DeviceId) -> u32 {
            32
        }
    }
    let (mut p, source) = project(MonitorMode::On, false);
    let device = DeviceId(p.alloc_id());
    Arc::make_mut(p.tracks.iter_mut().find(|t| t.id == source).unwrap())
        .channel
        .devices
        .push(Arc::new(Device {
            id: device,
            role: DeviceRole::Effect,
            plugin: PluginRef {
                format: PluginFormat::Builtin,
                id: "test".into(),
                name: "Delay32".into(),
                vendor: String::new(),
                version: String::new(),
                module: "".into(),
            },
            bypass: false,
            state: None,
            sidechain: None,
        }));
    let (p, bus) = edits::add_bus(&p, NewBus::group(vec![source])).unwrap();
    let (p, record) = edits::add_audio_track(
        &p,
        NewAudioTrack {
            input: AudioInput::Bus(bus),
            ..Default::default()
        },
    )
    .unwrap();
    let p = edits::set_track_prop(&p, &[record], TrackProp::Arm(true)).unwrap();
    let program = compile::build(&p, None, &Env);
    let (rec, mut session, _) = prepare(&p, &[record], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.send(Command::InsertProcessor {
        slot: program.slots.slot(device).unwrap(),
        processor: Box::new(komponist_engine::testing::TestGain::with_latency(1.0, 32)),
    });
    rig.set_program(&program);
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let data: Vec<_> = (0..256).flat_map(|i| [i as f32 / 1000.0; 2]).collect();
    feed(&mut rig, &data, 100, 100, false);
    rig.send(Command::Stop);
    feed(&mut rig, &[0.0; 128], 100, 100, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes[0].frames, 256);
    let samples = hound::WavReader::open(&result.takes[0].path)
        .unwrap()
        .samples::<f32>()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(samples[0], 0.0);
    assert_eq!(samples[2], 0.001);
    assert_eq!(samples[samples.len() - 2], 0.255);
}
