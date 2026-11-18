//! Synthetic audio capture only. Every engine callback is guarded against allocation/free.
use crate::common::*;
use komponist_engine::compile::{self, NoEnv};
use komponist_engine::recording::{
    self, RecordingConfig, RecordingError, RecordingFormat, RecordingResult, RecordingSession,
    RecordingTrack,
};
use komponist_engine::{AudioInputBuffer, AudioInputRoutes, Command, LiveInput};
use komponist_model::edits::{self, ChannelProp, NewAudioTrack, NewBus, TrackProp};
use komponist_model::{
    AudioInput, HardwareInput, MonitorMode, Project, Tick, TickRange, TrackId, TrackOutput,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn directory() -> Scratch {
    loop {
        let dir = std::env::temp_dir().join(format!(
            "komponist-record-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Scratch(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create {}: {error}", dir.display()),
        }
    }
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
) -> (Scratch, Box<recording::Recorder>, RecordingSession) {
    let directory = directory();
    let (rec, session) = recording::prepare(RecordingConfig {
        directory: directory.0.clone(),
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
    (directory, rec, session)
}
fn feed(
    rig: &mut Rig,
    data: &[f32],
    output_latency: u32,
    input_latency: u32,
    discontinuous: bool,
) -> (Vec<f32>, Vec<f32>) {
    let audio = AudioInputBuffer {
        data,
        channels: 2,
        latency: input_latency,
        discontinuous,
        routes: None,
    };
    feed_routed(rig, output_latency, &audio)
}

fn feed_routed(
    rig: &mut Rig,
    output_latency: u32,
    audio: &AudioInputBuffer<'_>,
) -> (Vec<f32>, Vec<f32>) {
    let frames = audio.data.len() / audio.channels;
    let mut l = vec![0.0; frames];
    let mut r = vec![0.0; frames];
    let live = LiveInput {
        midi: &[],
        output_latency,
    };
    assert_no_alloc::assert_no_alloc(|| {
        rig.engine.process_with_audio(&mut l, &mut r, &live, audio)
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
    let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let input: Vec<_> = (0..512)
        .flat_map(|i| [i as f32 / 1000.0, -(i as f32) / 1000.0])
        .collect();
    // A driver can update its reported latency while every input frame still arrives.
    // Keep the take positioned with its initial correction through capture and drain.
    for (samples, output, capture) in [(&input[..512], 6, 4), (&input[512..], 32, 21)] {
        let (l, r) = feed(&mut rig, samples, output, capture, false);
        assert!(l.iter().chain(&r).all(|v| *v == 0.0));
    }
    rig.send(Command::Stop);
    let tail: Vec<_> = (512..576)
        .flat_map(|i| [i as f32 / 1000.0, -(i as f32) / 1000.0])
        .collect();
    feed(&mut rig, &tail, 16, 0, false);
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
    let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
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
    let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Pcm24);
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
fn checkpoints_include_all_samples_submitted_before_the_request() {
    let (p, track) = project(MonitorMode::Off, true);
    let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    for block in 1..=64 {
        feed(&mut rig, &[0.25; 128], 0, 0, false);
        let checkpoint = session.checkpoint().unwrap();
        let frames = block * 64;
        assert_eq!(checkpoint[0].frames, frames);
        let reader = hound::WavReader::open(&checkpoint[0].path).unwrap();
        assert_eq!(u64::from(reader.duration()), frames);
        assert!(reader.into_samples::<f32>().all(|s| s.unwrap() == 0.25));
    }
    rig.send(Command::Stop);
    feed(&mut rig, &[0.0; 2], 0, 0, false);
    assert_eq!(finished(&mut session).unwrap().takes[0].frames, 64 * 64);
}

#[test]
fn checkpoint_and_relocation_keep_capture_running_and_a_failed_move_keeps_old_paths() {
    let (p, track) = project(MonitorMode::Off, true);
    let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
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
    let blocked = _dir.0.join("blocked");
    std::fs::write(&blocked, b"not a directory").unwrap();
    assert!(session.relocate(&blocked).is_err());
    feed(&mut rig, &[0.25; 128], 0, 0, false);
    let checkpoint = session.checkpoint().unwrap();
    assert_eq!(checkpoint[0].frames, 128);
    assert!(checkpoint[0].path.starts_with(&_dir.0));
    let destination = _dir.0.join("Takes");
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
        let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
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
            assert_eq!(std::fs::read_dir(&_dir.0).unwrap().count(), 0);
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
        let (_dir, rec, mut session) = prepare(&p, &[record], RecordingFormat::Float32);
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
    let dir = directory();
    let (rec, mut session) = recording::prepare(RecordingConfig {
        directory: dir.0.clone(),
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
    let device = DeviceId(p.alloc_id().expect("fixture identifiers"));
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
    let (_dir, rec, mut session) = prepare(&p, &[record], RecordingFormat::Float32);
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

#[test]
fn double_stop_and_locate_preserve_hardware_and_bus_latency_tails() {
    use komponist_engine::compile::CompileEnv;
    use komponist_model::{Device, DeviceId, DeviceRole, PluginFormat, PluginRef};
    use std::sync::Arc;
    struct Env;
    impl CompileEnv for Env {
        fn midi_port(&self, _: &str) -> Option<komponist_engine::MidiPort> {
            None
        }
        fn device_latency(&self, _: DeviceId) -> u32 {
            400
        }
    }
    let (mut p, source) = project(MonitorMode::On, true);
    let device = DeviceId(p.alloc_id().expect("fixture identifiers"));
    Arc::make_mut(p.tracks.iter_mut().find(|t| t.id == source).unwrap())
        .channel
        .devices
        .push(Arc::new(Device {
            id: device,
            role: DeviceRole::Effect,
            plugin: PluginRef {
                format: PluginFormat::Builtin,
                id: "test".into(),
                name: "Delay400".into(),
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
    let (_dir, rec, mut session) = prepare(&p, &[source, record], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.send(Command::InsertProcessor {
        slot: program.slots.slot(device).unwrap(),
        processor: Box::new(komponist_engine::testing::TestGain::with_latency(1.0, 400)),
    });
    rig.set_program(&program);
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let input = |from: usize, n: usize| {
        (from..from + n)
            .flat_map(|i| [i as f32 / 2048.0; 2])
            .collect::<Vec<_>>()
    };
    feed(&mut rig, &input(0, 512), 256, 256, false);
    rig.send(Command::Stop);
    feed(&mut rig, &input(512, 100), 256, 256, false);
    rig.send(Command::Stop);
    rig.send(Command::Locate(Tick::ZERO));
    // The speaker fade finishes before either pending capture tail. Resetting effects now
    // would erase the final 44 frames still waiting in the bus's 400-frame processor delay.
    feed(&mut rig, &input(612, 256), 256, 256, false);
    feed(&mut rig, &input(868, 256), 256, 256, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes.len(), 2);
    for take in &result.takes {
        assert_eq!((take.start, take.frames), (Tick::ZERO, 512));
        let samples = hound::WavReader::open(&take.path)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        let expected = if take.track == source {
            1023.0 / 2048.0
        } else {
            511.0 / 2048.0
        };
        assert_eq!(samples[samples.len() - 1], expected);
    }
}

#[test]
fn night_drive_recording_before_its_loop_keeps_the_linear_take() {
    let (mut project, track) = project(MonitorMode::Off, true);
    let demo = komponist_model::fixtures::demo_project();
    project.tempo = demo.tempo;
    project.meter = demo.meter;
    project.loop_on = demo.loop_on;
    project.loop_range = demo.loop_range;
    project.count_in_bars = demo.count_in_bars;
    let map = komponist_model::TempoMap::constant(project.tempo, project.meter);
    let rate = f64::from(RATE);
    let span = project.meter.ticks_per_bar();
    assert_eq!(map.tick_to_sample(project.loop_range.start, rate), 743_226);
    for (from, first_frame) in [(Tick(5 * span), 464_516), (Tick::ZERO, 0)] {
        let (_dir, rec, mut session) = prepare(&project, &[track], RecordingFormat::Float32);
        assert_eq!(map.tick_to_sample(from, rate), first_frame);
        let mut rig = Rig::new(RATE, 256);
        rig.set_program(&compile::build(&project, None, &NoEnv));
        rig.send(Command::SetRecorder(rec));
        rig.send(Command::Record { from });
        let begin = Tick(from.0 - i64::from(project.count_in_bars) * span);
        let count_in_frames = (first_frame - map.tick_to_sample(begin, rate)) as usize;
        feed(&mut rig, &vec![0.9; count_in_frames * 2], 0, 0, false);
        feed(&mut rig, &[0.25; 256], 0, 0, false);
        rig.send(Command::Stop);
        feed(&mut rig, &[0.0; 2], 0, 0, false);
        let result = finished(&mut session);
        assert!(
            result.is_ok(),
            "demo recording at frame {first_frame}: {result:?}"
        );
        let result = result.unwrap();
        assert_eq!(result.takes.len(), 1);
        let take = &result.takes[0];
        assert_eq!(take.start, map.sample_to_tick(first_frame, rate));
        assert_eq!((take.frames, take.window_frames, take.lap), (128, None, 0));
        assert!(
            hound::WavReader::open(&take.path)
                .unwrap()
                .into_samples::<f32>()
                .all(|sample| sample.unwrap() == 0.25)
        );
    }
}

#[test]
fn recording_from_before_the_loop_keeps_the_intro_and_full_and_partial_laps() {
    let (mut p, track) = project(MonitorMode::Off, true);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(8), Tick(16)); // Frames 200..400 at 120 BPM.
    let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&compile::build(&p, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let input: Vec<_> = (0..660)
        .flat_map(|i| {
            let value = i as f32 / 1000.0;
            [value, -value]
        })
        .collect();
    feed(&mut rig, &input[..1300], 6, 4, false);

    let checkpoint = session.checkpoint().unwrap();
    assert_eq!(checkpoint.len(), 4);
    assert_eq!(
        (checkpoint[0].frames, checkpoint[0].window_frames),
        (200, None)
    );
    assert_eq!(
        (checkpoint[3].frames, checkpoint[3].window_frames),
        (40, Some(200))
    );
    rig.send(Command::Stop);
    feed(&mut rig, &input[1300..], 6, 4, false);
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes.len(), 4);
    for (i, take) in result.takes.iter().enumerate() {
        let (start, window) = if i == 0 {
            (Tick::ZERO, None)
        } else {
            (Tick(8), Some(200))
        };
        assert_eq!(
            (take.lap, take.start, take.frames, take.window_frames),
            (i as u32, start, 200, window)
        );
        let mut reader = hound::WavReader::open(&take.path).unwrap();
        let captured: Vec<f32> = reader.samples().map(Result::unwrap).collect();
        let from = 2 * (10 + i * 200);
        let to = (from + 400).min(input.len());
        assert_eq!(&captured[..to - from], &input[from..to]);
        assert!(captured[to - from..].iter().all(|&sample| sample == 0.0));
    }
}

#[test]
fn recording_at_or_past_loop_end_drains_the_latency_tail_without_wrapping() {
    let (mut p, track) = project(MonitorMode::Off, true);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(8), Tick(16));
    let input: Vec<_> = (0..74)
        .flat_map(|i| {
            let value = i as f32 / 100.0;
            [value, -value]
        })
        .collect();
    for from in [Tick(16), Tick(24)] {
        let (_dir, rec, mut session) = prepare(&p, &[track], RecordingFormat::Float32);
        let mut rig = Rig::new(RATE, 256);
        rig.set_program(&compile::build(&p, None, &NoEnv));
        rig.send(Command::SetRecorder(rec));
        rig.send(Command::Record { from });
        feed(&mut rig, &input[..128], 6, 4, false);
        rig.send(Command::Stop);
        feed(&mut rig, &input[128..], 6, 4, false);
        let result = finished(&mut session).unwrap();
        assert_eq!(result.takes.len(), 1);
        let take = &result.takes[0];
        assert_eq!(
            (take.lap, take.start, take.frames, take.window_frames),
            (0, from, 64, None)
        );
        let mut reader = hound::WavReader::open(&take.path).unwrap();
        let captured: Vec<f32> = reader.samples().map(Result::unwrap).collect();
        assert_eq!(captured, input[20..]);
    }
}

#[test]
fn healthy_jack_mono_uses_its_own_latency_through_an_unrelated_port_gap() {
    let (mut project, track) = project(MonitorMode::Off, true);
    let input = AudioInput::Hardware(HardwareInput {
        first: 0,
        stereo: false,
    });
    project = edits::set_track_prop(&project, &[track], TrackProp::AudioInput(input)).unwrap();
    let (_scratch, rec, mut session) = prepare(&project, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 64);
    rig.set_program(&compile::build(&project, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    for offset in (0..576).step_by(64) {
        if offset == 512 {
            rig.send(Command::Stop);
        }
        let data: Vec<_> = (offset..offset + 64)
            .flat_map(|i| [i as f32 / 1024.0, 0.0])
            .collect();
        feed_routed(
            &mut rig,
            0,
            &AudioInputBuffer {
                data: &data,
                channels: 2,
                latency: 256,
                discontinuous: offset == 128,
                routes: Some(AudioInputRoutes {
                    latencies: &[64, 256],
                    available: if offset < 128 { 3 } else { 1 },
                    discontinuous: if offset == 128 { 2 } else { 0 },
                    history: &[],
                    history_cursor: 0,
                    history_valid: &[0; 2],
                }),
            },
        );
    }
    let result = finished(&mut session).unwrap();
    let take = &result.takes[0];
    assert_eq!((take.frames, take.start), (512, Tick::ZERO));
    let captured: Vec<_> = hound::WavReader::open(&take.path)
        .unwrap()
        .into_samples::<f32>()
        .map(Result::unwrap)
        .collect();
    for (i, frame) in captured.as_chunks::<2>().0.iter().enumerate() {
        assert_eq!(*frame, [(i + 64) as f32 / 1024.0; 2]);
    }
}

#[test]
fn differently_delayed_jack_stereo_legs_share_aligned_samples_and_a_stop_tail() {
    let (project, track) = project(MonitorMode::Off, true);
    let (_scratch, rec, mut session) = prepare(&project, &[track], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 64);
    rig.set_program(&compile::build(&project, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let source = |first: i32, end| {
        (first..end)
            .flat_map(|i| [(i - 64) as f32 / 1024.0, (i - 256) as f32 / 1024.0])
            .collect::<Vec<_>>()
    };
    for offset in (0..768).step_by(64) {
        if offset == 512 {
            rig.send(Command::Stop);
        }
        let data = source(offset, offset + 64);
        let history = source(offset - 192, offset);
        feed_routed(
            &mut rig,
            0,
            &AudioInputBuffer {
                data: &data,
                channels: 2,
                latency: 256,
                discontinuous: false,
                routes: Some(AudioInputRoutes {
                    latencies: if offset < 256 {
                        &[64, 256]
                    } else {
                        &[128, 320]
                    },
                    available: 3,
                    discontinuous: 0,
                    history: &history,
                    history_cursor: 0,
                    history_valid: &[192; 2],
                }),
            },
        );
    }
    let result = finished(&mut session).unwrap();
    let take = &result.takes[0];
    assert_eq!((take.frames, take.start), (512, Tick::ZERO));
    let captured: Vec<_> = hound::WavReader::open(&take.path)
        .unwrap()
        .into_samples::<f32>()
        .map(Result::unwrap)
        .collect();
    for (i, frame) in captured.as_chunks::<2>().0.iter().enumerate() {
        assert_eq!(*frame, [i as f32 / 1024.0; 2]);
    }
}

#[test]
fn changed_jack_stereo_skew_ends_the_take_with_only_the_aligned_prefix() {
    for change_at in [384, 576] {
        let (project, track) = project(MonitorMode::Off, true);
        let (scratch, rec, mut session) = prepare(&project, &[track], RecordingFormat::Float32);
        let mut rig = Rig::new(RATE, 64);
        rig.set_program(&compile::build(&project, None, &NoEnv));
        rig.send(Command::SetRecorder(rec));
        rig.send(Command::Record { from: Tick::ZERO });
        let source = |first: i32, end, [left, right]: [i32; 2]| {
            (first..end)
                .flat_map(|i| [(i - left) as f32 / 1024.0, (i - right) as f32 / 1024.0])
                .collect::<Vec<_>>()
        };
        for offset in (0..=change_at).step_by(64) {
            if offset == 512 {
                rig.send(Command::Stop);
            }
            let latencies = if offset < change_at {
                [64, 256]
            } else {
                [256, 64]
            };
            let data = source(offset, offset + 64, latencies.map(|value| value as i32));
            let history = source(offset - 192, offset, [64, 256]);
            feed_routed(
                &mut rig,
                0,
                &AudioInputBuffer {
                    data: &data,
                    channels: 2,
                    latency: 256,
                    discontinuous: false,
                    routes: Some(AudioInputRoutes {
                        latencies: &latencies,
                        available: 3,
                        discontinuous: 0,
                        history: &history,
                        history_cursor: 0,
                        history_valid: &[192; 2],
                    }),
                },
            );
        }
        assert_eq!(
            finished(&mut session).unwrap_err(),
            RecordingError::Discontinuity
        );
        let files: Vec<_> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        let wav = hound::WavReader::open(&files[0]).unwrap();
        assert_eq!(wav.duration(), (change_at - 256) as u32);
        for (i, sample) in wav.into_samples::<f32>().enumerate() {
            assert_eq!(sample.unwrap(), (i / 2) as f32 / 1024.0);
        }
    }
}

#[test]
fn stereo_monitoring_silences_history_before_a_confirmed_port_gap() {
    let (project, _) = project(MonitorMode::On, false);
    let mut rig = Rig::new(RATE, 64);
    rig.set_program(&compile::build(&project, None, &NoEnv));
    let data: Vec<_> = (0..64).flat_map(|_| [0.25, 0.5]).collect();
    let history: Vec<_> = (0..4).flat_map(|_| [0.9, 0.9]).collect();
    let (left, right) = feed_routed(
        &mut rig,
        0,
        &AudioInputBuffer {
            data: &data,
            channels: 2,
            latency: 4,
            discontinuous: true,
            routes: Some(AudioInputRoutes {
                latencies: &[0, 4],
                available: 3,
                discontinuous: 1,
                history: &history,
                history_cursor: 0,
                history_valid: &[0, 4],
            }),
        },
    );
    assert_eq!(&left[..4], &[0.0; 4]);
    assert!(left[4..].iter().all(|&value| value == 0.25));
    assert!(right.iter().all(|&value| value == 0.5));
}

#[test]
fn a_shorter_finished_jack_lane_cannot_abort_another_lanes_tail() {
    let (project, left) = project(MonitorMode::Off, true);
    let mono = |first| {
        AudioInput::Hardware(HardwareInput {
            first,
            stereo: false,
        })
    };
    let project = edits::set_track_prop(&project, &[left], TrackProp::AudioInput(mono(0))).unwrap();
    let (project, right) = edits::add_audio_track(
        &project,
        NewAudioTrack {
            input: mono(1),
            ..NewAudioTrack::default()
        },
    )
    .unwrap();
    let project = edits::set_track_prop(&project, &[right], TrackProp::Arm(true)).unwrap();
    let (_scratch, rec, mut session) = prepare(&project, &[left, right], RecordingFormat::Float32);
    let mut rig = Rig::new(RATE, 64);
    rig.set_program(&compile::build(&project, None, &NoEnv));
    rig.send(Command::SetRecorder(rec));
    rig.send(Command::Record { from: Tick::ZERO });
    let data: Vec<_> = (0..768)
        .flat_map(|i| [i as f32 / 1024.0, -(i as f32) / 1024.0])
        .collect();
    for offset in (0..768).step_by(64) {
        if offset == 512 {
            rig.send(Command::Stop);
        }
        let lost = offset >= 576;
        feed_routed(
            &mut rig,
            0,
            &AudioInputBuffer {
                data: &data[offset * 2..(offset + 64) * 2],
                channels: 2,
                latency: 256,
                discontinuous: lost,
                routes: Some(AudioInputRoutes {
                    latencies: &[64, 256],
                    available: if lost { 2 } else { 3 },
                    discontinuous: u64::from(lost),
                    history: &[],
                    history_cursor: 0,
                    history_valid: &[0; 2],
                }),
            },
        );
    }
    let result = finished(&mut session).unwrap();
    assert_eq!(result.takes.len(), 2);
    for take in result.takes {
        assert_eq!((take.frames, take.start), (512, Tick::ZERO));
        let captured = hound::WavReader::open(&take.path)
            .unwrap()
            .into_samples::<f32>()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        for (i, frame) in captured.as_chunks::<2>().0.iter().enumerate() {
            let expected = if take.track == left {
                (i + 64) as f32
            } else {
                -((i + 256) as f32)
            };
            assert_eq!(*frame, [expected / 1024.0; 2]);
        }
    }
}

#[test]
fn input_trim_is_shared_by_capture_meter_and_monitoring() {
    for stereo in [false, true] {
        for gain_db in [0, 200] {
            for format in [RecordingFormat::Float32, RecordingFormat::Pcm24] {
                let (p, track) = project(MonitorMode::On, true);
                let input = AudioInput::Hardware(HardwareInput { first: 0, stereo });
                let p = edits::set_track_prop(&p, &[track], TrackProp::AudioInput(input)).unwrap();
                let p = edits::set_track_prop(&p, &[track], TrackProp::InputGain(gain_db)).unwrap();
                let scratch = directory();
                let (rec, mut session) = recording::prepare(RecordingConfig {
                    directory: scratch.0.clone(),
                    sample_rate: 48000,
                    format,
                    tracks: vec![RecordingTrack {
                        track,
                        name: "Take".into(),
                        channels: if stereo { 2 } else { 1 },
                        input,
                    }],
                })
                .unwrap();
                let program = compile::build(&p, None, &NoEnv);
                let meter = program.meter(track).unwrap().clone();
                let mut rig = Rig::new(RATE, 256);
                rig.set_program(&program);
                rig.send(Command::SetRecorder(rec));
                rig.send(Command::Record { from: Tick::ZERO });
                let input: Vec<_> = (0..256).flat_map(|_| [0.025, 0.05]).collect();
                // JACK's right leg arrives four frames later. The trim applies to the
                // aligned input, and both legs use that same gain for monitoring and capture.
                let routes = AudioInputRoutes {
                    latencies: &[0, 4],
                    available: 3,
                    discontinuous: 0,
                    history: &[0.0; 8],
                    history_cursor: 0,
                    history_valid: &[0; 2],
                };
                let audio = AudioInputBuffer {
                    data: &input,
                    channels: 2,
                    latency: 4,
                    discontinuous: false,
                    routes: Some(routes),
                };
                let (l, r) = feed_routed(&mut rig, 0, &audio);
                if stereo {
                    assert_eq!(&l[..4], &[0.0; 4]);
                }
                let factor = if gain_db == 0 { 1.0 } else { 10.0 };
                let expected = (
                    0.025 * factor,
                    if stereo {
                        0.05 * factor
                    } else {
                        0.025 * factor
                    },
                );
                assert!((l[255] - expected.0).abs() < 1e-5);
                assert!((r[255] - expected.1).abs() < 1e-5);
                let measured = meter.take_input();
                assert!((measured.0 - expected.0).abs() < 1e-5);
                assert!((measured.1 - expected.1).abs() < 1e-5);
                rig.send(Command::Stop);
                let tail = AudioInputBuffer {
                    data: &input[..8],
                    routes: Some(AudioInputRoutes {
                        history: &input[input.len() - 8..],
                        history_valid: &[4; 2],
                        ..routes
                    }),
                    ..audio
                };
                feed_routed(&mut rig, 0, &tail);
                let result = finished(&mut session).unwrap();
                assert_eq!(result.takes[0].frames, 256);
                let mut reader = hound::WavReader::open(&result.takes[0].path).unwrap();
                let samples: Vec<f32> = match format {
                    RecordingFormat::Float32 => {
                        reader.samples::<f32>().map(Result::unwrap).collect()
                    }
                    RecordingFormat::Pcm24 => reader
                        .samples::<i32>()
                        .map(|v| v.unwrap() as f32 / 8_388_608.0)
                        .collect(),
                };
                assert!((samples[0] - expected.0).abs() < 1e-5);
                assert_eq!(reader.spec().channels, if stereo { 2 } else { 1 });
                if stereo {
                    assert!((samples[1] - expected.1).abs() < 1e-5);
                }
            }
        }
    }
}

#[test]
fn input_trim_meter_reports_gain_induced_clipping() {
    let (p, track) = project(MonitorMode::On, true);
    let p = edits::set_track_prop(&p, &[track], TrackProp::InputGain(200)).unwrap();
    let program = compile::build(&p, None, &NoEnv);
    let meter = program.meter(track).unwrap().clone();
    let mut rig = Rig::new(RATE, 256);
    rig.set_program(&program);
    let (l, _) = feed(&mut rig, &[0.2; 512], 0, 0, false);
    assert!(l[255] > 1.0);
    assert!(meter.take_input().0 > 1.0);
}
