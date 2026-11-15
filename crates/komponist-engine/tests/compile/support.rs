//! Project builders, a test environment and the golden dump format.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use komponist_engine::MidiPort;
use komponist_engine::compile::CompileEnv;
use komponist_engine::program::{ChainSlot, PROBABILITY_FOLLOW, Program, TrackProgram};
use komponist_model::format::{fmt_pos, note_name};
use komponist_model::project::{MAX_STEPS, TRACK_HEIGHT_DEFAULT};
use komponist_model::{
    Channel, Clip, ClipContent, ClipId, Device, DeviceId, DeviceRole, Hue, Meter, MidiChannelSel,
    MidiClip, MidiInputSel, Note, NoteId, PluginFormat, PluginRef, Project, Step, StepClip,
    StepResolution, StepRow, Take, Tick, Track, TrackId, TrackKind,
};

// ------------------------------------------------------------------------------------------------
// Builders
// ------------------------------------------------------------------------------------------------

/// A project named "Test" (120 bpm, 4/4, loop off) with these tracks.
pub fn project(tracks: Vec<Track>) -> Project {
    let mut p = Project::new("Test");
    p.tracks = tracks.into_iter().map(Arc::new).collect();
    p.next_id = 10_000;
    p
}

/// A MIDI track without devices at 0 dB.
pub fn track(id: u64, name: &str, clips: Vec<Clip>) -> Track {
    Track {
        id: TrackId(id),
        name: name.to_owned(),
        hue: Hue::GREEN,
        kind: TrackKind::Midi,
        channel: Channel::default(),
        arm: false,
        monitor: false,
        input: MidiInputSel::All,
        midi_channel: MidiChannelSel::Omni,
        height: TRACK_HEIGHT_DEFAULT,
        clips: clips.into_iter().map(Arc::new).collect(),
        output: komponist_model::TrackOutput::Master,
        sends: Vec::new(),
        folder: false,
        folded: false,
    }
}

/// A device with a builtin plugin reference named `name`.
pub fn device(id: u64, role: DeviceRole, name: &str) -> Arc<Device> {
    Arc::new(Device {
        id: DeviceId(id),
        role,
        plugin: PluginRef {
            format: PluginFormat::Builtin,
            id: name.to_lowercase(),
            name: name.to_owned(),
            vendor: "Test".to_owned(),
            version: "1".to_owned(),
            module: Box::default(),
        },
        bypass: false,
        state: None,
        sidechain: None,
    })
}

/// A clip at `start` of `length` ticks, not looped, not muted, no transposition.
pub fn clip(id: u64, start: i64, length: i64, content: ClipContent) -> Clip {
    Clip {
        id: ClipId(id),
        name: format!("Clip {id}"),
        start: Tick(start),
        length: Tick(length),
        loop_length: None,
        mute: false,
        transpose: 0,
        velocity_pct: 0,
        content,
    }
}

/// A take of `(start, length, pitch, velocity)` notes (clip-local ticks), ids from `first_id`.
pub fn take(name: &str, first_id: u64, notes: &[(i64, i64, u8, u8)]) -> Take {
    let notes = notes
        .iter()
        .zip(first_id..)
        .map(|(&(start, length, pitch, velocity), id)| Note {
            id: NoteId(id),
            start: Tick(start),
            length: Tick(length),
            pitch,
            velocity,
        })
        .collect();
    Take {
        name: name.to_owned(),
        notes: Arc::new(notes),
    }
}

/// MIDI content with one take.
pub fn midi(notes: &[(i64, i64, u8, u8)]) -> ClipContent {
    ClipContent::Midi(MidiClip {
        takes: vec![take("", 5_000, notes)],
        active_take: 0,
    })
}

/// A step pattern with these `(name, note)` rows, all steps off, no swing.
pub fn pattern(rows: &[(&str, u8)], length_steps: u8, resolution: StepResolution) -> StepClip {
    StepClip {
        rows: rows
            .iter()
            .map(|&(name, note)| StepRow {
                name: name.to_owned(),
                note,
                mute: false,
                solo: false,
                steps: [Step::default(); MAX_STEPS],
            })
            .collect(),
        length_steps,
        resolution,
        swing: 0.0,
    }
}

/// Turns step `step` of row `row` on and returns it for further changes.
pub fn on(pattern: &mut StepClip, row: usize, step: usize) -> &mut Step {
    let s = &mut pattern.rows[row].steps[step];
    s.on = true;
    s
}

// ------------------------------------------------------------------------------------------------
// Environment
// ------------------------------------------------------------------------------------------------

/// A compile environment with fixed MIDI ports and device latencies.
#[derive(Debug, Clone, Default)]
pub struct TestEnv {
    /// `(device name, port)`.
    pub ports: Vec<(String, MidiPort)>,
    /// `(device, latency in frames)`; others have 0.
    pub latencies: Vec<(DeviceId, u32)>,
}

impl CompileEnv for TestEnv {
    fn midi_port(&self, device_name: &str) -> Option<MidiPort> {
        self.ports
            .iter()
            .find(|(name, _)| name == device_name)
            .map(|&(_, port)| port)
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.latencies
            .iter()
            .find(|(d, _)| *d == device)
            .map_or(0, |&(_, latency)| latency)
    }
}

// ------------------------------------------------------------------------------------------------
// Golden dump
// ------------------------------------------------------------------------------------------------

/// A readable dump of `program`; device names are looked up in `project`.
pub fn dump(title: &str, project: &Project, program: &Program) -> String {
    let meter = program.tempo_map.meter;
    let mut out = String::new();
    let w = &mut out;
    writeln!(w, "# {title}").unwrap();
    writeln!(
        w,
        "# Golden fixture of compile::build (WP-24). Regenerate with\n\
         # UPDATE_GOLDEN=1 cargo test -p komponist-engine --test compile"
    )
    .unwrap();
    writeln!(w).unwrap();
    writeln!(
        w,
        "tempo {:.2} bpm, meter {}/{}",
        program.tempo_map.tempo.bpm, meter.num, meter.den
    )
    .unwrap();
    writeln!(
        w,
        "loop {}, {} .. {} (ticks {} .. {})",
        on_off(program.loop_on),
        fmt_pos(program.loop_range.start, meter),
        fmt_pos(program.loop_range.end, meter),
        program.loop_range.start.0,
        program.loop_range.end.0
    )
    .unwrap();
    writeln!(
        w,
        "metronome {}, count-in {} bar(s)",
        on_off(program.metronome),
        program.count_in_bars
    )
    .unwrap();
    writeln!(
        w,
        "song end {} (tick {})",
        fmt_pos(program.song_end, meter),
        program.song_end.0
    )
    .unwrap();
    let master = &program.master;
    writeln!(
        w,
        "master: gain {:.4}, pan {:+.2}, mute {}",
        master.gain,
        master.pan,
        on_off(master.mute)
    )
    .unwrap();
    dump_chain(w, project, &master.chain);
    let slots = program.slots.entries();
    writeln!(
        w,
        "slots: {}",
        if slots.is_empty() {
            "none".to_owned()
        } else {
            slots
                .iter()
                .map(|(d, s)| format!("dev {} -> {}", d.0, s.0))
                .collect::<Vec<_>>()
                .join(", ")
        }
    )
    .unwrap();
    writeln!(w, "tracks: {}", program.tracks.len()).unwrap();
    for t in &program.tracks {
        dump_track(w, project, t, meter);
    }
    out
}

fn dump_track(w: &mut String, project: &Project, t: &TrackProgram, meter: Meter) {
    writeln!(w).unwrap();
    writeln!(
        w,
        "track {} \"{}\": gain {:.4}, pan {:+.2}, mute {}, solo {}, arm {}, monitor {}",
        t.id.0,
        t.source.name,
        t.gain,
        t.pan,
        on_off(t.mute),
        on_off(t.solo),
        on_off(t.arm),
        on_off(t.monitor)
    )
    .unwrap();
    let port = match (t.input.port, t.input.port_missing) {
        (_, true) => "missing port (receives nothing)".to_owned(),
        (Some(p), false) => format!("port {}", p.0),
        (None, false) => "all ports".to_owned(),
    };
    let channel = t
        .input
        .channel
        .map_or("omni".to_owned(), |c| format!("channel {c}"));
    writeln!(w, "  input: {port}, {channel}; pdc delay {}", t.pdc_delay).unwrap();
    dump_chain(w, project, &t.chain);
    writeln!(w, "  notes: {}", t.notes.len()).unwrap();
    if t.notes.is_empty() {
        return;
    }
    writeln!(
        w,
        "    {:<9} {:>6} {:>6} {:>8} {:>4} {:>6} {:>3}",
        "pos", "tick", "length", "key", "vel", "prob", "ch"
    )
    .unwrap();
    for n in t.notes.iter() {
        let prob = if n.probability == PROBABILITY_FOLLOW {
            "follow".to_owned()
        } else {
            n.probability.to_string()
        };
        writeln!(
            w,
            "    {:<9} {:>6} {:>6} {:>3} {:<4} {:>4} {:>6} {:>3}",
            fmt_pos(n.start, meter),
            n.start.0,
            n.length.0,
            n.key,
            note_name(n.key),
            n.velocity,
            prob,
            n.channel
        )
        .unwrap();
    }
}

fn dump_chain(w: &mut String, project: &Project, chain: &[ChainSlot]) {
    if chain.is_empty() {
        writeln!(w, "  chain: empty").unwrap();
        return;
    }
    writeln!(w, "  chain:").unwrap();
    for (index, s) in chain.iter().enumerate() {
        let name = project
            .find_device(s.device)
            .map_or("?".to_owned(), |(_, _, d)| d.plugin.name.clone());
        let role = match s.role {
            DeviceRole::Instrument => "instrument",
            DeviceRole::Effect => "effect",
            DeviceRole::MidiEffect => "midi effect",
        };
        let bypass = if s.bypass { ", bypass" } else { "" };
        writeln!(
            w,
            "    [{index}] dev {} \"{name}\" {role}{bypass} -> slot {}",
            s.device.0, s.slot.0
        )
        .unwrap();
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// Compares `actual` with `tests/golden/<name>.txt`, or writes it when `UPDATE_GOLDEN` is set.
pub fn check_golden(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; generate it with UPDATE_GOLDEN=1 and review it",
            path.display()
        )
    });
    if expected == actual {
        return;
    }
    let (line, (want, got)) = expected
        .lines()
        .chain(std::iter::repeat("<end of file>"))
        .zip(actual.lines().chain(std::iter::repeat("<end of output>")))
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .expect("texts differ, so some line differs");
    panic!(
        "{} differs from the compiled program at line {}:\n  golden:   {want}\n  compiled: {got}\n\
         Run with UPDATE_GOLDEN=1 to accept the new output (then review the diff).",
        path.display(),
        line + 1
    );
}
