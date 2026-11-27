//! 1 000 random command sequences (PLAN WP-23 "Done when").
//!
//! Every sequence builds an engine with random settings, sends random commands (programs, transport,
//! processors, fast paths, live notes, panic) between `process` / `render` calls of random length
//! with random live MIDI, all inside `assert_no_alloc`. Checked after every call:
//! - output finite and bounded,
//! - processor calls continuous, `1 ..= max_block` frames, events inside the call and sorted
//!   (note-offs before note-ons at the same frame),
//! - every note id is new at its note-on, and a note-off only ends a note sounding on the same
//!   processor (no double or stray note-offs).
//!
//! Half of the sequences keep their tracks and instruments fixed ("stable"); after a final `Stop`
//! every note-on there must have had its note-off.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use klang_dsp::rng::Rng;
use komponist_engine::program::{MidiPort, PROBABILITY_FOLLOW, SeqNote, TrackProgram};
use komponist_engine::testing::{EventLog, Logged, TestGain, TestSine};
use komponist_engine::{Command, EngineConfig, MAX_PDC_FRAMES, MidiInEvent, Program, SlotId};
use komponist_model::{Tick, TickRange, TrackId};
use komponist_processor::EventKind;

use crate::common::*;

struct Gen(Rng);

impl Gen {
    fn below(&mut self, n: usize) -> usize {
        self.0.below(n.max(1))
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as usize) as i64
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    fn unit(&mut self) -> f32 {
        self.0.unipolar()
    }
}

/// Bookkeeping of one logged instrument.
#[derive(Default)]
struct Checker {
    next_frame: u64,
    open: HashSet<u32>,
}

/// Note ids seen in note-ons across all processors of a sequence.
type Seen = HashSet<u32>;

impl Checker {
    fn check(&mut self, entries: &[Logged], max_block: usize, seen: &mut Seen, ctx: &str) {
        let mut call = (0_u64, 0_u64);
        let mut last: Option<(u64, bool)> = None;
        for entry in entries {
            match *entry {
                Logged::Call { frame, frames, .. } => {
                    assert_eq!(frame, self.next_frame, "{ctx}: calls continuous");
                    assert!(
                        (1..=max_block as u32).contains(&frames),
                        "{ctx}: {frames} frames"
                    );
                    self.next_frame += u64::from(frames);
                    call = (frame, frame + u64::from(frames));
                    last = None;
                }
                Logged::Event { frame, kind } => {
                    assert!(
                        (call.0..call.1).contains(&frame),
                        "{ctx}: event at {frame} outside {call:?}"
                    );
                    let is_on = matches!(kind, EventKind::NoteOn { .. });
                    if let Some((f, was_on)) = last {
                        assert!(f <= frame, "{ctx}: events sorted");
                        let off_after_on =
                            f == frame && was_on && matches!(kind, EventKind::NoteOff { .. });
                        assert!(!off_after_on, "{ctx}: note-off after note-on at {frame}");
                    }
                    last = Some((frame, is_on));
                    match kind {
                        EventKind::NoteOn {
                            note_id,
                            velocity,
                            key,
                            channel,
                        } => {
                            assert!(seen.insert(note_id), "{ctx}: note id {note_id} reused");
                            assert!((1..=127).contains(&velocity) && key <= 127 && channel <= 15);
                            self.open.insert(note_id);
                        }
                        EventKind::NoteOff { note_id, .. } => {
                            assert!(
                                self.open.remove(&note_id),
                                "{ctx}: note-off for {note_id} not sounding here"
                            );
                        }
                        _ => {}
                    }
                }
                Logged::Reset { .. } => {}
            }
        }
    }
}

/// Instrument slots 1..=8 (logged), effect slots 11..=18.
const INSTRUMENTS: u32 = 8;

struct Sequence {
    g: Gen,
    rig: Rig,
    stable: bool,
    tracks: usize,
    max_block: usize,
    logs: HashMap<u32, (EventLog, Checker)>,
    seen: Seen,
    seed: u32,
    /// Hash of every output sample's bits so far (FNV-1a over the words).
    digest: u64,
}

impl Sequence {
    fn new(seed: u32) -> Sequence {
        let mut g = Gen(Rng::new(seed.wrapping_mul(0x9e37_79b9) ^ 0x5555));
        let rate = g.pick(&[44_100.0, 48_000.0, 96_000.0, 22_050.0]);
        let max_block = g.pick(&[1, 16, 64, 333, 2048]);
        let rig = Rig::with_config(EngineConfig {
            sample_rate: rate,
            max_block,
            seed,
        });
        let tracks = 1 + g.below(4);
        let mut s = Sequence {
            g,
            rig,
            stable: seed.is_multiple_of(2),
            tracks,
            max_block,
            logs: HashMap::new(),
            seen: HashSet::new(),
            seed,
            digest: 0xcbf2_9ce4_8422_2325,
        };
        for slot in 1..=INSTRUMENTS {
            s.insert_sine(slot);
            s.rig.insert(10 + slot, TestGain::new(1.0));
        }
        s
    }

    fn insert_sine(&mut self, slot: u32) {
        let (sine, log) = TestSine::logged(1 << 15);
        self.rig.insert(slot, sine);
        // A replaced processor's open notes are gone with it.
        self.logs.insert(slot, (log, Checker::default()));
    }

    fn notes(&mut self) -> Vec<SeqNote> {
        let g = &mut self.g;
        let mut notes: Vec<SeqNote> = (0..g.below(40))
            .map(|_| SeqNote {
                start: Tick({
                    let end = if g.chance(60) { 960 } else { BAR };
                    g.range(0, end)
                }),
                length: Tick(g.pick(&[1, 30, 240, 960, 5000, 20_000])),
                key: g.range(24, 100) as u8,
                velocity: g.range(1, 127) as u8,
                probability: g.pick(&[100, 100, 50, 0, PROBABILITY_FOLLOW]),
                channel: g.pick(&[0, 0, 0, 9]),
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        notes
    }

    fn program(&mut self) -> Program {
        let count = if self.stable {
            self.tracks
        } else {
            self.g.below(6)
        };
        // Distinct instrument slots; the stable sequences keep track i on slot i.
        let mut slots: Vec<u32> = (1..=INSTRUMENTS).collect();
        if !self.stable {
            for i in (1..slots.len()).rev() {
                let j = self.g.below(i + 1);
                slots.swap(i, j);
            }
        }
        let mut tracks: Vec<TrackProgram> = Vec::new();
        for (i, &slot) in slots.iter().enumerate().take(count) {
            let id = if self.stable {
                i as u64 + 1
            } else {
                self.g.range(1, 6) as u64
            };
            let notes = self.notes();
            let g = &mut self.g;
            let mut t = track(id, slot, notes);
            if !self.stable && g.chance(15) {
                t.chain[0].bypass = true;
            }
            if g.chance(50) {
                let mut fx = effect(10 + slot);
                fx.bypass = g.chance(30);
                t.chain.push(fx);
            }
            t.gain = g.unit() * 1.5;
            t.pan = g.unit() * 2.0 - 1.0;
            t.mute = g.chance(20);
            t.solo = g.chance(15);
            t.arm = g.chance(40);
            t.monitor = g.chance(60);
            t.input.port = g.chance(30).then(|| MidiPort(g.below(3) as u16));
            t.input.channel = g.chance(30).then(|| g.below(16) as u8);
            t.input.port_missing = g.chance(5);
            t.pdc_delay = g.pick(&[0, 0, 0, 17, 1000, MAX_PDC_FRAMES as u32 + 5]);
            tracks.push(t);
        }
        let g = &mut self.g;
        let mut p = program(g.range(40, 240) as f64 + 0.5, tracks);
        p.loop_on = g.chance(50);
        let start = g.pick(&[0, 0, 960, BAR]);
        let length = g.pick(&[0, 60, 240, BAR, 2 * BAR]);
        p.loop_range = TickRange::new(Tick(start), Tick(start + length));
        p.metronome = g.chance(50);
        p.count_in_bars = g.below(3) as u8;
        p.master.gain = g.unit() * 1.5;
        p.master.mute = g.chance(5);
        if g.chance(30) {
            p.master.chain.push(effect(19));
        }
        p
    }

    fn command(&mut self) -> Command {
        let roll = self.g.below(100);
        let track = TrackId(self.g.range(1, 7) as u64);
        match roll {
            0..=9 => Command::Play,
            10..=14 => Command::Stop,
            15..=21 => Command::Locate(Tick(self.g.range(-100, 2 * BAR))),
            22..=26 => Command::Record {
                from: Tick(self.g.range(-10, 4 * BAR)),
            },
            27..=29 => Command::StopRecording,
            30..=41 => Command::SetProgram(Box::new(self.program())),
            42..=45 if !self.stable => {
                let slot = self.g.range(1, INSTRUMENTS as i64) as u32;
                // `insert_sine` sends the insert; a parameter for the new processor follows.
                self.insert_sine(slot);
                Command::SetParam {
                    slot: SlotId(slot),
                    id: 0,
                    value: 0.0,
                }
            }
            46..=48 if !self.stable => {
                let slot = self.g.pick(&[3, 12, 19, 5000]);
                // The log of a removed instrument stays for its last calls.
                Command::RemoveProcessor { slot: SlotId(slot) }
            }
            49..=52 => Command::SetParam {
                slot: SlotId(self.g.range(0, 20) as u32),
                id: self.g.below(20) as u32,
                value: f64::from(self.g.unit()),
            },
            53..=56 => Command::SetTrackGain {
                track,
                gain: self.g.unit() * 1.5,
            },
            57..=59 => Command::SetTrackPan {
                track,
                pan: self.g.unit() * 3.0 - 1.5,
            },
            60..=63 => Command::SetTrackMute {
                track,
                mute: self.g.chance(50),
            },
            64..=67 => Command::SetTrackSolo {
                track,
                solo: self.g.chance(50),
            },
            68..=69 => Command::SetMasterGain(self.g.unit() * 1.5),
            70..=89 => Command::LiveNote {
                track,
                key: self.g.range(40, 130) as u8,
                velocity: self.g.pick(&[0, 0, 1, 64, 127, 200]),
            },
            90..=92 => Command::AllNotesOff,
            _ => Command::Play,
        }
    }

    fn midi(&mut self, frames: usize) -> Vec<MidiInEvent> {
        let g = &mut self.g;
        let mut midi: Vec<MidiInEvent> = (0..g.below(8))
            .map(|_| {
                let channel = g.pick(&[0, 0, 3, 9]) as u8;
                let status = g.pick(&[0x90, 0x90, 0x80, 0xb0, 0xe0, 0xf0]) | channel;
                MidiInEvent {
                    frame: g.below(frames) as u32,
                    port: MidiPort(g.below(3) as u16),
                    data: [status, g.below(128) as u8, g.below(128) as u8],
                }
            })
            .collect();
        midi.sort_by_key(|m| m.frame);
        midi
    }

    fn step(&mut self) {
        for _ in 0..self.g.below(6) {
            let command = self.command();
            if self.rig.handle.send(command).is_err() {
                break;
            }
        }
        if self.g.chance(70) {
            let frames = 1 + self.g.below(1500);
            let midi = self.midi(frames);
            let latency = self.g.pick(&[0, 0, 256, 5000]);
            self.rig.process_with(frames, &midi, latency);
        } else {
            let frames = 1 + self.g.below(2048);
            self.rig.render(frames, frames);
        }
        self.check();
    }

    fn check(&mut self) {
        for (i, x) in self.rig.l.iter().chain(&self.rig.r).enumerate() {
            assert!(
                x.is_finite() && x.abs() < 1000.0,
                "seed {}: {x} at {i}",
                self.seed
            );
        }
        for x in self.rig.l.iter().chain(&self.rig.r) {
            self.digest = (self.digest ^ u64::from(x.to_bits())).wrapping_mul(0x0100_0000_01b3);
        }
        self.rig.l.clear();
        self.rig.r.clear();
        self.rig.events.clear();
        let ctx = format!("seed {}", self.seed);
        for (slot, (log, checker)) in &mut self.logs {
            assert_eq!(log.lost(), 0, "{ctx}: log of slot {slot} overflowed");
            let entries = log.drain();
            checker.check(&entries, self.max_block, &mut self.seen, &ctx);
        }
    }

    /// Runs the sequence; returns the number of note-ons played.
    fn run(self, steps: usize) -> usize {
        self.run_digest(steps).0
    }

    /// Runs the sequence; returns the number of note-ons played and the output's digest.
    fn run_digest(mut self, steps: usize) -> (usize, u64) {
        let first = self.program();
        self.rig.send(Command::SetProgram(Box::new(first)));
        for _ in 0..steps {
            self.step();
        }
        // Wait until all queued commands are applied, then stop.
        for _ in 0..4 {
            self.rig.process_with(1, &[], 0);
        }
        self.rig.send(Command::Stop);
        self.rig.process_with(64, &[], 0);
        self.check();
        if self.stable {
            for (slot, (_, checker)) in &self.logs {
                assert!(
                    checker.open.is_empty(),
                    "seed {}: slot {slot} has hanging notes {:?}",
                    self.seed,
                    checker.open
                );
            }
        }
        (self.seen.len(), self.digest)
    }
}

fn sequences(seeds: std::ops::Range<u32>) {
    let (mut notes, mut stable_notes) = (0, 0);
    for seed in seeds {
        let s = Sequence::new(seed);
        let stable = s.stable;
        let n = s.run(24);
        notes += n;
        if stable {
            stable_notes += n;
        }
    }
    // Not vacuous: plenty of notes, in the stable sequences too.
    assert!(
        notes > 5000 && stable_notes > 2500,
        "{notes} {stable_notes}"
    );
}

#[test]
fn random_command_sequences_0() {
    sequences(0..250);
}

#[test]
fn random_command_sequences_1() {
    sequences(250..500);
}

#[test]
fn random_command_sequences_2() {
    sequences(500..750);
}

#[test]
fn random_command_sequences_3() {
    sequences(750..1000);
}

/// WP-65: the same sequences with 1–3 worker threads give bit-exact the same output (and pass
/// the same checks).
#[test]
fn random_command_sequences_with_workers_are_bit_exact() {
    let (mut batches, mut worker_jobs) = (0, 0);
    for seed in 0..120 {
        let single = Sequence::new(seed).run_digest(24);
        let mut s = Sequence::new(seed);
        s.rig.engine.set_workers(1 + seed as usize % 3, None);
        let stats = s.rig.engine.worker_stats();
        let parallel = s.run_digest(24);
        assert_eq!(single, parallel, "seed {seed}");
        batches += stats.batches();
        worker_jobs += stats.worker_jobs();
    }
    // Not vacuous: the pool ran batches, and workers took jobs.
    assert!(
        batches > 500 && worker_jobs > 100,
        "{batches} {worker_jobs}"
    );
}

#[test]
fn shared_track_programs_survive_random_swaps() {
    // Programs sharing `Arc<TrackProgram>`s, as the compiler builds them.
    let mut s = Sequence::new(4242);
    let base = s.program();
    let shared: Arc<TrackProgram> = base
        .tracks
        .first()
        .cloned()
        .unwrap_or_else(|| Arc::new(track(1, 1, vec![note(0, BAR, 60)])));
    for _ in 0..50 {
        let mut p = s.program();
        if let Some(first) = p.tracks.first_mut() {
            *first = shared.clone();
        }
        s.rig.send(Command::SetProgram(Box::new(p)));
        s.rig.send(Command::Play);
        s.rig.process_with(500, &[], 0);
        s.check();
    }
}
