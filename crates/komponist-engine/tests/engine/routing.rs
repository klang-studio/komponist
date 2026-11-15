//! The routing graph in the engine (WP-102, ARCHITECTURE §6.8.4–6.8.7): bus sums, sends, nested
//! groups, sidechain inputs, delay compensation across the graph, mute and solo along the signal
//! path, bit-exact output with any number of workers, allocation-free program swaps, offline
//! render, and projects without buses unchanged.
//!
//! Projects are built with the model's routing edits and compiled with `compile::build`; sources
//! are constant (`Dc`), single-sample (`Pulse`) or noise instruments, so levels and arrival times
//! can be read off the output. Every `process` / `render` call runs inside `assert_no_alloc`.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use klang_dsp::units::db_to_gain;
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::testing::{TestGain, TestSidechain, TestSidechainMode, TestSine};
use komponist_engine::{
    Command, Event, MidiPort, ProcessContext, ProcessStatus, Processor, ProcessorError, Program,
    StereoIo,
};
use komponist_model::edits::{self, ChannelProp, NewBus, NewDevice, NewTrack, SendProp, TrackProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{
    ChannelRef, DeviceId, DeviceRole, PluginFormat, PluginRef, Project, SendId, Sidechain, Tap,
    Tick, TrackId, TrackOutput,
};

use crate::common::*;

/// Latencies of the effect devices of a project, as a compile environment.
struct Latencies(HashMap<DeviceId, u32>);

impl CompileEnv for Latencies {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.0.get(&device).copied().unwrap_or(0)
    }
}

/// Compiles `project` with a latency of `(id % 4) × 37` frames on every effect and inserts a
/// `TestSine` per instrument and a `TestGain` (0.8, that latency) per effect.
fn load(rig: &mut Rig, project: &Project) -> Program {
    let mut latencies = HashMap::new();
    let devices = project
        .tracks
        .iter()
        .flat_map(|t| &t.channel.devices)
        .chain(&project.master.devices);
    for device in devices {
        if device.role == DeviceRole::Effect {
            latencies.insert(device.id, (device.id.0 % 4) as u32 * 37);
        }
    }
    let env = Latencies(latencies);
    let program = compile::build(project, None, &env);
    for (device, slot) in compile::slot_changes(None, &program).added {
        let (_, _, model) = project.find_device(device).unwrap();
        match model.role {
            DeviceRole::Instrument => rig.insert(slot.0, TestSine::new()),
            DeviceRole::Effect => rig.insert(
                slot.0,
                TestGain::with_latency(0.8, env.device_latency(device)),
            ),
            DeviceRole::MidiEffect => {}
        }
    }
    rig.set_program(&program);
    program
}

/// FNV-1a over the bits of both channels.
fn hash(l: &[f32], r: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for x in l.iter().chain(r) {
        for b in x.to_bits().to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The demo project (no buses) with test processors and latencies: played with fast-path solo
/// and mute changes, then rendered offline; `workers` worker threads.
fn demo_render(workers: usize) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::new(RATE, 256);
    rig.engine.set_workers(workers, None);
    let project = demo_project();
    let program = load(&mut rig, &project);
    rig.send(Command::Locate(program.loop_range.start));
    rig.send(Command::Play);
    let ids: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
    let blocks = [256, 256, 100, 512, 37, 2048];
    for call in 0..60 {
        rig.process(blocks[call % blocks.len()], 256);
        match call {
            10 => rig.send(Command::SetTrackSolo {
                track: ids[2],
                solo: true,
            }),
            20 => rig.send(Command::SetTrackMute {
                track: ids[2],
                mute: true,
            }),
            30 => rig.send(Command::SetTrackSolo {
                track: ids[2],
                solo: false,
            }),
            40 => rig.set_program(&program),
            _ => {}
        }
    }
    rig.send(Command::Locate(Tick(program.loop_range.start.0 + BAR)));
    rig.render(48_000, 1000);
    (rig.l, rig.r)
}

/// Golden: a project without buses renders bit-identically to the engine before WP-102 (the
/// hash was taken from a render of the base commit `56a0a27`).
#[test]
fn projects_without_buses_render_as_before() {
    const BEFORE: u64 = 3_480_190_116_315_408_716;
    for workers in [0, 3] {
        let (l, r) = demo_render(workers);
        assert!(l.iter().any(|x| x.abs() > 0.01), "silent");
        assert_eq!(hash(&l, &r), BEFORE, "{workers} workers");
    }
}

// ------------------------------------------------------------------------------------------------
// Sources and devices
// ------------------------------------------------------------------------------------------------

/// What a device of a test project is.
#[derive(Debug, Clone, Copy)]
enum Dev {
    /// Instrument: a constant on both channels.
    Dc(f32),
    /// Instrument: `level` at frame `at` of its own calls, else 0.
    Pulse(u64, f32),
    /// Instrument: deterministic noise with this seed (left and right differ).
    Noise(u32),
    /// Instrument: noise that costs about as much as a synth voice per sample (`k` rounds of
    /// work per sample), for the timing bench.
    Busy(u32),
    /// Effect: gain and latency (`TestGain`).
    Gain(f32, u32),
    /// Effect: reads its sidechain (`TestSidechain`).
    Side(TestSidechainMode),
}

/// The instruments of [`Dev`].
struct Source {
    dev: Dev,
    frame: u64,
    state: u32,
}

impl Processor for Source {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        let [l, r] = &mut io.output;
        for i in 0..l.len() {
            let (a, b) = match self.dev {
                Dev::Dc(level) => (level, level),
                Dev::Pulse(at, level) => {
                    let x = if self.frame + i as u64 == at {
                        level
                    } else {
                        0.0
                    };
                    (x, x)
                }
                Dev::Noise(_) => {
                    let mut next = || {
                        self.state = self
                            .state
                            .wrapping_mul(1_664_525)
                            .wrapping_add(1_013_904_223);
                        (self.state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                    };
                    (next(), next())
                }
                Dev::Busy(k) => {
                    let mut x = self.state;
                    for _ in 0..k {
                        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        x ^= x >> 13;
                    }
                    self.state = x;
                    let v = (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                    (v * 0.1, v * 0.1)
                }
                Dev::Gain(..) | Dev::Side(_) => (0.0, 0.0),
            };
            l[i] = a;
            r[i] = b;
        }
        self.frame += l.len() as u64;
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

fn processor(dev: Dev) -> Box<dyn Processor> {
    match dev {
        Dev::Gain(gain, latency) => Box::new(TestGain::with_latency(gain, latency)),
        Dev::Side(mode) => Box::new(TestSidechain::new(mode)),
        Dev::Noise(seed) | Dev::Busy(seed) => Box::new(Source {
            dev,
            frame: 0,
            state: seed,
        }),
        _ => Box::new(Source {
            dev,
            frame: 0,
            state: 0,
        }),
    }
}

fn latency(dev: Dev) -> u32 {
    match dev {
        Dev::Gain(_, latency) => latency,
        _ => 0,
    }
}

fn plugin(name: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: format!("test.{name}"),
        name: name.to_owned(),
        vendor: "Test".to_owned(),
        version: "1.0".to_owned(),
        module: Box::default(),
    }
}

/// A test project and what its devices are.
struct Song {
    project: Project,
    devs: HashMap<DeviceId, Dev>,
    /// Latency the compiler is told per device (defaults to the device's own).
    reported: HashMap<DeviceId, u32>,
}

impl CompileEnv for Song {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.reported
            .get(&device)
            .copied()
            .or_else(|| self.devs.get(&device).map(|d| latency(*d)))
            .unwrap_or(0)
    }
}

impl Song {
    fn new() -> Song {
        Song {
            project: Project::new("Routing"),
            devs: HashMap::new(),
            reported: HashMap::new(),
        }
    }

    #[track_caller]
    fn edit(&mut self, f: impl FnOnce(&Project) -> edits::EditResult) {
        self.project = f(&self.project).expect("edit");
    }

    /// A MIDI track with `instrument`, output to the master.
    fn track(&mut self, instrument: Dev) -> TrackId {
        let spec = NewTrack {
            name: None,
            hue: None,
            instrument: Some(plugin("src")),
            index: None,
            arm_exclusive: false,
        };
        let (project, id) = edits::add_track(&self.project, spec).unwrap();
        self.project = project;
        let device = self.project.track(id).unwrap().channel.devices[0].id;
        self.devs.insert(device, instrument);
        id
    }

    fn bus(&mut self, spec: NewBus) -> TrackId {
        let (project, id) = edits::add_bus(&self.project, spec).unwrap();
        self.project = project;
        id
    }

    /// A group of `members`.
    fn group(&mut self, members: &[TrackId]) -> TrackId {
        self.bus(NewBus::group(members.to_vec()))
    }

    /// An effect bus fed by a send from each of `senders` (−6 dB, post).
    fn fx_bus(&mut self, senders: &[TrackId]) -> TrackId {
        self.bus(NewBus::effect(senders.to_vec()))
    }

    /// Appends the effect `dev` to `channel`.
    fn fx(&mut self, channel: ChannelRef, dev: Dev) -> DeviceId {
        let index = match channel {
            ChannelRef::Track(id) => self.project.track(id).unwrap().channel.devices.len(),
            ChannelRef::Master => self.project.master.devices.len(),
        };
        let spec = NewDevice {
            role: DeviceRole::Effect,
            plugin: plugin("fx"),
            state: None,
        };
        let (project, id) = edits::insert_device(&self.project, channel, index, spec).unwrap();
        self.project = project;
        self.devs.insert(id, dev);
        id
    }

    fn channel(&mut self, track: TrackId, prop: ChannelProp) {
        self.edit(|p| edits::set_track_prop(p, &[track], TrackProp::Channel(prop)));
    }

    fn volume(&mut self, track: TrackId, db: f32) {
        self.channel(track, ChannelProp::VolumeDb(db));
    }

    fn send_to(&self, from: TrackId, to: TrackId) -> SendId {
        self.project.track(from).unwrap().send_to(to).unwrap().id
    }

    /// Sets the send `from → to` (adding it first if needed).
    fn send(&mut self, from: TrackId, to: TrackId, db: f32, tap: Tap) -> SendId {
        if self.project.track(from).unwrap().send_to(to).is_none() {
            let (project, _) = edits::add_send(&self.project, from, to).unwrap();
            self.project = project;
        }
        let id = self.send_to(from, to);
        self.edit(|p| edits::set_send(p, id, SendProp::LevelDb(db)));
        self.edit(|p| edits::set_send(p, id, SendProp::Tap(tap)));
        id
    }

    fn output(&mut self, track: TrackId, output: TrackOutput) {
        self.edit(|p| edits::set_output(p, &[track], output));
    }

    fn sidechain(&mut self, device: DeviceId, source: TrackId, tap: Tap) {
        self.edit(|p| edits::set_sidechain(p, device, Some(Sidechain { source, tap })));
    }

    /// Compiles against `previous`, inserts the processors of new devices and installs the
    /// program.
    fn load(&self, rig: &mut Rig, previous: Option<&Program>) -> Program {
        let program = compile::build(&self.project, previous, self);
        for (device, slot) in compile::slot_changes(previous, &program).added {
            rig.send(Command::InsertProcessor {
                slot,
                processor: processor(self.devs[&device]),
            });
        }
        rig.set_program(&program);
        program
    }
}

fn db(db: f32) -> f32 {
    db_to_gain(db)
}

#[track_caller]
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= 1e-5 * expected.abs().max(1.0),
        "{what}: {actual} != {expected}"
    );
}

/// The last frame of the output so far.
fn last(rig: &Rig) -> (f32, f32) {
    (*rig.l.last().unwrap(), *rig.r.last().unwrap())
}

/// Frames of `buf` that are not zero.
fn nonzero(buf: &[f32]) -> Vec<usize> {
    (0..buf.len()).filter(|&i| buf[i] != 0.0).collect()
}

/// The peak of a track's (or bus's) meter since the last read.
fn meter(program: &Program, track: TrackId) -> f32 {
    program.meter(track).unwrap().take().0
}

// ------------------------------------------------------------------------------------------------
// Sums, sends, groups
// ------------------------------------------------------------------------------------------------

#[test]
fn a_bus_sums_its_inputs_times_their_gains() {
    let mut s = Song::new();
    let a = s.track(Dev::Dc(0.25));
    s.volume(a, -6.0);
    let b = s.track(Dev::Dc(0.5));
    let g = s.group(&[a, b]);
    s.volume(g, -3.0);
    let c = s.track(Dev::Dc(0.125));
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    assert!(!program.routing.is_flat());
    rig.process(2048, 256);
    let bus = (0.25 * db(-6.0) + 0.5) * db(-3.0);
    let (l, r) = last(&rig);
    assert_close(l, bus + 0.125, "master left");
    assert_close(r, bus + 0.125, "master right");
    // The bus meters like a track (after its fader).
    assert_close(meter(&program, g), bus, "bus meter");
    assert_close(meter(&program, c), 0.125, "track meter");
}

#[test]
fn pre_and_post_sends_tap_before_and_after_the_fader() {
    let mut s = Song::new();
    let t = s.track(Dev::Dc(0.5));
    s.volume(t, -6.0);
    let pre = s.fx_bus(&[t]);
    s.send(t, pre, 0.0, Tap::Pre);
    let post = s.fx_bus(&[t]);
    s.send(t, post, -6.0, Tap::Post);
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    rig.process(2048, 256);
    let dry = 0.5 * db(-6.0);
    assert_close(meter(&program, pre), 0.5, "pre send ignores the fader");
    assert_close(
        meter(&program, post),
        dry * db(-6.0),
        "post send follows it",
    );
    // Both buses output to the master with the dry signal.
    assert_close(last(&rig).0, dry + 0.5 + dry * db(-6.0), "master");
}

#[test]
fn a_send_reaches_the_master_through_its_bus_and_a_disabled_one_feeds_nothing() {
    let mut s = Song::new();
    let t = s.track(Dev::Dc(0.5));
    s.channel(t, ChannelProp::Mute(false));
    let fx = s.fx_bus(&[t]);
    s.fx(ChannelRef::Track(fx), Dev::Gain(0.5, 0));
    s.volume(fx, -3.0);
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    rig.process(1024, 256);
    let wet = 0.5 * db(-6.0) * 0.5 * db(-3.0);
    assert_close(last(&rig).0, 0.5 + wet, "dry + wet");
    let send = s.send_to(t, fx);
    s.edit(|p| edits::set_send(p, send, SendProp::Enabled(false)));
    s.load(&mut rig, Some(&program));
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.5, "dry only");
}

#[test]
fn nested_groups_multiply_their_faders() {
    let mut s = Song::new();
    let a = s.track(Dev::Dc(0.25));
    let b = s.track(Dev::Dc(0.5));
    let inner = s.group(&[a]);
    s.volume(inner, -6.0);
    let outer = s.group(&[inner, b]);
    s.volume(outer, -3.0);
    assert_eq!(
        s.project.track(inner).unwrap().output,
        TrackOutput::Bus(outer)
    );
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    // Sources first, the outer group last.
    let index = |id| program.tracks.iter().position(|t| t.id == id).unwrap() as u16;
    let order = &program.routing.order;
    let at = |id| order.iter().position(|&i| i == index(id)).unwrap();
    assert!(at(a) < at(inner) && at(inner) < at(outer) && at(b) < at(outer));
    rig.process(2048, 256);
    let expected = (0.25 * db(-6.0) + 0.5) * db(-3.0);
    assert_close(last(&rig).0, expected, "master");
    assert_close(meter(&program, inner), 0.25 * db(-6.0), "inner meter");
}

// ------------------------------------------------------------------------------------------------
// Sidechains
// ------------------------------------------------------------------------------------------------

#[test]
fn a_sidechain_gets_the_source_tap_pre_or_post() {
    let mut s = Song::new();
    let source = s.track(Dev::Dc(0.5));
    s.volume(source, -6.0);
    let holder = s.track(Dev::Dc(0.0));
    let device = s.fx(
        ChannelRef::Track(holder),
        Dev::Side(TestSidechainMode::Replace),
    );
    s.sidechain(device, source, Tap::Pre);
    // The source's own output is muted: the master carries the holder only.
    s.channel(source, ChannelProp::Mute(true));
    let mut rig = Rig::new(RATE, 256);
    let mut program = s.load(&mut rig, None);
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.5, "pre: no fader, no mute");
    s.sidechain(device, source, Tap::Post);
    program = s.load(&mut rig, Some(&program));
    rig.process(1024, 256);
    assert_eq!(last(&rig), (0.0, 0.0), "post follows mute");
    s.channel(source, ChannelProp::Mute(false));
    s.load(&mut rig, Some(&program));
    rig.process(1024, 256);
    // The source plays now too (0.5 × −6 dB), the holder outputs its post tap (the same).
    let post = 0.5 * db(-6.0);
    assert_close(last(&rig).0, post + post, "post follows the fader");
}

/// Plays `s` for 4096 frames and returns where the left and right outputs are not zero.
fn arrivals(s: &Song) -> (Vec<usize>, Vec<usize>) {
    let mut rig = Rig::new(RATE, 256);
    s.load(&mut rig, None);
    rig.process(4096, 256);
    (nonzero(&rig.l), nonzero(&rig.r))
}

#[test]
fn a_late_sidechain_delays_its_holder() {
    // Lookahead on the source: the holder's head waits for it.
    let mut s = Song::new();
    let source = s.track(Dev::Pulse(1000, 1.0));
    s.fx(ChannelRef::Track(source), Dev::Gain(1.0, 100));
    s.channel(source, ChannelProp::Mute(true));
    let holder = s.track(Dev::Pulse(1000, 0.5));
    let device = s.fx(
        ChannelRef::Track(holder),
        Dev::Side(TestSidechainMode::Split),
    );
    s.sidechain(device, source, Tap::Pre);
    let (l, r) = arrivals(&s);
    assert_eq!(l, [1100], "main input of the device");
    assert_eq!(r, [1100], "sidechain input");
}

#[test]
fn an_early_sidechain_is_delayed_to_its_device() {
    // Lookahead on the holder in front of the device: the sidechain waits.
    let mut s = Song::new();
    let source = s.track(Dev::Pulse(1000, 1.0));
    s.channel(source, ChannelProp::Mute(true));
    let holder = s.track(Dev::Pulse(1000, 0.5));
    s.fx(ChannelRef::Track(holder), Dev::Gain(1.0, 64));
    let device = s.fx(
        ChannelRef::Track(holder),
        Dev::Side(TestSidechainMode::Split),
    );
    s.sidechain(device, source, Tap::Pre);
    let (l, r) = arrivals(&s);
    assert_eq!((l, r), (vec![1064], vec![1064]));
}

#[test]
fn sidechains_on_a_bus_and_on_the_master_line_up() {
    let mut s = Song::new();
    let source = s.track(Dev::Pulse(1000, 1.0));
    s.fx(ChannelRef::Track(source), Dev::Gain(1.0, 80));
    s.channel(source, ChannelProp::Mute(true));
    let member = s.track(Dev::Pulse(1000, 0.5));
    let bus = s.group(&[member]);
    let device = s.fx(ChannelRef::Track(bus), Dev::Side(TestSidechainMode::Split));
    s.sidechain(device, source, Tap::Pre);
    let (l, r) = arrivals(&s);
    assert_eq!((l, r), (vec![1080], vec![1080]), "bus");

    let mut s = Song::new();
    let source = s.track(Dev::Pulse(1000, 1.0));
    s.channel(source, ChannelProp::Mute(true));
    let slow = s.track(Dev::Pulse(1000, 0.5));
    s.fx(ChannelRef::Track(slow), Dev::Gain(1.0, 30));
    let device = s.fx(ChannelRef::Master, Dev::Side(TestSidechainMode::Split));
    s.sidechain(device, source, Tap::Pre);
    let (l, r) = arrivals(&s);
    assert_eq!((l, r), (vec![1030], vec![1030]), "master");
}

// ------------------------------------------------------------------------------------------------
// Delay compensation
// ------------------------------------------------------------------------------------------------

#[test]
fn parallel_paths_with_different_latencies_arrive_together() {
    let mut s = Song::new();
    let t1 = s.track(Dev::Pulse(1000, 1.0));
    let t2 = s.track(Dev::Pulse(1000, 2.0));
    let t3 = s.track(Dev::Pulse(1000, 4.0));
    let a = s.group(&[t1]);
    s.fx(ChannelRef::Track(a), Dev::Gain(1.0, 200));
    let b = s.fx_bus(&[]);
    s.fx(ChannelRef::Track(b), Dev::Gain(1.0, 50));
    s.output(t3, TrackOutput::Bus(b));
    s.send(t3, a, 0.0, Tap::Post);
    s.fx(ChannelRef::Track(t2), Dev::Gain(1.0, 7));
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    assert_eq!(program.routing.latency(), 200);
    rig.process(4096, 256);
    // t1 → a (200), t2 (7) → master, t3 → b (50) and t3 → send → a (200): one impulse.
    assert_eq!(nonzero(&rig.l), [1200]);
    assert_close(rig.l[1200], 1.0 + 2.0 + 4.0 + 4.0, "sum");
}

#[test]
fn latency_changes_move_the_delays_without_allocating() {
    let mut s = Song::new();
    let t1 = s.track(Dev::Noise(1));
    let t2 = s.track(Dev::Noise(2));
    let gain = s.fx(ChannelRef::Track(t1), Dev::Gain(1.0, 0));
    let g = s.group(&[t1]);
    s.send(t2, g, -3.0, Tap::Pre);
    let mut rig = Rig::new(RATE, 256);
    let mut program = s.load(&mut rig, None);
    rig.process(1024, 256);
    for latency in [64, 0, 300, 12] {
        // The device reports a new latency (`LatencyChanged`): the app recompiles.
        s.reported.insert(gain, latency);
        program = s.load(&mut rig, Some(&program));
        let index = program.tracks.iter().position(|t| t.id == t2).unwrap();
        assert_eq!(program.routing.node(index).latency_out, 0, "t2 has none");
        assert_eq!(program.tracks[index].pdc_delay, latency, "t2 waits for t1");
        rig.process(700, 256);
    }
    assert!(rig.l.iter().all(|x| x.is_finite()));
}

// ------------------------------------------------------------------------------------------------
// Mute and solo
// ------------------------------------------------------------------------------------------------

#[test]
fn solo_of_a_group_member_keeps_the_group_and_the_master() {
    let mut s = Song::new();
    let a = s.track(Dev::Dc(0.125));
    let b = s.track(Dev::Dc(0.25));
    let g = s.group(&[a, b]);
    s.track(Dev::Dc(0.5));
    let mut rig = Rig::new(RATE, 256);
    let before = s.load(&mut rig, None);
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.875, "all");

    // Fast path: the gate table of the soloed project, for the running program.
    s.channel(a, ChannelProp::Solo(true));
    rig.send(Command::SetGates(compile::gates(&s.project, &before)));
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.125, "fast path");
    // After the fades of the first block.
    meter(&before, g);
    rig.process(256, 256);
    assert_close(meter(&before, g), 0.125, "the group is heard");

    // The compiled program agrees.
    let after = s.load(&mut rig, Some(&before));
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.125, "compiled");
    s.channel(a, ChannelProp::Solo(false));
    s.load(&mut rig, Some(&after));
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.875, "unsoloed");
}

#[test]
fn solo_of_an_effect_bus_plays_only_its_wet_signal() {
    let mut s = Song::new();
    let t = s.track(Dev::Dc(0.5));
    let fx = s.fx_bus(&[t]);
    s.fx(ChannelRef::Track(fx), Dev::Gain(0.5, 0));
    s.track(Dev::Dc(0.25));
    s.channel(fx, ChannelProp::Solo(true));
    let mut rig = Rig::new(RATE, 256);
    s.load(&mut rig, None);
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.5 * db(-6.0) * 0.5, "wet only");
}

#[test]
fn mute_silences_pre_and_post_sends() {
    let mut s = Song::new();
    let t = s.track(Dev::Dc(0.5));
    let post = s.fx_bus(&[t]);
    let pre = s.fx_bus(&[t]);
    s.send(t, pre, 0.0, Tap::Pre);
    s.channel(t, ChannelProp::Mute(true));
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    rig.process(1024, 256);
    assert_eq!(last(&rig), (0.0, 0.0));
    assert_eq!(meter(&program, pre), 0.0);
    assert_eq!(meter(&program, post), 0.0);
    // Unmuted by the fast path: the sends fade in.
    s.channel(t, ChannelProp::Mute(false));
    rig.send(Command::SetGates(compile::gates(&s.project, &program)));
    rig.process(1024, 256);
    assert_close(meter(&program, pre), 0.5, "pre");
    assert_close(last(&rig).0, 0.5 + 0.5 * db(-6.0) + 0.5, "all");
}

#[test]
fn muting_a_bus_silences_its_output_only() {
    let mut s = Song::new();
    let t = s.track(Dev::Dc(0.5));
    let g = s.group(&[t]);
    let fx = s.fx_bus(&[t]);
    s.channel(g, ChannelProp::Mute(true));
    let mut rig = Rig::new(RATE, 256);
    let program = s.load(&mut rig, None);
    rig.process(1024, 256);
    assert_close(last(&rig).0, 0.5 * db(-6.0), "the send still feeds its bus");
    assert_close(meter(&program, t), 0.5, "the member plays on");
    assert_close(meter(&program, fx), 0.5 * db(-6.0), "fx");
}

// ------------------------------------------------------------------------------------------------
// Workers, swaps, export
// ------------------------------------------------------------------------------------------------

/// A routed project of noise sources with groups, a nested group, an effect bus with pre and post
/// sends, sidechains on a track, a bus and the master, and latencies everywhere.
fn big_song() -> Song {
    let mut s = Song::new();
    let t: Vec<TrackId> = (0..10).map(|i| s.track(Dev::Noise(i + 1))).collect();
    for (i, &id) in t.iter().enumerate() {
        s.volume(id, -(i as f32));
        s.channel(id, ChannelProp::Pan(i as f32 / 10.0 - 0.5));
        if i % 3 == 0 {
            s.fx(ChannelRef::Track(id), Dev::Gain(0.9, 16 * i as u32 + 5));
        }
    }
    let drums = s.group(&t[0..3]);
    s.fx(ChannelRef::Track(drums), Dev::Gain(0.8, 33));
    let keys = s.group(&t[3..5]);
    let all = s.group(&[drums, keys]);
    s.volume(all, -2.0);
    let fx = s.fx_bus(&[t[5], t[6], t[7]]);
    s.fx(ChannelRef::Track(fx), Dev::Gain(0.7, 120));
    s.send(t[0], fx, -9.0, Tap::Pre);
    s.send(t[8], fx, -4.0, Tap::Post);
    let duck = s.fx(ChannelRef::Track(t[9]), Dev::Side(TestSidechainMode::Split));
    s.sidechain(duck, t[0], Tap::Pre);
    let glue = s.fx(ChannelRef::Track(keys), Dev::Side(TestSidechainMode::Split));
    s.sidechain(glue, drums, Tap::Post);
    let master = s.fx(ChannelRef::Master, Dev::Side(TestSidechainMode::Split));
    s.sidechain(master, fx, Tap::Post);
    s.channel(t[6], ChannelProp::Mute(true));
    s
}

/// Plays [`big_song`] with `workers` threads, with gate changes, program swaps and a render.
fn play_big(workers: usize) -> (Vec<f32>, Vec<f32>) {
    let mut s = big_song();
    let mut rig = Rig::new(RATE, 256);
    let status = rig.engine.set_workers(workers, None);
    assert_eq!(status.threads, workers);
    let mut program = s.load(&mut rig, None);
    let blocks = [256, 256, 100, 512, 37, 2048, 3000];
    let ids: Vec<TrackId> = s.project.tracks.iter().map(|t| t.id).collect();
    for call in 0..40 {
        rig.process(blocks[call % blocks.len()], 256);
        match call {
            8 => {
                s.channel(ids[4], ChannelProp::Solo(true));
                rig.send(Command::SetGates(compile::gates(&s.project, &program)));
            }
            14 => program = s.load(&mut rig, Some(&program)),
            20 => {
                s.channel(ids[4], ChannelProp::Solo(false));
                let sender = s.project.tracks.iter().find(|t| !t.sends.is_empty());
                let send = sender.unwrap().sends[0].id;
                s.edit(|p| edits::set_send(p, send, SendProp::LevelDb(-1.0)));
                program = s.load(&mut rig, Some(&program));
            }
            26 => {
                // A nested group leaves its outer group.
                let nested = s
                    .project
                    .tracks
                    .iter()
                    .find(|t| t.is_bus() && t.output != TrackOutput::Master);
                let id = nested.unwrap().id;
                s.output(id, TrackOutput::Master);
                program = s.load(&mut rig, Some(&program));
            }
            _ => {}
        }
    }
    rig.render(20_000, 1000);
    let stats = rig.engine.worker_stats();
    assert_eq!(stats.worker_jobs() > 0, workers > 0, "workers took part");
    (rig.l, rig.r)
}

#[test]
fn worker_counts_give_bit_identical_output() {
    let serial = play_big(0);
    assert!(serial.0.iter().any(|x| x.abs() > 0.01), "silent");
    for workers in [1, 4] {
        let parallel = play_big(workers);
        assert_eq!(parallel.0.len(), serial.0.len());
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
        for (channel, (a, b)) in [(&serial.0, &parallel.0), (&serial.1, &parallel.1)]
            .into_iter()
            .enumerate()
        {
            if let Some(i) = bits(a).iter().zip(bits(b)).position(|(x, y)| *x != y) {
                panic!("{workers} workers: channel {channel} differs first at frame {i}");
            }
        }
    }
}

/// Swapping in a recompiled program of the same project keeps every delay line and gain (no
/// allocation on the audio thread, no click): the output is bit-identical to not swapping.
#[test]
fn program_swaps_keep_the_graph_state() {
    let s = big_song();
    let run = |swap: bool| {
        let mut rig = Rig::new(RATE, 256);
        let mut program = s.load(&mut rig, None);
        for call in 0..30 {
            rig.process(300, 256);
            if swap && call % 3 == 0 {
                // A fresh compile from the same previous program (lines and slots kept).
                program = s.load(&mut rig, Some(&program));
            }
        }
        // The old programs come back for dropping on this thread.
        assert!(rig.garbage >= if swap { 10 } else { 0 });
        (rig.l, rig.r)
    };
    let (a, b) = (run(false), run(true));
    assert!(a.0 == b.0 && a.1 == b.1, "a swap changed the output");
}

#[test]
fn offline_render_plays_the_same_graph() {
    let s = big_song();
    let mut live = Rig::new(RATE, 256);
    s.load(&mut live, None);
    live.process(20_000, 512);
    let mut offline = Rig::new(RATE, 256);
    s.load(&mut offline, None);
    offline.render(20_000, 512);
    assert!(live.l == offline.l && live.r == offline.r);
}

#[test]
fn a_sidechained_device_is_called_with_its_input_every_call() {
    let mut s = Song::new();
    let source = s.track(Dev::Dc(0.5));
    let holder = s.track(Dev::Dc(0.0));
    let device = s.fx(
        ChannelRef::Track(holder),
        Dev::Side(TestSidechainMode::Replace),
    );
    s.sidechain(device, source, Tap::Pre);
    let mut rig = Rig::new(RATE, 256);
    let program = compile::build(&s.project, None, &s);
    let mut probe = None;
    for (id, slot) in compile::slot_changes(None, &program).added {
        let processor: Box<dyn Processor> = if id == device {
            let p = TestSidechain::new(TestSidechainMode::Replace);
            probe = Some(p.sidechain_calls());
            Box::new(p)
        } else {
            processor(s.devs[&id])
        };
        rig.send(Command::InsertProcessor { slot, processor });
    }
    rig.set_program(&program);
    rig.process(1000, 100);
    assert_eq!(probe.unwrap().load(Ordering::Relaxed), 10);
}

// ------------------------------------------------------------------------------------------------
// Timing
// ------------------------------------------------------------------------------------------------

/// 16 tracks, each `instrument` → `TestGain` (latencies 0 … 75 frames). Routed: two groups of six
/// tracks, a group around both (nested), two effect buses (a `TestGain` each) fed by a post send
/// from every track and a pre send from every other track, and a sidechain from track 0 to an
/// effect on track 12. Flat: every track straight into the master.
fn bench_song(routed: bool, instrument: impl Fn(u32) -> Dev) -> Song {
    let mut s = Song::new();
    let t: Vec<TrackId> = (0..16).map(|i| s.track(instrument(i))).collect();
    for (i, &id) in t.iter().enumerate() {
        s.fx(ChannelRef::Track(id), Dev::Gain(0.9, 5 * (i as u32 % 16)));
    }
    if routed {
        let a = s.group(&t[0..6]);
        let b = s.group(&t[6..12]);
        s.group(&[a, b]);
        let hall = s.fx_bus(&t);
        s.fx(ChannelRef::Track(hall), Dev::Gain(0.5, 64));
        let echo = s.fx_bus(&[]);
        s.fx(ChannelRef::Track(echo), Dev::Gain(0.5, 0));
        for &id in t.iter().step_by(2) {
            s.send(id, echo, -10.0, Tap::Pre);
        }
        let duck = s.fx(
            ChannelRef::Track(t[12]),
            Dev::Side(TestSidechainMode::Split),
        );
        s.sidechain(duck, t[0], Tap::Pre);
    }
    s
}

/// Mean and worst wall time of one 256-frame `process` call over 20 s, with `workers` workers.
fn bench_run(song: &Song, workers: usize) -> (std::time::Duration, std::time::Duration) {
    use std::time::{Duration, Instant};
    const BLOCK: usize = 256;
    const BLOCKS: usize = 20 * 48_000 / BLOCK;
    let mut rig = Rig::new(RATE, BLOCK);
    let status = rig.engine.set_workers(
        workers,
        Some(komponist_engine::workers::DEFAULT_WORKER_PRIORITY),
    );
    if let Some(e) = status.realtime_error {
        eprintln!("workers without SCHED_FIFO: {e}");
    }
    song.load(&mut rig, None);
    rig.process(96_000, BLOCK);
    rig.l.reserve(BLOCKS * BLOCK);
    rig.r.reserve(BLOCKS * BLOCK);
    let (mut total, mut worst) = (Duration::ZERO, Duration::ZERO);
    for _ in 0..BLOCKS {
        let t0 = Instant::now();
        rig.process_with(BLOCK, &[], 0);
        let t = t0.elapsed();
        total += t;
        worst = worst.max(t);
    }
    (total / BLOCKS as u32, worst)
}

/// Makes the instrument of track `i`.
type Instrument = fn(u32) -> Dev;

/// CPU per block of 16 tracks + 4 buses + sends + a sidechain against the same 16 tracks without
/// routing (ARCHITECTURE §6.8.4, PERFORMANCE §9). Release build:
///
/// ```text
/// cargo test --release -p komponist-engine --test engine routing::bench -- --ignored --nocapture
/// ```
#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn bench_routing_16_tracks_4_buses() {
    let period = std::time::Duration::from_secs_f64(256.0 / f64::from(RATE));
    let share = |d: std::time::Duration| d.as_secs_f64() / period.as_secs_f64() * 100.0;
    let counts: Vec<usize> = std::env::var("KOMPONIST_BENCH_WORKERS")
        .ok()
        .map(|s| s.split(',').filter_map(|w| w.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0, 4]);
    let loads: [(&str, Instrument); 2] = [
        ("constant instruments", |_| Dev::Dc(0.01)),
        ("busy instruments", |i| Dev::Busy(i + 1)),
    ];
    for (name, instrument) in loads {
        let busy = name.starts_with("busy");
        for workers in &counts {
            for routed in [false, true] {
                let mut song = bench_song(routed, instrument);
                if busy {
                    // About 150 µs per track and block in release builds.
                    for dev in song.devs.values_mut() {
                        if let Dev::Busy(seed) = dev {
                            *dev = Dev::Busy(*seed % 7 + 320);
                        }
                    }
                }
                let (mean, worst) = bench_run(&song, *workers);
                eprintln!(
                    "{name}, {}, {workers} workers: mean {mean:.1?} ({:.2} %), worst {worst:.1?} ({:.1} %) per 256 frames",
                    if routed {
                        "16 tracks + 4 buses + sends"
                    } else {
                        "16 tracks, flat"
                    },
                    share(mean),
                    share(worst),
                );
            }
        }
    }
}
