//! Writes the seed corpora of the fuzz targets into `corpus/<target>/`, from the repository's
//! fixtures and from valid encodings made with the crates' own encoders. Run from anywhere:
//! `cargo run --manifest-path fuzz/Cargo.toml --bin seed-corpora`.

use std::io::Read;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use komponist_audiofile::peaks::cache;
use komponist_audiofile::{FileId, PeakData};
use komponist_ipc::control::{BridgeChannel, BridgeMsg, ControlMsg, LogLevel, socket_pair};
use komponist_ipc::shm::{WireEvent, WireEventKind};
use komponist_plugin_api::params::ParamInfo;
use komponist_plugin_api::{device_assets, sampler_ui, waveform_markers};

fn main() {
    let fuzz = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = fuzz.parent().expect("fuzz/ is inside the repository");
    let corpus = |target: &str| {
        let dir = fuzz.join("corpus").join(target);
        std::fs::create_dir_all(&dir).expect("corpus folder");
        dir
    };
    let write = |dir: &PathBuf, name: &str, bytes: &[u8]| {
        std::fs::write(dir.join(name), bytes).expect("seed file");
    };

    // Control messages: payloads, and the frames a bridge writes for them.
    let messages = bridge_messages();
    let payloads = corpus("control_decode");
    for (i, msg) in messages.iter().enumerate() {
        write(
            &payloads,
            &format!("bridge-{i}"),
            &postcard::to_stdvec(msg).unwrap(),
        );
    }
    for (i, msg) in [
        ControlMsg::ListParams,
        ControlMsg::Shutdown,
        ControlMsg::Deactivate,
    ]
    .iter()
    .enumerate()
    {
        write(
            &payloads,
            &format!("app-{i}"),
            &postcard::to_stdvec(msg).unwrap(),
        );
    }
    let stream = corpus("control_stream");
    for (i, msg) in messages.iter().enumerate() {
        write(&stream, &format!("bridge-{i}"), &wire_of(msg));
    }
    write(
        &stream,
        "all",
        &messages.iter().flat_map(wire_of).collect::<Vec<_>>(),
    );

    // Wire events: one of each kind.
    let events = corpus("wire_event");
    for (i, kind) in [
        WireEventKind::NoteOn {
            channel: 1,
            key: 60,
            velocity: 100,
            note_id: 7,
        },
        WireEventKind::NoteOff {
            channel: 1,
            key: 60,
            note_id: u32::MAX,
        },
        WireEventKind::Midi([0xb0, 64, 127]),
        WireEventKind::ParamValue { id: 3, value: 0.5 },
        WireEventKind::ParamGesture { id: 3, begin: true },
        WireEventKind::AllNotesOff,
    ]
    .into_iter()
    .enumerate()
    {
        write(
            &events,
            &format!("kind-{i}"),
            &wire_event_bytes(&WireEvent::new(i as u32, kind)),
        );
    }

    // Native layouts: the documented example and the C fixture.
    let layouts = corpus("native_layout");
    for path in [
        "docs/clap-device-layout/example.json",
        "test-plugins/native-layout/c-fixture/controls.json",
    ] {
        let name = path.rsplit('/').next().unwrap();
        write(
            &layouts,
            name,
            &std::fs::read(repo.join(path)).expect("layout fixture"),
        );
    }

    // Plugin messages of the komponist extensions.
    let plugin = corpus("plugin_messages");
    for (i, bytes) in plugin_message_seeds().into_iter().enumerate() {
        write(&plugin, &format!("message-{i}"), &bytes);
    }

    // VST3 state blobs and stream calls.
    let states = corpus("vst3_state");
    for (i, (component, controller)) in [
        (&b"component"[..], &b"ctl"[..]),
        (b"", b""),
        (&[7; 300], b""),
    ]
    .into_iter()
    .enumerate()
    {
        write(
            &states,
            &format!("state-{i}"),
            &vst3_state(component, controller),
        );
    }
    let calls = corpus("vst3_stream");
    // Initial length 3, "abc"; then write 4, seek set 0, read 8, tell, size, set size 2.
    write(
        &calls,
        "calls-0",
        &[
            3, b'a', b'b', b'c', 2, 4, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 6, 0, 0, 8,
            0, 0, 7, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0,
        ],
    );
    write(
        &calls,
        "calls-1",
        &[
            0, 1, 0, 0, 3, 0, 0, 4, 1, 0, 255, 255, 255, 255, 255, 255, 255, 127,
        ],
    );

    // Audio files: the compressed fixtures and WAVs of the sampler kit, behind the pick byte
    // (extension index in the low bits, see the target).
    let audio = corpus("audio_file");
    for (pick, name) in [
        (2u8, "stereo-440-880.flac"),
        (3, "stereo-440-880.ogg"),
        (4, "stereo-440-880.mp3"),
    ] {
        let bytes = std::fs::read(
            repo.join("crates/komponist-audiofile/tests/fixtures")
                .join(name),
        )
        .expect("audio fixture");
        write(&audio, name, &[&[pick][..], &bytes].concat());
    }
    let scratch = std::env::temp_dir().join(format!("komponist-fuzz-seed-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch folder");
    for (i, path) in komponist_audiofile::fixtures::write_kit(&scratch)
        .expect("kit")
        .into_iter()
        .enumerate()
        .take(3)
    {
        // Short: the first 4 KiB keep the header and some frames.
        let mut bytes = std::fs::read(&path).expect("kit file");
        bytes.truncate(4096);
        write(
            &audio,
            &format!("kit-{i}.wav"),
            &[&[0u8][..], &bytes].concat(),
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);

    // Peak cache records.
    let peaks = corpus("peak_cache");
    let id = FileId {
        path: PathBuf::from("/fuzz/input.wav"),
        len: 4096,
        mtime_ns: Some(1_700_000_000_000_000_000),
    };
    for (i, frames) in [1usize, 128, 1000, 9000].into_iter().enumerate() {
        let left: Vec<f32> = (0..frames)
            .map(|n| ((n * 7) % 100) as f32 / 100.0 - 0.5)
            .collect();
        let right: Vec<f32> = left.iter().map(|s| -s).collect();
        let data = PeakData::from_planar(48_000, &[left, right][..1 + i % 2]);
        write(&peaks, &format!("record-{i}"), &cache::to_bytes(&data, &id));
    }

    // Resampler: rate bytes, engine rate pick, channels, then chunk sizes.
    let resampler = corpus("resampler");
    for (i, seed) in [
        &[0x44, 0xac, 0x00, 1, 2, 4, 1, 9, 0xff, 3][..],
        &[0x80, 0xbb, 0x00, 0, 1, 27, 0],
        &[0x01, 0x77, 0x01, 1, 2, 5, 8, 5, 0],
    ]
    .into_iter()
    .enumerate()
    {
        write(&resampler, &format!("pushes-{i}"), seed);
    }
    println!("seed corpora written to {}", fuzz.join("corpus").display());
}

/// A few bridge messages of different shapes.
fn bridge_messages() -> Vec<BridgeMsg> {
    let param = ParamInfo {
        id: 1,
        name: "Cutoff".into(),
        module: "Filter".into(),
        min: 20.0,
        max: 20_000.0,
        default: 1_000.0,
        step_count: 0,
        flags: 1,
    };
    vec![
        BridgeMsg::Hello {
            protocol: komponist_ipc::control::PROTOCOL_VERSION,
            pid: 4242,
        },
        BridgeMsg::Activated { latency: 64 },
        BridgeMsg::StartupState(vec![1, 2, 3]),
        BridgeMsg::State {
            request: 7,
            state: vec![9; 100],
            verified: vec![(1, 0.4)],
        },
        BridgeMsg::Params {
            params: vec![param],
            native_layout: Some(b"{}".to_vec()),
        },
        BridgeMsg::PluginParamEdits(vec![(1, 0.5)]),
        BridgeMsg::LatencyChanged(128),
        BridgeMsg::Log {
            level: LogLevel::Warning,
            message: "slow".into(),
        },
        BridgeMsg::Pong { seq: 17 },
    ]
}

/// The bytes a bridge writes into the socket for `msg`.
fn wire_of(msg: &BridgeMsg) -> Vec<u8> {
    let (app, bridge_end) = socket_pair().expect("socket pair");
    let bridge = BridgeChannel::from_fd(bridge_end);
    bridge.send(msg).expect("send");
    drop(bridge);
    let fd = app.as_fd().try_clone_to_owned().expect("dup");
    let mut socket = std::os::unix::net::UnixStream::from(fd);
    let mut wire = Vec::new();
    socket.read_to_end(&mut wire).expect("read");
    wire
}

fn wire_event_bytes(e: &WireEvent) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    for word in [e.offset, e.kind, e.note_id, e.param_id] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&e.value.to_le_bytes());
    out.extend_from_slice(&e.data);
    out.extend_from_slice(&e.reserved.to_le_bytes());
    out
}

/// A `KPV3` blob (layout in `komponist-hosting/src/vst3/stream.rs`).
fn vst3_state(component: &[u8], controller: &[u8]) -> Vec<u8> {
    let mut blob = b"KPV3".to_vec();
    blob.extend_from_slice(&1u16.to_le_bytes());
    blob.extend_from_slice(&0u16.to_le_bytes());
    blob.extend_from_slice(b"kpTestSynth00001");
    blob.extend_from_slice(&(component.len() as u64).to_le_bytes());
    blob.extend_from_slice(&(controller.len() as u64).to_le_bytes());
    blob.extend_from_slice(component);
    blob.extend_from_slice(controller);
    blob
}

fn plugin_message_seeds() -> Vec<Vec<u8>> {
    let sampler = sampler_ui::Snapshot {
        pads: vec![sampler_ui::Pad {
            status: sampler_ui::Status::Ready,
            name: "Kick".into(),
            length: 1.5,
            peaks: vec![0.5; 8],
            playhead: Some(0.25),
        }],
        last_hit: (3, 0),
        transients: vec![0.1, 0.2],
        sync: Some(sampler_ui::Sync::Ready),
        source_rate: Some(48_000),
        idle: false,
    };
    let options = waveform_markers::SliceOptions {
        gate: true,
        reverse: Some(false),
        fade_in_ms: Some(5.0),
        fade_out_ms: Some(10.0),
    };
    let markers = waveform_markers::Snapshot {
        revision: 7,
        editable: true,
        positions: vec![0.5, 1.0],
        slice_options: Some(vec![options; 3]),
        default_options: Some(options),
        preparation: None,
    };
    let assets = device_assets::Snapshot {
        slots: vec![device_assets::Slot {
            id: 1,
            role: "sample".into(),
            label: "Sample".into(),
            kind: device_assets::AssetKind::AudioSample,
            status: device_assets::AssetStatus::Ready,
        }],
    };
    let bindings = device_assets::Bindings {
        slots: vec![device_assets::Binding {
            id: 1,
            path: Some("/a.wav".into()),
            reload: true,
        }],
    };
    vec![
        postcard::to_stdvec(&sampler).unwrap(),
        postcard::to_stdvec(&markers).unwrap(),
        postcard::to_stdvec(&assets).unwrap(),
        postcard::to_stdvec(&bindings).unwrap(),
        br#"{"frames":[{"key":"env","primitives":[{"kind":"line","from":[0,0],"to":[1,1],"tone":"accent"}]}]}"#.to_vec(),
    ]
}
