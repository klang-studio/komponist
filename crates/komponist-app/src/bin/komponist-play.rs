//! `komponist-play`: the headless demo player (WP-28, gate G3).
//!
//! Plays the demo project "Night Drive" (or a `.komponist` file) through the real engine and
//! hexa, before any UI exists:
//!
//! ```text
//! komponist-play [PROJECT.komponist] [--seconds N] [--song] [--gain DB] [--buffer FRAMES]
//!                [--null] [--mute]
//! komponist-play [PROJECT.komponist] --export OUT.wav [--float] [--gain DB]
//! ```
//!
//! - **Play** (default): the default output device of the first available host (PipeWire, JACK,
//!   ALSA), from bar 1, looping the project loop if it is on, else the whole song (`--song` always
//!   loops the whole song). Prints position, CPU load, xruns and the master peak once per second;
//!   stops after `--seconds N`, on Enter, or on Ctrl-C (which ends the process). `--null` runs the
//!   null backend (no device), `--mute` opens the device but writes zeros.
//! - **Export** (`--export OUT.wav`): renders the whole song plus a 2 s tail offline with
//!   `Engine::render` at 48 kHz, without opening a device, and writes a 24-bit WAV (`--float`:
//!   32-bit float). Prints length, peak, RMS and per-track activity per bar.
//!
//! komponist hosts no plugins yet, so hexa stands in for every instrument: a hexa device with
//! a stored state plays that state; other instruments get a factory preset chosen by the track's
//! role (track name), and drum tracks (the Sampler, step clips) get [`DrumKit`], one hexa per drum
//! note. Effects get no processor, so the engine passes their input through. The table is printed
//! at start.
//!
//! The master gets a trim of [`DEFAULT_GAIN_DB`] on top of the project's master fader (`--gain`
//! overrides), so the demo peaks at about −8 dBFS. Debug builds abort on any allocation inside the
//! audio callback and the offline render (`assert_no_alloc`). Play release builds
//! (`cargo run --release -p komponist-app --bin komponist-play`): unoptimised hexa needs more
//! than the whole buffer period.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use komponist_audio::{
    AudioProcess, AudioState, AudioSystem, CallbackInfo, HostKind, MidiIn, MidiReceiver,
    OutputBuffer, StreamConfig, StreamParams,
};
use komponist_builtin::{HexaProcessor, Patch, preset_patch, presets};
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::{
    Command, Engine, EngineConfig, EngineEvent, EngineHandle, Event, EventKind, LiveInput,
    MAX_BLOCK, MAX_EVENTS_PER_BLOCK, MidiPort, ProcessContext, ProcessStatus, Processor,
    ProcessorError, Program, StereoIo,
};
use komponist_model::format::{fmt_db, fmt_pos, fmt_time, note_name};
use komponist_model::project::{BUILTIN_HEXA, BUILTIN_SAMPLER};
use komponist_model::{
    ClipContent, Device, DeviceId, PluginFormat, Project, Tick, TickRange, Track, fixtures, persist,
};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// Master trim in dB on top of the project's master fader. The demo export peaks at −8.4 dBFS with
/// it (−6.4 dBFS without).
const DEFAULT_GAIN_DB: f32 = -2.0;

/// Sample rate of the export.
const EXPORT_RATE: u32 = 48_000;

/// Silence rendered after the song end, so releases and tails decay.
const TAIL_SECONDS: f64 = 2.0;

/// Frames per `Engine::render` call (meters are read per call).
const RENDER_BLOCK: usize = 512;

/// Below this level (linear, −60 dBFS) a bar of a track counts as silent.
const ACTIVE_LEVEL: f32 = 0.001;

/// How often the play loop polls the audio system and the engine events.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Track roles recognised by words in the track name (lower case), with the hexa preset that
/// stands in for the instrument. Checked in order, after the drum check.
const ROLES: [(&[&str], &str); 5] = [
    (&["bass"], "Analog Bass"),
    (&["lead", "melod"], "Poly Synth"),
    (&["pad", "string", "streicher", "chor"], "Warm Pad"),
    (
        &["key", "piano", "klavier", "rhodes", "orgel", "organ"],
        "E-Piano",
    ),
    (&["arp"], "Arp Bleep"),
];

/// Preset for instruments of tracks without a recognised role.
const FALLBACK_PRESET: &str = "Poly Synth";

/// Words in a track name that make it a drum track.
const DRUM_WORDS: [&str; 4] = ["drum", "beat", "perc", "schlagzeug"];

fn main() {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return;
        }
        Err(e) => {
            eprintln!("komponist-play: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(&args) {
        eprintln!("komponist-play: {e}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let project = load_project(args.project.as_deref())?;
    let map = project.tempo_map();
    let end = project.song_end();
    println!(
        "komponist-play · \"{}\" · {:.2} bpm · {}/{} · song end {} ({})",
        project.name,
        map.tempo.bpm,
        map.meter.num,
        map.meter.den,
        fmt_pos(end, map.meter),
        fmt_time(map.tick_to_seconds(end)),
    );
    let plans = plan(&project);
    print_plans(&plans);
    match &args.export {
        Some(path) => export(project, &plans, args, path),
        None => play(project, &plans, args),
    }
}

/// The project in the file at `path`, or the demo project.
fn load_project(path: Option<&std::path::Path>) -> Result<Project, String> {
    let Some(path) = path else {
        return Ok(fixtures::demo_project());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    persist::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
}

// ------------------------------------------------------------------------------------------------
// Command line
// ------------------------------------------------------------------------------------------------

const USAGE: &str = "\
usage: komponist-play [PROJECT.komponist] [options]

Plays the demo project \"Night Drive\" (or PROJECT) through the engine and hexa.

play (default): default output device, from bar 1, looping the project loop (or the whole song)
  --seconds N      stop after N seconds (default: on Enter or Ctrl-C)
  --song           loop the whole song, ignoring the project loop
  --buffer FRAMES  device buffer size (default 256)
  --null           no device: drive the engine from a timer thread
  --mute           open the device but write silence
export:
  --export OUT.wav render the whole song + 2 s offline at 48 kHz, 24-bit (no device)
  --float          write 32-bit float instead of 24-bit
both:
  --gain DB        master trim in dB (default -2)";

/// Parsed command line.
#[derive(Debug, Clone, PartialEq)]
struct Args {
    project: Option<PathBuf>,
    export: Option<PathBuf>,
    float: bool,
    seconds: Option<f64>,
    whole_song: bool,
    gain_db: f32,
    buffer: u32,
    null: bool,
    mute: bool,
}

impl Args {
    /// `None` for `--help`.
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Args>, String> {
        let mut parsed = Args {
            project: None,
            export: None,
            float: false,
            seconds: None,
            whole_song: false,
            gain_db: DEFAULT_GAIN_DB,
            buffer: StreamConfig::default().buffer_frames,
            null: false,
            mute: false,
        };
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--export" => parsed.export = Some(PathBuf::from(value()?)),
                "--float" => parsed.float = true,
                "--seconds" => {
                    let s: f64 = value()?.parse().map_err(|e| format!("--seconds: {e}"))?;
                    if !(s > 0.0 && s.is_finite()) {
                        return Err("--seconds must be positive".to_owned());
                    }
                    parsed.seconds = Some(s);
                }
                "--song" => parsed.whole_song = true,
                "--gain" => {
                    let db: f32 = value()?.parse().map_err(|e| format!("--gain: {e}"))?;
                    if !db.is_finite() {
                        return Err("--gain must be finite".to_owned());
                    }
                    parsed.gain_db = db;
                }
                "--buffer" => {
                    parsed.buffer = value()?.parse().map_err(|e| format!("--buffer: {e}"))?;
                }
                "--null" => parsed.null = true,
                "--mute" => parsed.mute = true,
                flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
                path if parsed.project.is_none() => parsed.project = Some(PathBuf::from(path)),
                extra => return Err(format!("unexpected argument {extra}")),
            }
        }
        Ok(Some(parsed))
    }
}

// ------------------------------------------------------------------------------------------------
// Substitution plan
// ------------------------------------------------------------------------------------------------

/// What plays one device of the project.
#[derive(Debug, Clone)]
struct DevicePlan {
    device: DeviceId,
    /// Track name, `None` for the master.
    track: Option<String>,
    /// Plugin name and format as the project references it.
    plugin: String,
    format: PluginFormat,
    stand_in: StandIn,
}

/// The processor that stands in for a device.
#[derive(Debug, Clone)]
enum StandIn {
    /// One hexa playing `patch`; `label` says where the patch comes from.
    Synth { patch: Box<Patch>, label: String },
    /// A hexa per drum note.
    Kit(Vec<PadPlan>),
    /// No processor: the engine passes an effect's input through (an instrument would be silent).
    Bypass,
}

/// One drum note of a [`StandIn::Kit`].
#[derive(Debug, Clone, PartialEq)]
struct PadPlan {
    /// Row name ("Kick").
    name: String,
    /// The note the track plays.
    note: u8,
    /// Factory preset that plays it.
    preset: &'static str,
    /// The key the preset is played at (pitched drums sound best in their own range).
    key: u8,
    /// Hi-hat decay stretched for an open hat.
    open: bool,
}

/// The stand-in for every device of `project`, in project order (tracks, then master).
fn plan(project: &Project) -> Vec<DevicePlan> {
    let mut plans = Vec::new();
    for track in &project.tracks {
        for (index, device) in track.channel.devices.iter().enumerate() {
            let is_instrument = track
                .channel
                .instrument()
                .is_some_and(|(at, _)| at == index);
            let stand_in = if is_instrument {
                instrument_stand_in(track, device)
            } else {
                StandIn::Bypass
            };
            plans.push(DevicePlan {
                device: device.id,
                track: Some(track.name.clone()),
                plugin: device.plugin.name.clone(),
                format: device.plugin.format,
                stand_in,
            });
        }
    }
    for device in &project.master.devices {
        plans.push(DevicePlan {
            device: device.id,
            track: None,
            plugin: device.plugin.name.clone(),
            format: device.plugin.format,
            stand_in: StandIn::Bypass,
        });
    }
    plans
}

/// The stand-in for the instrument of `track`.
fn instrument_stand_in(track: &Track, device: &Device) -> StandIn {
    let plugin = &device.plugin;
    let is_hexa = plugin.format == PluginFormat::Builtin && plugin.id == BUILTIN_HEXA;
    if is_hexa && let Some(state) = device.state.as_deref() {
        match komponist_builtin::decode_state(Some(state)) {
            Ok(patch) => {
                return StandIn::Synth {
                    patch: Box::new(patch),
                    label: "its stored patch".to_owned(),
                };
            }
            Err(e) => eprintln!("warning: {}: {e}; using a preset", track.name),
        }
    }
    if is_drum_track(track) {
        return StandIn::Kit(drum_pads(track));
    }
    let name = track.name.to_lowercase();
    let preset = ROLES
        .iter()
        .find(|(words, _)| words.iter().any(|w| name.contains(w)))
        .map_or(FALLBACK_PRESET, |&(_, preset)| preset);
    StandIn::Synth {
        patch: Box::new(preset_by_name(preset)),
        label: format!("preset \"{preset}\""),
    }
}

/// A track is a drum track if it plays the sampler, has step clips, or says so in its name.
fn is_drum_track(track: &Track) -> bool {
    let sampler = track.channel.instrument().is_some_and(|(_, d)| {
        d.plugin.format == PluginFormat::Builtin && d.plugin.id == BUILTIN_SAMPLER
    });
    let steps = track
        .clips
        .iter()
        .any(|c| matches!(c.content, ClipContent::Step(_)));
    let name = track.name.to_lowercase();
    sampler || steps || DRUM_WORDS.iter().any(|w| name.contains(w))
}

/// The drum notes `track` plays, in note order: step rows with at least one step on (named by the
/// row), and notes of MIDI clips (named by their General MIDI drum).
fn drum_pads(track: &Track) -> Vec<PadPlan> {
    let mut names: Vec<(u8, String)> = Vec::new();
    let mut add = |note: u8, name: &str| {
        if !names.iter().any(|(n, _)| *n == note) {
            names.push((note, name.to_owned()));
        }
    };
    for clip in &track.clips {
        match &clip.content {
            ClipContent::Step(steps) => {
                let len = usize::from(steps.length_steps);
                for row in &steps.rows {
                    if row.steps.iter().take(len).any(|s| s.on) {
                        add(row.note, &row.name);
                    }
                }
            }
            ClipContent::Midi(midi) => {
                if let Some(take) = midi.takes.get(midi.active_take) {
                    for note in take.notes.iter() {
                        add(note.pitch, "");
                    }
                }
            }
            ClipContent::Audio(_) => {} // no drum notes
        }
    }
    names.sort_by_key(|(note, _)| *note);
    names
        .into_iter()
        .map(|(note, name)| {
            let (preset, key, open) = drum_sound(&name, note);
            PadPlan {
                name: if name.is_empty() {
                    note_name(note)
                } else {
                    name
                },
                note,
                preset,
                key,
                open,
            }
        })
        .collect()
}

/// A drum sound: factory preset, the key it is played at, open-hat flag.
type DrumSound = (&'static str, u8, bool);

/// The sound for a drum row named `name` on `note`: by the name first, then by the General MIDI
/// drum map, else a bongo.
fn drum_sound(name: &str, note: u8) -> DrumSound {
    const KICK: DrumSound = ("Bass Drum", 36, false);
    const SNARE: DrumSound = ("Snare", 55, false);
    const CLAP: DrumSound = ("Clap", 60, false);
    const OPEN_HAT: DrumSound = ("Hi-Hat", 60, true);
    const HAT: DrumSound = ("Hi-Hat", 60, false);
    const TOM: DrumSound = ("Tom", 45, false);
    const RIM: DrumSound = ("Buchla Bongo", 79, false);
    const CYMBAL: DrumSound = ("Cymbal", 60, false);
    const PERC: DrumSound = ("Buchla Bongo", 67, false);
    let name = name.to_lowercase();
    let by_name: [(&[&str], DrumSound); 8] = [
        (&["kick", "bassdrum", "bass drum"], KICK),
        (&["snare"], SNARE),
        (&["clap"], CLAP),
        (&["offen", "open"], OPEN_HAT),
        (&["hat", "hh"], HAT),
        (&["tom"], TOM),
        (&["rim"], RIM),
        (&["becken", "crash", "ride", "cymbal"], CYMBAL),
    ];
    if let Some(&(_, sound)) = by_name
        .iter()
        .find(|(words, _)| words.iter().any(|w| name.contains(w)))
    {
        return sound;
    }
    match note {
        35 | 36 => KICK,
        37 => RIM,
        38 | 40 => SNARE,
        39 => CLAP,
        42 | 44 => HAT,
        46 => OPEN_HAT,
        41 | 43 | 45 | 47 | 48 | 50 => TOM,
        49 | 51 | 52 | 53 | 55 | 57 | 59 => CYMBAL,
        _ => PERC,
    }
}

/// The patch of the factory preset named `name` ("Init" if hexa has no such preset).
fn preset_by_name(name: &str) -> Patch {
    presets()
        .find(|p| p.name == name)
        .and_then(|p| preset_patch(p.index))
        .unwrap_or_else(komponist_builtin::default_patch)
}

/// The patch of a drum pad.
fn pad_patch(pad: &PadPlan) -> Patch {
    let mut patch = preset_by_name(pad.preset);
    if pad.open {
        // An open hat: the closed hat ringing four times as long.
        for op in &mut patch.operators {
            op.envelope.decay *= 4.0;
            op.envelope.release *= 4.0;
        }
        patch = patch.sanitized();
    }
    patch
}

fn print_plans(plans: &[DevicePlan]) {
    println!("stand-ins (komponist hosts no plugins yet; hexa plays every instrument):");
    let width = plans
        .iter()
        .map(|p| p.track.as_deref().unwrap_or("Master").chars().count())
        .max()
        .unwrap_or(0);
    let mut last_track = None;
    for p in plans {
        let track = p.track.as_deref().unwrap_or("Master");
        let shown = if last_track == Some(track) { "" } else { track };
        last_track = Some(track);
        let device = format!("{} [{:?}]", p.plugin, p.format);
        let what = match &p.stand_in {
            StandIn::Synth { label, .. } => format!("Hexa, {label}"),
            StandIn::Kit(pads) => format!("drum kit, a hexa per note ({} notes)", pads.len()),
            StandIn::Bypass => "bypass (no processor, audio passes through)".to_owned(),
        };
        println!("  {shown:width$}  {device:34} → {what}");
        if let StandIn::Kit(pads) = &p.stand_in {
            for pad in pads {
                println!(
                    "  {:width$}  {:34}     {} {} → \"{}\"{} at {}",
                    "",
                    "",
                    pad.name,
                    note_name(pad.note),
                    pad.preset,
                    if pad.open { " (decay ×4)" } else { "" },
                    note_name(pad.key),
                );
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Drum kit
// ------------------------------------------------------------------------------------------------

/// One hexa per drum note, summed: a stand-in for the Sampler.
///
/// A note-on/off for a pad's note goes to that pad's hexa at the pad's key; other notes are
/// ignored. MIDI messages and `AllNotesOff` go to every pad; parameters to none. A pad without
/// voices whose last output was silent is skipped until its next event.
struct DrumKit {
    pads: Vec<Pad>,
    /// One pad's output.
    scratch: [Vec<f32>; 2],
    /// One pad's events.
    events: Vec<Event>,
}

struct Pad {
    note: u8,
    key: u8,
    synth: HexaProcessor,
    /// No voices and silent output in the last call.
    quiet: bool,
}

/// A pad counts as silent below this sample magnitude (−100 dBFS).
const PAD_SILENCE: f32 = 1e-5;

impl DrumKit {
    fn new(pads: &[PadPlan]) -> DrumKit {
        DrumKit {
            pads: pads
                .iter()
                .map(|p| Pad {
                    note: p.note,
                    key: p.key,
                    synth: HexaProcessor::new(pad_patch(p)),
                    quiet: true,
                })
                .collect(),
            scratch: [Vec::new(), Vec::new()],
            events: Vec::with_capacity(MAX_EVENTS_PER_BLOCK),
        }
    }
}

impl Processor for DrumKit {
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        for pad in &mut self.pads {
            pad.synth.activate(sample_rate, max_block)?;
            pad.quiet = true;
        }
        for buf in &mut self.scratch {
            buf.clear();
            buf.resize(max_block, 0.0);
        }
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let frames = io.frames();
        io.silence();
        let input = io.input;
        let [scratch_l, scratch_r] = &mut self.scratch;
        let (scratch_l, scratch_r) = (&mut scratch_l[..frames], &mut scratch_r[..frames]);
        for pad in &mut self.pads {
            self.events.clear();
            for event in events {
                let kind = match event.kind {
                    EventKind::NoteOn {
                        key,
                        velocity,
                        channel,
                        note_id,
                    } if key == pad.note => EventKind::NoteOn {
                        key: pad.key,
                        velocity,
                        channel,
                        note_id,
                    },
                    EventKind::NoteOff {
                        key,
                        channel,
                        note_id,
                    } if key == pad.note => EventKind::NoteOff {
                        key: pad.key,
                        channel,
                        note_id,
                    },
                    EventKind::NoteOn { .. } | EventKind::NoteOff { .. } => continue,
                    EventKind::Param { .. } => continue,
                    kind @ (EventKind::Midi(_) | EventKind::AllNotesOff) => kind,
                };
                if self.events.len() < self.events.capacity() {
                    self.events.push(Event {
                        offset: event.offset,
                        kind,
                    });
                }
            }
            if pad.quiet && self.events.is_empty() {
                continue;
            }
            let mut pad_io = StereoIo {
                input,
                output: [&mut *scratch_l, &mut *scratch_r],
            };
            pad.synth.process(ctx, &self.events, &mut pad_io);
            let mut loudest = 0.0f32;
            for (out, &s) in io.output[0].iter_mut().zip(scratch_l.iter()) {
                *out += s;
                loudest = loudest.max(s.abs());
            }
            for (out, &s) in io.output[1].iter_mut().zip(scratch_r.iter()) {
                *out += s;
                loudest = loudest.max(s.abs());
            }
            pad.quiet = pad.synth.active_voices() == 0 && loudest < PAD_SILENCE;
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        self.pads.first().map_or(0, |p| p.synth.latency())
    }

    fn reset(&mut self) {
        for pad in &mut self.pads {
            pad.synth.reset();
            pad.quiet = true;
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Engine setup
// ------------------------------------------------------------------------------------------------

/// The processor for a stand-in, `None` for a bypass.
fn processor(stand_in: &StandIn) -> Option<Box<dyn Processor>> {
    match stand_in {
        StandIn::Synth { patch, .. } => Some(Box::new(HexaProcessor::new(**patch))),
        StandIn::Kit(pads) => Some(Box::new(DrumKit::new(pads))),
        StandIn::Bypass => None,
    }
}

/// Compiler environment: no MIDI inputs, the latencies of the stand-ins.
struct Latencies(HashMap<DeviceId, u32>);

impl CompileEnv for Latencies {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.0.get(&device).copied().unwrap_or(0)
    }
}

/// Loads `project` into the engine behind `handle` (`EngineSync` in one go): creates and activates
/// the stand-ins at `sample_rate`, compiles, inserts the processors into their slots, installs the
/// program, locates to tick 0 and starts playback. Returns the program (for its meters).
fn load(
    project: &Project,
    plans: &[DevicePlan],
    handle: &mut EngineHandle,
    sample_rate: f32,
) -> Result<Program, String> {
    let mut processors = HashMap::new();
    for p in plans {
        if let Some(mut processor) = processor(&p.stand_in) {
            processor
                .activate(sample_rate, MAX_BLOCK)
                .map_err(|e| format!("{}: {e}", p.plugin))?;
            processors.insert(p.device, processor);
        }
    }
    let env = Latencies(processors.iter().map(|(&d, p)| (d, p.latency())).collect());
    let program = compile::build(project, None, &env);
    let mut send = |command| {
        handle
            .send(command)
            .map_err(|c| format!("engine command queue full at {c:?}"))
    };
    for (device, slot) in compile::slot_changes(None, &program).added {
        if let Some(processor) = processors.remove(&device) {
            send(Command::InsertProcessor { slot, processor })?;
        }
    }
    send(Command::SetProgram(Box::new(program.clone())))?;
    send(Command::Locate(Tick::ZERO))?;
    send(Command::Play)?;
    Ok(program)
}

/// `project` with the master trimmed by `gain_db`.
fn trimmed(mut project: Project, gain_db: f32) -> Project {
    Arc::make_mut(&mut project.master).volume_db += gain_db;
    project
}

/// The song end rounded up to a whole bar.
fn song_bars_end(project: &Project) -> Tick {
    let bar = project.meter.ticks_per_bar();
    Tick((project.song_end().0 + bar - 1).div_euclid(bar) * bar)
}

/// Linear peak → "−7.2 dBFS".
fn dbfs(level: f32) -> String {
    format!("{} dBFS", fmt_db(20.0 * level.max(1e-9).log10()))
}

// ------------------------------------------------------------------------------------------------
// Play
// ------------------------------------------------------------------------------------------------

/// The engine as the audio stream's [`AudioProcess`].
struct Player {
    engine: Box<Engine>,
    left: Vec<f32>,
    right: Vec<f32>,
    /// Write zeros instead of the engine's output (`--mute`).
    mute: bool,
    /// The engine's sample rate, for the main thread.
    rate: Arc<AtomicU32>,
}

impl AudioProcess for Player {
    fn prepare(&mut self, params: &StreamParams) {
        let rate = params.sample_rate as f32;
        if rate != self.engine.sample_rate()
            && let Err(errors) = self.engine.set_sample_rate(rate)
        {
            for (slot, e) in errors {
                eprintln!("slot {slot:?} failed to re-activate at {rate} Hz: {e}");
            }
        }
        self.rate.store(params.sample_rate, Ordering::Relaxed);
        let frames = params.max_frames as usize;
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
    }

    fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo, midi: &mut MidiIn<'_>) {
        while midi.pop().is_some() {}
        let frames = out.frames().min(self.left.len());
        let latency = info.output_latency.as_secs_f64() * f64::from(info.sample_rate);
        let input = LiveInput {
            midi: &[],
            output_latency: latency as u32,
        };
        let (left, right) = (&mut self.left[..frames], &mut self.right[..frames]);
        self.engine.process(left, right, &input);
        if self.mute {
            out.silence();
        } else {
            out.write_stereo(left, right);
        }
    }
}

fn play(project: Project, plans: &[DevicePlan], args: &Args) -> Result<(), String> {
    let mut project = trimmed(project, args.gain_db);
    let looped = if args.whole_song || !project.loop_on || project.loop_range.is_empty() {
        project.loop_range = TickRange::new(Tick::ZERO, song_bars_end(&project));
        project.loop_on = true;
        "whole song"
    } else {
        "project loop"
    };
    let meter = project.meter;
    let map = project.tempo_map();

    let host = if args.null {
        HostKind::Null
    } else {
        AudioSystem::<Player>::available_hosts()
            .first()
            .copied()
            .unwrap_or(HostKind::Null)
    };
    let config = StreamConfig {
        host,
        buffer_frames: args.buffer,
        ..StreamConfig::default()
    };
    let (engine, mut handle) = Engine::new(EngineConfig {
        sample_rate: config.sample_rate as f32,
        ..EngineConfig::default()
    });
    let rate = Arc::new(AtomicU32::new(config.sample_rate));
    let player = Player {
        engine,
        left: Vec::new(),
        right: Vec::new(),
        mute: args.mute,
        rate: Arc::clone(&rate),
    };
    let mut system = AudioSystem::new(Box::new(player), MidiReceiver::disconnected());
    system
        .start(&config)
        .map_err(|e| format!("cannot open the {host:?} output: {e}"))?;
    let device = match system.state() {
        AudioState::Running { config, .. } => format!(
            "{:?} {} · {} Hz · {} frames",
            config.host,
            match (config.host, config.device.as_deref()) {
                (HostKind::Null, _) => "backend, no device",
                (_, Some(name)) => name,
                (_, None) => "default device",
            },
            config.sample_rate,
            config.buffer_frames
        ),
        other => format!("{other:?}"),
    };
    // `prepare` ran in `start`: the engine has its final rate before the processors are made.
    let sample_rate = rate.load(Ordering::Relaxed) as f32;
    let program = load(&project, plans, &mut handle, sample_rate)?;
    println!(
        "output: {device}{} · master trim {} dB",
        if args.mute { " · muted" } else { "" },
        fmt_db(args.gain_db)
    );
    println!(
        "playing from {}, looping {} {} – {} · {}",
        fmt_pos(Tick::ZERO, meter),
        looped,
        fmt_pos(program.loop_range.start, meter),
        fmt_pos(program.loop_range.end, meter),
        match args.seconds {
            Some(s) => format!("stops after {s} s"),
            None => "Enter stops (Ctrl-C ends the process)".to_owned(),
        }
    );

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            // A line (Enter) stops; end of input (no terminal) does not.
            if let Some(Ok(_)) = std::io::stdin().lock().lines().next() {
                stop.store(true, Ordering::Relaxed);
            }
        });
    }

    let stats = Arc::clone(system.stats());
    let telemetry = Arc::clone(handle.telemetry());
    let started = Instant::now();
    let deadline = args.seconds.map(|s| started + Duration::from_secs_f64(s));
    let mut next_report = started + Duration::from_secs(1);
    let (mut peak, mut peak_all, mut cpu_max) = (0.0f32, 0.0f32, 0.0f32);
    let (mut wraps, mut failed) = (0u32, 0u32);
    loop {
        std::thread::sleep(POLL_INTERVAL);
        system.poll();
        while let Some(event) = handle.poll() {
            match event {
                EngineEvent::LoopWrapped { .. } => wraps += 1,
                EngineEvent::ProcessorFailed { slot } => {
                    failed += 1;
                    eprintln!("processor in slot {slot:?} failed");
                }
                // Garbage is dropped here, on this thread.
                _ => {}
            }
        }
        let (l, r) = telemetry.master_meter().take();
        peak = peak.max(l).max(r);
        let now = Instant::now();
        if now >= next_report {
            let tick = telemetry.playhead();
            let cpu = stats.cpu_load();
            cpu_max = cpu_max.max(cpu);
            peak_all = peak_all.max(peak);
            println!(
                "[{:5.0} s]  {:>9}  {}  CPU {:5.1} %  xruns {}  peak {}",
                (now - started).as_secs_f64(),
                fmt_pos(tick, meter),
                fmt_time(map.tick_to_seconds(tick)),
                cpu * 100.0,
                stats.xruns(),
                dbfs(peak),
            );
            peak = 0.0;
            next_report += Duration::from_secs(1);
        }
        if stop.load(Ordering::Relaxed) || deadline.is_some_and(|d| now >= d) {
            break;
        }
    }
    peak_all = peak_all.max(peak);
    let _ = handle.send(Command::Stop);
    std::thread::sleep(POLL_INTERVAL * 2);
    system.stop();
    while handle.poll().is_some() {}
    println!(
        "stopped after {:.1} s · {} callbacks · xruns {} · CPU max {:.1} % · peak {} · {} loop wraps{}{}",
        started.elapsed().as_secs_f64(),
        stats.callbacks(),
        stats.xruns(),
        cpu_max * 100.0,
        dbfs(peak_all),
        wraps,
        if failed > 0 {
            format!(" · {failed} processors FAILED")
        } else {
            String::new()
        },
        if stats.realtime_denied() {
            " · real-time scheduling DENIED"
        } else {
            ""
        },
    );
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Export
// ------------------------------------------------------------------------------------------------

/// What an offline render produced.
struct Render {
    /// Interleaved stereo at [`EXPORT_RATE`].
    samples: Vec<f32>,
    /// Frames of the song (without the tail).
    song_frames: usize,
    /// First frame of every bar that the render reaches (bar `i` = index `i`, bar 1 at frame 0).
    bar_starts: Vec<usize>,
    /// Per track (program order): name and peak after the fader per bar.
    tracks: Vec<(String, Vec<f32>)>,
    /// Per track: the bars its unmuted clips cover, if the track is audible.
    expected: Vec<Vec<bool>>,
}

/// Renders `project` (loop off) from tick 0 to the song end plus [`TAIL_SECONDS`]. Render calls
/// end at bar starts, so the track meters are read per bar exactly.
fn render(project: &Project, plans: &[DevicePlan]) -> Result<Render, String> {
    let mut project = project.clone();
    project.loop_on = false;
    let rate = EXPORT_RATE as f32;
    let (mut engine, mut handle) = Engine::new(EngineConfig {
        sample_rate: rate,
        ..EngineConfig::default()
    });
    let program = load(&project, plans, &mut handle, rate)?;
    let map = project.tempo_map();
    let frame_of = |tick: Tick| map.tick_to_sample(tick, f64::from(rate)).max(0) as usize;
    let song_frames = frame_of(project.song_end());
    let total = song_frames + (TAIL_SECONDS * f64::from(rate)) as usize;
    let per_bar = project.meter.ticks_per_bar();
    let bar_starts: Vec<usize> = (0..)
        .map(|bar| frame_of(Tick(bar * per_bar)))
        .take_while(|&frame| frame < total)
        .collect();
    let bars = bar_starts.len();

    let mut meters: Vec<_> = program
        .tracks
        .iter()
        .map(|t| (Arc::clone(&t.meter), vec![0.0f32; bars]))
        .collect();
    let mut samples = Vec::with_capacity(2 * total);
    let (mut done, mut bar) = (0, 0);
    while done < total {
        let next_bar = bar_starts.get(bar + 1).copied().unwrap_or(total);
        let n = RENDER_BLOCK.min(next_bar - done);
        let out = no_alloc(|| engine.render(n));
        samples.extend_from_slice(out);
        for (meter, peaks) in &mut meters {
            let (l, r) = meter.take();
            peaks[bar] = peaks[bar].max(l).max(r);
        }
        while handle.poll().is_some() {}
        done += n;
        if done == next_bar {
            bar += 1;
        }
    }
    let any_solo = project.any_solo();
    let expected = program
        .tracks
        .iter()
        .map(|t| {
            let track = &t.source;
            let audible = !track.channel.mute && (!any_solo || track.channel.solo);
            let mut on = vec![false; bars];
            for clip in track.clips.iter().filter(|c| audible && !c.mute) {
                let first = clip.start.0.div_euclid(per_bar).max(0) as usize;
                let last = (clip.end().0 + per_bar - 1).div_euclid(per_bar).max(0) as usize;
                for bar in on.iter_mut().take(last).skip(first) {
                    *bar = true;
                }
            }
            on
        })
        .collect();
    Ok(Render {
        samples,
        song_frames,
        bar_starts,
        tracks: program
            .tracks
            .iter()
            .zip(meters)
            .map(|(t, (_, peaks))| (t.source.name.clone(), peaks))
            .collect(),
        expected,
    })
}

/// Runs `f` inside `assert_no_alloc` in debug builds (the global allocator aborts on allocation).
fn no_alloc<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(debug_assertions)]
    {
        assert_no_alloc::assert_no_alloc(f)
    }
    #[cfg(not(debug_assertions))]
    {
        f()
    }
}

fn export(
    project: Project,
    plans: &[DevicePlan],
    args: &Args,
    path: &std::path::Path,
) -> Result<(), String> {
    let project = trimmed(project, args.gain_db);
    let started = Instant::now();
    let render = render(&project, plans)?;
    let elapsed = started.elapsed();
    write_wav(path, &render.samples, args.float)?;
    let frames = render.samples.len() / 2;
    let seconds = frames as f64 / f64::from(EXPORT_RATE);
    println!(
        "exported {} · {} Hz · {} · {} frames = {} (song {} + tail {} s) · rendered in {:.1} s ({:.0}× real time) · master trim {} dB",
        path.display(),
        EXPORT_RATE,
        if args.float { "32-bit float" } else { "24-bit" },
        frames,
        fmt_time(seconds),
        fmt_time(render.song_frames as f64 / f64::from(EXPORT_RATE)),
        TAIL_SECONDS,
        elapsed.as_secs_f64(),
        seconds / elapsed.as_secs_f64(),
        fmt_db(args.gain_db),
    );
    print_analysis(&render);
    Ok(())
}

fn write_wav(path: &std::path::Path, samples: &[f32], float: bool) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: EXPORT_RATE,
        bits_per_sample: if float { 32 } else { 24 },
        sample_format: if float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let error = |e: hound::Error| format!("cannot write {}: {e}", path.display());
    let mut writer = hound::WavWriter::create(path, spec).map_err(error)?;
    for &s in samples {
        if float {
            writer.write_sample(s).map_err(error)?;
        } else {
            let max = f64::from((1 << 23) - 1);
            let value = (f64::from(s.clamp(-1.0, 1.0)) * max).round() as i32;
            writer.write_sample(value).map_err(error)?;
        }
    }
    writer.finalize().map_err(error)
}

/// Peak and RMS (linear) of interleaved stereo `samples`.
fn levels(samples: &[f32]) -> (f32, f32, f32) {
    let (mut peak_l, mut peak_r, mut sum) = (0.0f32, 0.0f32, 0.0f64);
    for frame in samples.as_chunks::<2>().0 {
        peak_l = peak_l.max(frame[0].abs());
        peak_r = peak_r.max(frame[1].abs());
        sum += f64::from(frame[0]).powi(2) + f64::from(frame[1]).powi(2);
    }
    let rms = (sum / samples.len().max(1) as f64).sqrt() as f32;
    (peak_l, peak_r, rms)
}

fn print_analysis(render: &Render) {
    let song = &render.samples[..2 * render.song_frames];
    let (peak_l, peak_r, _) = levels(&render.samples);
    let (_, _, rms) = levels(song);
    println!(
        "mix: peak {} (L {}, R {}) · RMS over the song {}",
        dbfs(peak_l.max(peak_r)),
        dbfs(peak_l),
        dbfs(peak_r),
        dbfs(rms)
    );
    // Silent stretches inside the song, in 100 ms windows below −60 dBFS.
    let window = (EXPORT_RATE / 10) as usize * 2;
    let silent: Vec<usize> = song
        .chunks(window)
        .enumerate()
        .filter(|(_, w)| w.iter().all(|s| s.abs() < ACTIVE_LEVEL))
        .map(|(i, _)| i)
        .collect();
    println!(
        "silent 100 ms windows inside the song (< −60 dBFS): {}{}",
        silent.len(),
        match silent.first() {
            Some(&i) => format!(" (first at {})", fmt_time(i as f64 / 10.0)),
            None => String::new(),
        }
    );
    let bars = render.bar_starts.len();
    let rms_bars: Vec<String> = (0..bars)
        .map(|bar| {
            let from = 2 * render.bar_starts[bar];
            let to = render
                .bar_starts
                .get(bar + 1)
                .map_or(render.samples.len(), |&f| 2 * f);
            let (_, _, rms) = levels(&render.samples[from..to]);
            format!("{:.0}", 20.0 * rms.max(1e-9).log10())
        })
        .collect();
    println!("mix RMS per bar (dBFS): {}", rms_bars.join(" "));
    println!(
        "tracks, peak after the fader per bar 1–{bars} ('#' ≥ −20 dBFS, '+' ≥ −40, '.' ≥ −60, \
         ' ' silent; '!' = a clip there but silent):"
    );
    let width = render
        .tracks
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for ((name, peaks), expected) in render.tracks.iter().zip(&render.expected) {
        let peak = peaks.iter().copied().fold(0.0f32, f32::max);
        let map: String = peaks
            .iter()
            .zip(expected)
            .map(|(&p, &clip)| match p {
                p if p >= 0.1 => '#',
                p if p >= 0.01 => '+',
                p if p >= ACTIVE_LEVEL => '.',
                _ if clip => '!',
                _ => ' ',
            })
            .collect();
        let missing = peaks
            .iter()
            .zip(expected)
            .filter(|&(&p, &clip)| clip && p < ACTIVE_LEVEL)
            .count();
        println!(
            "  {name:width$}  peak {:>12}  |{map}|{}",
            dbfs(peak),
            if missing > 0 {
                format!("  {missing} bars with clips SILENT")
            } else {
                String::new()
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    fn args(list: &[&str]) -> Result<Option<Args>, String> {
        Args::parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_the_command_line() {
        let a = args(&[]).unwrap().unwrap();
        assert_eq!((a.project, a.export, a.seconds), (None, None, None));
        assert_eq!((a.gain_db, a.buffer), (DEFAULT_GAIN_DB, 256));
        let a = args(&[
            "x.komponist",
            "--export",
            "o.wav",
            "--float",
            "--gain",
            "-3",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.project, Some(PathBuf::from("x.komponist")));
        assert_eq!(a.export, Some(PathBuf::from("o.wav")));
        assert!(a.float);
        assert_eq!(a.gain_db, -3.0);
        let a = args(&["--seconds", "3", "--null", "--mute", "--song"])
            .unwrap()
            .unwrap();
        assert_eq!(a.seconds, Some(3.0));
        assert!(a.null && a.mute && a.whole_song);
        assert_eq!(args(&["--help"]), Ok(None));
        assert!(args(&["--seconds"]).is_err());
        assert!(args(&["--seconds", "0"]).is_err());
        assert!(args(&["--loud"]).is_err());
        assert!(args(&["a", "b"]).is_err());
    }

    #[test]
    fn loads_the_demo_or_a_project_file() {
        assert_eq!(load_project(None), Ok(demo_project()));
        let dir = std::env::temp_dir().join(format!("komponist-play-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.komponist");
        let mut project = demo_project();
        project.name = "Probe".to_owned();
        std::fs::write(&path, persist::to_json(&project).unwrap()).unwrap();
        assert_eq!(load_project(Some(&path)), Ok(project));
        std::fs::write(&path, "{}").unwrap();
        assert!(
            load_project(Some(&path))
                .unwrap_err()
                .contains("demo.komponist")
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            load_project(Some(&path))
                .unwrap_err()
                .starts_with("cannot read")
        );
    }

    #[test]
    fn every_demo_instrument_gets_a_stand_in() {
        let plans = plan(&demo_project());
        let summary: Vec<(&str, &str, String)> = plans
            .iter()
            .map(|p| {
                let what = match &p.stand_in {
                    StandIn::Synth { label, .. } => label.clone(),
                    StandIn::Kit(pads) => format!("kit of {}", pads.len()),
                    StandIn::Bypass => "bypass".to_owned(),
                };
                (p.track.as_deref().unwrap(), p.plugin.as_str(), what)
            })
            .collect();
        let expected = [
            ("Drums", "Klopfer", "kit of 6"),
            ("Drums", "LSP Compressor", "bypass"),
            ("Bass", "Hexa", "preset \"Analog Bass\""),
            ("Bass", "LSP Para EQ x8", "bypass"),
            ("Lead", "Hexa", "preset \"Poly Synth\""),
            ("Lead", "LSP Para EQ x8", "bypass"),
            ("Lead", "TAL-Chorus-LX", "bypass"),
            ("Lead", "Dragonfly Hall Reverb", "bypass"),
            ("Pad", "Surge XT", "preset \"Warm Pad\""),
            ("Pad", "LSP Para EQ x8", "bypass"),
            ("Pad", "Dragonfly Hall Reverb", "bypass"),
            ("Keys", "Dexed", "preset \"E-Piano\""),
            ("Arp", "Vital", "preset \"Arp Bleep\""),
        ];
        let expected: Vec<(&str, &str, String)> = expected
            .iter()
            .map(|&(t, d, w)| (t, d, w.to_owned()))
            .collect();
        assert_eq!(summary, expected);
        // Every named preset exists in hexa's bank.
        let names: Vec<&str> = presets().map(|p| p.name).collect();
        for (_, preset) in ROLES {
            assert!(names.contains(&preset), "{preset}");
        }
        assert!(names.contains(&FALLBACK_PRESET));
    }

    #[test]
    fn the_demo_beat_maps_rows_to_drum_presets() {
        let project = demo_project();
        let pads = drum_pads(&project.tracks[0]);
        let pads: Vec<(&str, u8, &str, u8, bool)> = pads
            .iter()
            .map(|p| (p.name.as_str(), p.note, p.preset, p.key, p.open))
            .collect();
        assert_eq!(
            pads,
            [
                ("Kick", 36, "Bass Drum", 36, false),
                ("Snare", 37, "Snare", 55, false),
                ("Clap", 38, "Clap", 60, false),
                ("Closed Hat", 39, "Hi-Hat", 60, false),
                ("Open Hat", 40, "Hi-Hat", 60, true),
                ("Tom", 41, "Tom", 45, false),
            ]
        );
        let names: Vec<&str> = presets().map(|p| p.name).collect();
        for note in 30..60 {
            let (preset, _, _) = drum_sound("", note);
            assert!(names.contains(&preset), "{preset}");
        }
        // General MIDI notes without names.
        assert_eq!(drum_sound("", 38).0, "Snare");
        assert_eq!(drum_sound("", 46), ("Hi-Hat", 60, true));
    }

    #[test]
    fn a_stored_hexa_state_wins_over_the_role_preset() {
        let mut project = demo_project();
        let patch = preset_patch(1).unwrap();
        let bass = Arc::make_mut(&mut project.tracks[1]);
        Arc::make_mut(&mut bass.channel.devices[0]).state =
            Some(komponist_builtin::encode_state(&patch).into());
        let plans = plan(&project);
        let bass = plans
            .iter()
            .find(|p| p.track.as_deref() == Some("Bass"))
            .unwrap();
        match &bass.stand_in {
            StandIn::Synth { patch: p, label } => {
                assert_eq!(**p, patch.sanitized());
                assert_eq!(label, "its stored patch");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Runs `kit` for one call of `frames` with `events`; returns the left and right output.
    fn run_kit(kit: &mut DrumKit, frames: usize, events: &[Event]) -> (Vec<f32>, Vec<f32>) {
        let silence = vec![0.0f32; frames];
        let (mut l, mut r) = (vec![f32::NAN; frames], vec![f32::NAN; frames]);
        let ctx = ProcessContext {
            frames: frames as u32,
            sample_rate: 48_000.0,
            transport: komponist_engine::TransportInfo {
                playing: true,
                recording: false,
                looping: false,
                tempo_bpm: 120.0,
                meter: komponist_model::Meter::FOUR_FOUR,
                position_ticks: 0.0,
                loop_range: TickRange::default(),
            },
            offline: true,
            deadline: None,
        };
        let mut io = StereoIo {
            input: [&silence, &silence],
            output: [&mut l, &mut r],
        };
        assert_eq!(
            no_alloc(|| kit.process(&ctx, events, &mut io)),
            ProcessStatus::Continue
        );
        (l, r)
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn the_drum_kit_plays_its_notes_and_ignores_others() {
        let pads = drum_pads(&demo_project().tracks[0]);
        let mut kit = DrumKit::new(&pads);
        kit.activate(48_000.0, 1024).unwrap();
        assert_eq!(kit.latency(), HexaProcessor::default().latency());
        // Silence without events, every output frame written.
        let (l, r) = run_kit(&mut kit, 512, &[]);
        assert_eq!((peak(&l), peak(&r)), (0.0, 0.0));
        // A note no pad plays stays silent.
        let on = |key| Event {
            offset: 0,
            kind: EventKind::NoteOn {
                key,
                velocity: 120,
                channel: 0,
                note_id: 1,
            },
        };
        let (l, _) = run_kit(&mut kit, 512, &[on(70)]);
        assert_eq!(peak(&l), 0.0);
        // The kick sounds, and keeps sounding in the next call without events.
        let (l, r) = run_kit(&mut kit, 512, &[on(36)]);
        assert!(peak(&l) > 0.01 && peak(&r) > 0.01, "{}", peak(&l));
        assert!(kit.pads[0].synth.active_voices() > 0);
        let (l, _) = run_kit(&mut kit, 512, &[]);
        assert!(peak(&l) > 0.001);
        // Reset silences at once.
        kit.reset();
        let (l, _) = run_kit(&mut kit, 512, &[]);
        assert_eq!(peak(&l), 0.0);
        assert!(kit.pads.iter().all(|p| p.quiet));
    }

    #[test]
    fn every_pad_of_the_demo_kit_sounds() {
        let pads = drum_pads(&demo_project().tracks[0]);
        let mut kit = DrumKit::new(&pads);
        kit.activate(48_000.0, 512).unwrap();
        for pad in &pads {
            kit.reset();
            let hit = Event {
                offset: 0,
                kind: EventKind::NoteOn {
                    key: pad.note,
                    velocity: 100,
                    channel: 0,
                    note_id: 1,
                },
            };
            let mut level = 0.0f32;
            for call in 0..47 {
                let events = if call == 0 { &[hit][..] } else { &[] };
                let (l, r) = run_kit(&mut kit, 512, events);
                level = level.max(peak(&l)).max(peak(&r));
            }
            println!("{} → {}: peak {}", pad.name, pad.preset, dbfs(level));
            assert!((0.01..1.0).contains(&level), "{}: {level}", pad.name);
        }
    }

    #[test]
    fn the_open_hat_rings_longer_than_the_closed_one() {
        let closed = PadPlan {
            name: "Closed Hat".to_owned(),
            note: 42,
            preset: "Hi-Hat",
            key: 60,
            open: false,
        };
        let open = PadPlan {
            open: true,
            note: 46,
            ..closed.clone()
        };
        let (c, o) = (pad_patch(&closed), pad_patch(&open));
        assert!(o.operators[0].envelope.decay > 3.9 * c.operators[0].envelope.decay);
    }

    #[test]
    fn mute_writes_zeros_while_the_engine_runs() {
        let (engine, mut handle) = Engine::new(EngineConfig::default());
        let project = demo_project();
        let plans = plan(&project);
        load(&project, &plans, &mut handle, 48_000.0).unwrap();
        let mut player = Player {
            engine,
            left: Vec::new(),
            right: Vec::new(),
            mute: true,
            rate: Arc::new(AtomicU32::new(0)),
        };
        player.prepare(&StreamParams {
            sample_rate: 48_000,
            max_frames: 256,
            channels: 2,
            workgroup: None,
        });
        assert_eq!(player.rate.load(Ordering::Relaxed), 48_000);
        let mut data = vec![1.0f32; 512];
        let mut receiver = MidiReceiver::disconnected();
        let info = CallbackInfo {
            sample_rate: 48_000,
            frames: 256,
            now: Instant::now(),
            output_latency: Duration::ZERO,
        };
        for _ in 0..40 {
            let mut out = OutputBuffer::new(&mut data, 2);
            let mut midi = MidiIn::new(&mut receiver, info);
            no_alloc(|| player.process(&mut out, &info, &mut midi));
        }
        assert!(data.iter().all(|&s| s == 0.0));
        // The engine played: the playhead moved and the master meter saw the drums.
        assert!(handle.telemetry().playhead() > Tick(400));
        assert!(handle.telemetry().master_meter().take().0 > 0.0);
        while handle.poll().is_some() {}
    }

    #[test]
    fn the_first_bar_of_the_demo_renders_with_sound_and_headroom() {
        let mut project = trimmed(demo_project(), DEFAULT_GAIN_DB);
        // Bar 1 only: drums, bass and pad.
        for track in &mut project.tracks {
            let track = Arc::make_mut(track);
            track.clips.retain(|c| c.start == Tick::ZERO);
            for clip in &mut track.clips {
                Arc::make_mut(clip).length = Tick(project.meter.ticks_per_bar());
            }
        }
        let plans = plan(&project);
        let render = render(&project, &plans).unwrap();
        let bar = project
            .tempo_map()
            .tick_to_sample(Tick(3840), f64::from(EXPORT_RATE));
        assert_eq!(render.song_frames as i64, bar);
        assert_eq!(
            render.samples.len(),
            2 * (render.song_frames + (TAIL_SECONDS * f64::from(EXPORT_RATE)) as usize)
        );
        let (peak_l, peak_r, rms) = levels(&render.samples[..2 * render.song_frames]);
        assert!(rms > 0.01, "rms {rms}");
        assert!(peak_l.max(peak_r) < 0.5, "peak {peak_l} {peak_r}");
        let active: Vec<&str> = render
            .tracks
            .iter()
            .filter(|(_, peaks)| peaks[0] >= ACTIVE_LEVEL)
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(active, ["Drums", "Bass", "Pad"]);
    }
}
