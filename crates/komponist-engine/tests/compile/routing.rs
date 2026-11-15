//! The routing part of the compiler (WP-102, ARCHITECTURE §6.8.4–6.8.6): job order, bus inputs,
//! taps, delays across the graph, stable edge lines, gates.

use std::sync::Arc;

use komponist_engine::compile::{self, NoEnv};
use komponist_engine::route::{LineKey, RouteInput};
use komponist_model::fixtures::demo_project;
use komponist_model::{
    DeviceId, DeviceRole, Project, SendId, Sidechain, Tap, Track, TrackId, TrackKind, TrackOutput,
    TrackSend,
};

use crate::support::*;

fn bus(id: u64, name: &str) -> Track {
    Track {
        kind: TrackKind::Bus,
        folder: true,
        ..track(id, name, Vec::new())
    }
}

fn send(id: u64, target: u64, tap: Tap) -> TrackSend {
    TrackSend {
        id: SendId(id),
        target: TrackId(target),
        level_db: 0.0,
        tap,
        enabled: true,
    }
}

/// Display order G (bus 3), A (1), B (2) → G; C (4) → master with a pre send to G. Latencies: A's
/// effect 100, G's 10, C's 30.
fn graph() -> (Project, TestEnv) {
    let mut a = track(1, "A", Vec::new());
    a.channel.devices = vec![
        device(101, DeviceRole::Instrument, "I"),
        device(102, DeviceRole::Effect, "E"),
    ];
    a.output = TrackOutput::Bus(TrackId(3));
    let mut b = track(2, "B", Vec::new());
    b.channel.devices = vec![device(201, DeviceRole::Instrument, "I")];
    b.output = TrackOutput::Bus(TrackId(3));
    let mut g = bus(3, "G");
    g.channel.devices = vec![device(301, DeviceRole::Effect, "E")];
    let mut c = track(4, "C", Vec::new());
    c.channel.devices = vec![
        device(401, DeviceRole::Instrument, "I"),
        device(402, DeviceRole::Effect, "E"),
    ];
    c.sends = vec![send(9001, 3, Tap::Pre)];
    let env = TestEnv {
        latencies: vec![
            (DeviceId(102), 100),
            (DeviceId(301), 10),
            (DeviceId(402), 30),
        ],
        ..TestEnv::default()
    };
    (project(vec![g, a, b, c]), env)
}

#[test]
fn projects_without_routing_compile_flat() {
    let program = compile::build(&demo_project(), None, &NoEnv);
    assert!(program.routing.is_flat());
    assert!(program.routing.gates.is_none());
    // A bus nothing routes into changes nothing either.
    let mut p = demo_project();
    p.tracks.push(Arc::new(bus(77, "Leer")));
    assert!(compile::build(&p, None, &NoEnv).routing.is_flat());
}

#[test]
fn the_graph_compiles_to_jobs_inputs_and_delays() {
    let (p, env) = graph();
    let program = compile::build(&p, None, &env);
    let r = &*program.routing;
    // Program tracks in display order: G 0, A 1, B 2, C 3. Jobs: sources first, G last.
    assert_eq!(&*r.order, &[1, 2, 3, 0]);
    let g = r.node(0);
    assert!(g.bus);
    assert_eq!((g.latency_in, g.latency_out), (100, 110));
    assert_eq!(g.waits, 3);
    let inputs: Vec<(u16, Option<u32>)> = r
        .inputs(g.inputs)
        .iter()
        .map(|i: &RouteInput| (i.from, i.send.map(|s| s.delay.frames)))
        .collect();
    // A, B (outputs) and C's send, in job order; the send waits 100 − 30.
    assert_eq!(inputs, [(1, None), (2, None), (3, Some(70))]);
    let master: Vec<u16> = r.inputs(r.master.inputs).iter().map(|i| i.from).collect();
    assert_eq!(master, [3, 0]);
    assert_eq!(r.latency(), 110);
    // Output delays: A → G 0, B → G 100, G → master 0, C → master 80.
    let pdc: Vec<u32> = program.tracks.iter().map(|t| t.pdc_delay).collect();
    assert_eq!(pdc, [0, 0, 100, 80]);
    // C's pre send needs its pre tap; nothing reads a post tap.
    assert!(r.node(3).pre && !r.node(3).post);
    assert!(!r.node(1).pre && !r.node(1).post);
    for source in [1, 2, 3] {
        assert_eq!(r.dependents(r.node(source).dependents), [0]);
    }
}

#[test]
fn a_late_sidechain_gives_its_midi_track_a_head_delay() {
    let mut s = track(1, "S", Vec::new());
    s.channel.devices = vec![
        device(101, DeviceRole::Instrument, "I"),
        device(102, DeviceRole::Effect, "E"),
    ];
    let mut h = track(2, "H", Vec::new());
    let mut duck = (*device(202, DeviceRole::Effect, "Duck")).clone();
    duck.sidechain = Some(Sidechain::new(TrackId(1)));
    h.channel.devices = vec![device(201, DeviceRole::Instrument, "I"), Arc::new(duck)];
    let env = TestEnv {
        latencies: vec![(DeviceId(102), 50)],
        ..TestEnv::default()
    };
    let p = project(vec![s, h]);
    let program = compile::build(&p, None, &env);
    let r = &*program.routing;
    let holder = r.node(1);
    assert_eq!(holder.head.map(|d| d.frames), Some(50));
    assert_eq!((holder.latency_in, holder.latency_out), (50, 50));
    assert_eq!(holder.waits, 1);
    let side = r.sidechains(holder.sidechains);
    assert_eq!(side.len(), 1);
    assert_eq!(
        (side[0].device, side[0].from, side[0].tap),
        (1, 0, Tap::Pre)
    );
    assert_eq!(side[0].delay.frames, 0);
    assert!(r.node(0).pre);
    // Both reach the master at 50: nothing else to delay.
    let pdc: Vec<u32> = program.tracks.iter().map(|t| t.pdc_delay).collect();
    assert_eq!(pdc, [0, 0]);
}

#[test]
fn edges_keep_their_lines_across_recompiles() {
    let (p, env) = graph();
    let first = compile::build(&p, None, &env);
    let line = |program: &komponist_engine::Program, key| program.routing.lines.line(key);
    let old = line(&first, LineKey::Send(SendId(9001))).unwrap();

    // A new send from B: another line; C's send keeps its own.
    let mut p2 = p.clone();
    Arc::make_mut(&mut p2.tracks[2]).sends = vec![send(9002, 3, Tap::Post)];
    let second = compile::build(&p2, Some(&first), &env);
    assert_eq!(line(&second, LineKey::Send(SendId(9001))), Some(old));
    let new = line(&second, LineKey::Send(SendId(9002))).unwrap();
    assert_ne!(new, old);
    assert!(second.routing.node(2).post);

    // C's send goes: B's keeps its line.
    let mut p3 = p2.clone();
    Arc::make_mut(&mut p3.tracks[3]).sends.clear();
    let third = compile::build(&p3, Some(&second), &env);
    assert_eq!(line(&third, LineKey::Send(SendId(9002))), Some(new));
    assert_eq!(line(&third, LineKey::Send(SendId(9001))), None);
}

#[test]
fn gates_follow_solo_along_the_signal_path() {
    let (mut p, env) = graph();
    let program = compile::build(&p, None, &env);
    let gates = program.routing.gates.unwrap();
    assert!((0..4).all(|i| gates.output_open(i)) && gates.send_open(0));

    // Solo A: its group and the master stay open, C and its send are solo-muted.
    Arc::make_mut(&mut p.tracks[1]).channel.solo = true;
    let gates = compile::gates(&p, &program);
    assert!(gates.output_open(0) && gates.output_open(1));
    assert!(!gates.output_open(2) && !gates.output_open(3));
    assert!(!gates.send_open(0));
    // Mute C: closed and muted (its post tap too).
    Arc::make_mut(&mut p.tracks[3]).channel.mute = true;
    let gates = compile::gates(&p, &program);
    assert!(gates.muted(3) && !gates.muted(1));
    // The compiled program carries the same table.
    assert_eq!(compile::build(&p, None, &env).routing.gates, Some(gates));
}

#[test]
fn a_broken_graph_compiles_flat() {
    // Two buses routed into each other (only a corrupt file could hold this).
    let mut a = bus(1, "A");
    a.output = TrackOutput::Bus(TrackId(2));
    let mut b = bus(2, "B");
    b.output = TrackOutput::Bus(TrackId(1));
    let program = compile::build(&project(vec![a, b]), None, &NoEnv);
    assert!(program.routing.is_flat());
}
