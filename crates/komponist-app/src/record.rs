//! Recording (WP-51): the take that grows while the engine captures (S02 `notes:ixs`,
//! ARCHITECTURE §6.5, `Prototyp.dc.html` `toggleRec`, `startRec`, `finishRec`).
//!
//! The running take is an overlay of the app ([`Recording`]), not part of the document: nothing is
//! recompiled while recording (monitoring already plays the input live), and other edits, undo and
//! redo stay possible without ending it. The app drives it:
//!
//! 1. R sends `Command::Record` (after arming the selected track if none is armed).
//! 2. `EngineEvent::CountInDone { at }` starts a [`Recording`] ([`Recording::new`]): one lane per
//!    armed track, the clip starting at `at` floored to the beat (`recAt`). The dock's editor shows
//!    the first lane's take ("● Aufnahme", "Piano Roll · live").
//! 3. `RecordedNoteOn` / `RecordedNoteOff` add notes to the lane's current lap, `LoopWrapped`
//!    closes the lap at the loop end and starts the next one ([`Recording::handle`]). A note-on
//!    whose tick lies after the playhead arrived late from before the wrap (latency correction):
//!    it belongs to the previous lap and ends at that lap's end at the latest.
//! 4. Every frame [`Recording::update`] moves the head to the playhead and rebuilds the clips the
//!    arrangement and the piano roll draw ([`Recording::clip`]): the clip grows with the head,
//!    held notes reach it.
//! 5. Stop (Leertaste, Stop, R) commits the take as **one** undo step ([`Recording::commit`],
//!    [`Commit::apply`]): a clip per lane whose track still exists, with one take per lap that has
//!    notes (a lane without any note keeps one empty take), named "Take n" with the numbers of
//!    [`edits::next_take_number`] counted lap by lap and lane by lane. The last take is active and
//!    names the clip (what plays is what the clip says), its length is whole bars (`finishRec`:
//!    `Math.ceil(bars − 0.05)`, at least one). Esc throws the overlay away while playback goes on.

use std::sync::Arc;

use komponist_engine::EngineEvent;
use komponist_model::edits::{self, EditError, NewClip, NewClipContent, NewNote};
use komponist_model::{
    Clip, ClipContent, ClipId, MidiClip, Note, NoteId, Project, Take, Tick, TrackId,
};

/// German texts of the recording (`Screens.dc.html` S02, `Prototyp.dc.html`).
pub mod names {
    /// Title of the dock's editor tab while recording: `editorTabs('● Aufnahme', …)` without the
    /// ●, which the fonts lack; the tab's `state.record` mark stands in for it.
    pub const TAB: &str = "Aufnahme";
    /// Its secondary text.
    pub const TAB_KIND: &str = "Piano Roll · live";
    /// The piano roll's selection info while recording (`selT:'live'`).
    pub const LIVE: &str = "live";
    /// R without an armed or selected track (`toggleRec`).
    pub const NO_TRACK: &str = "Keine Spur scharfgeschaltet · R im Spur-Header";

    /// "Take 3".
    pub fn take(number: u32) -> String {
        format!("Take {number}")
    }

    /// The clip being recorded: "● Aufnahme · Take 1" (S02 `mc(…, '● Aufnahme · Take 1', …)`).
    pub fn clip(take: &str) -> String {
        format!("● Aufnahme · {take}")
    }

    /// The arrangement's info while recording: "Aufnahme · Lead" (S02 `info`).
    pub fn info(tracks: &[&str]) -> String {
        format!("Aufnahme · {}", tracks.join(", "))
    }

    /// Status after Stop: "Take 1 aufgenommen · 14 Noten · Q quantisiert" (`finishRec`; one note:
    /// "1 Note").
    pub fn recorded(take: &str, notes: usize) -> String {
        let notes = if notes == 1 {
            "1 Note".to_owned()
        } else {
            format!("{notes} Noten")
        };
        format!("{take} aufgenommen · {notes} · Q quantisiert")
    }
}

/// `finishRec`: a take shorter than a whole bar by less than this share of a bar does not get
/// another bar (`Math.ceil((pos − recAt)/16 − 0.05)`).
const BAR_TOLERANCE: f64 = 0.05;

/// Id of the drawn clip of lane `index` (not in the document; never sent in an edit).
fn live_clip_id(index: usize) -> ClipId {
    ClipId(u64::MAX - index as u64)
}

/// A captured note, absolute song ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Captured {
    start: Tick,
    /// `None` while the key is down.
    end: Option<Tick>,
    pitch: u8,
    velocity: u8,
}

/// One pass of the loop (or the whole recording without loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lap {
    /// The loop end the playhead wrapped at; `None` for the running lap.
    end: Option<Tick>,
}

/// A key that is down: lap and index of its note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    key: u8,
    lap: usize,
    index: usize,
}

/// What one armed track records.
#[derive(Debug, Clone)]
struct Lane {
    track: TrackId,
    /// Notes per lap, in arrival order.
    laps: Vec<Vec<Captured>>,
    open: Vec<Open>,
    /// The clip the views draw ([`Recording::update`]).
    live: Arc<Clip>,
}

/// The running recording (see the module docs).
#[derive(Debug, Clone)]
pub struct Recording {
    /// Clip start: the capture start floored to the beat.
    start: Tick,
    /// The number the first take gets ([`edits::next_take_number`] at the start; display only,
    /// the commit counts again).
    base: u32,
    /// Ticks per bar and beat of the project at the start.
    bar: i64,
    lanes: Vec<Lane>,
    laps: Vec<Lap>,
    /// The playhead of the last [`Recording::update`].
    head: Tick,
}

impl Recording {
    /// A recording that captures from `at` on the armed tracks of `project`.
    pub fn new(project: &Project, at: Tick) -> Recording {
        let at = at.max(Tick::ZERO);
        let beat = project.meter.ticks_per_beat().max(1);
        let start = Tick(at.0 - at.0.rem_euclid(beat));
        let mut rec = Recording {
            start,
            base: edits::next_take_number(project),
            bar: project.meter.ticks_per_bar().max(1),
            lanes: Vec::new(),
            laps: vec![Lap { end: None }],
            head: at,
        };
        for track in project.tracks.iter().filter(|t| t.arm) {
            rec.lane_mut(track.id);
        }
        rec.update(at);
        rec
    }

    /// Where the clip starts.
    pub fn start(&self) -> Tick {
        self.start
    }

    /// The tracks that record, in the order they joined (track order at the start).
    pub fn tracks(&self) -> impl Iterator<Item = TrackId> + '_ {
        self.lanes.iter().map(|l| l.track)
    }

    /// Number of laps so far (1 + loop wraps).
    pub fn laps(&self) -> usize {
        self.laps.len()
    }

    /// The clip being recorded on `track` as the views draw it: named "● Aufnahme · Take n", from
    /// the start to the head (and to the loop end once the loop wrapped), with the notes of the
    /// running lap; held notes reach the head. Its id is not in the document.
    pub fn clip(&self, track: TrackId) -> Option<&Arc<Clip>> {
        self.lanes
            .iter()
            .find(|l| l.track == track)
            .map(|l| &l.live)
    }

    /// The first lane's track and clip: what the editor shows.
    pub fn first(&self) -> Option<(TrackId, &Arc<Clip>)> {
        self.lanes.first().map(|l| (l.track, &l.live))
    }

    /// The name the running take of `track` will get ("Take 1"), if it records.
    pub fn take_name(&self, track: TrackId) -> Option<String> {
        let lane = self.lanes.iter().position(|l| l.track == track)?;
        let numbers = self.numbers(self.base, true);
        let lap = self.laps.len() - 1;
        numbers[lane][lap].map(names::take)
    }

    /// Handles a captured note or a loop wrap. `playhead` is the engine's position after the event
    /// arrived; `project` gives the loop end.
    pub fn handle(&mut self, event: &EngineEvent, playhead: Tick, project: &Project) {
        match *event {
            EngineEvent::RecordedNoteOn {
                track,
                tick,
                key,
                velocity,
            } => {
                let current = self.laps.len() - 1;
                // Heard before the wrap, arrived after it.
                let lap = if current > 0 && tick > playhead {
                    current - 1
                } else {
                    current
                };
                let lane = self.lane_mut(track);
                let notes = &mut lane.laps[lap];
                notes.push(Captured {
                    start: tick,
                    end: None,
                    pitch: key,
                    velocity,
                });
                let index = notes.len() - 1;
                lane.open.push(Open { key, lap, index });
            }
            EngineEvent::RecordedNoteOff { track, tick, key } => {
                let ends: Vec<Option<Tick>> = self.laps.iter().map(|l| l.end).collect();
                let Some(lane) = self.lanes.iter_mut().find(|l| l.track == track) else {
                    return;
                };
                // Pairs with the latest open note-on of the key.
                let Some(i) = lane.open.iter().rposition(|o| o.key == key) else {
                    return;
                };
                let open = lane.open.remove(i);
                let note = &mut lane.laps[open.lap][open.index];
                note.end = Some(close(note.start, tick, ends[open.lap]));
            }
            EngineEvent::LoopWrapped { .. } => {
                let end = project.loop_range.end;
                let lap = self.laps.len() - 1;
                self.laps[lap].end = Some(end);
                // The engine ends held keys at the loop end before it wraps; anything still open
                // ends there too.
                for lane in &mut self.lanes {
                    for open in lane.open.extract_if(.., |o| o.lap == lap) {
                        let note = &mut lane.laps[lap][open.index];
                        note.end = Some(close(note.start, end, Some(end)));
                    }
                }
                self.laps.push(Lap { end: None });
                for lane in &mut self.lanes {
                    lane.laps.push(Vec::new());
                }
            }
            _ => {}
        }
    }

    /// Moves the head to `playhead` and rebuilds the drawn clips (once per frame).
    pub fn update(&mut self, playhead: Tick) {
        self.head = playhead;
        let end = self.extent_end(playhead).max(playhead + Tick(1));
        let lap = self.laps.len() - 1;
        let numbers = self.numbers(self.base, true);
        for (index, lane) in self.lanes.iter_mut().enumerate() {
            let take = numbers[index][lap].map(names::take).unwrap_or_default();
            let mut notes: Vec<Note> = lane.laps[lap]
                .iter()
                .enumerate()
                .filter_map(|(i, n)| {
                    // A held note reaches the head (inclusive, so it lights up as playing).
                    let note_end = n.end.unwrap_or(playhead + Tick(1));
                    local_note(NoteId(i as u64), n, note_end, self.start)
                })
                .collect();
            notes.sort_by_key(|n| (n.start, n.pitch, n.id));
            lane.live = Arc::new(Clip {
                id: live_clip_id(index),
                name: names::clip(&take),
                start: self.start,
                length: (end - self.start).max(Tick(1)),
                loop_length: None,
                mute: false,
                transpose: 0,
                velocity_pct: 0,
                content: ClipContent::Midi(MidiClip {
                    takes: vec![Take {
                        name: take,
                        notes: Arc::new(notes),
                    }],
                    active_take: 0,
                }),
            });
        }
    }

    /// What Stop writes into `project`, with the head at `head`: `None` if no lane's track is
    /// left (or nothing records).
    pub fn commit(&self, project: &Project, head: Tick) -> Option<Commit> {
        let numbers = self.numbers(edits::next_take_number(project), false);
        let end = self.extent_end(head).max(head);
        let mut clips = Vec::new();
        for (lane, numbers) in self.lanes.iter().zip(&numbers) {
            if project.track(lane.track).is_none() {
                continue;
            }
            let mut takes = Vec::new();
            let mut last = self.start;
            for (lap, number) in numbers.iter().enumerate() {
                let Some(number) = number else {
                    continue;
                };
                let lap_end = self.laps[lap].end;
                let notes: Vec<NewNote> = lane.laps[lap]
                    .iter()
                    .filter_map(|n| {
                        let note_end = n.end.unwrap_or_else(|| close(n.start, head, lap_end));
                        last = last.max(note_end);
                        let note = local_note(NoteId(0), n, note_end, self.start)?;
                        Some(NewNote {
                            start: note.start,
                            length: note.length,
                            pitch: note.pitch,
                            velocity: note.velocity,
                        })
                    })
                    .collect();
                takes.push((names::take(*number), notes));
            }
            let span = (end.max(last) - self.start).0.max(0) as f64 / self.bar as f64;
            let bars = (span - BAR_TOLERANCE).ceil().max(1.0) as i64;
            clips.push(CommitClip {
                track: lane.track,
                start: self.start,
                length: Tick(bars * self.bar),
                takes,
            });
        }
        let label = clips.first()?.takes.last()?.0.clone();
        Some(Commit { label, clips })
    }

    /// The lane of `track`, created (with empty laps) if it is new: a track armed while recording
    /// joins when its first note arrives.
    fn lane_mut(&mut self, track: TrackId) -> &mut Lane {
        let index = match self.lanes.iter().position(|l| l.track == track) {
            Some(i) => i,
            None => {
                self.lanes.push(Lane {
                    track,
                    laps: vec![Vec::new(); self.laps.len()],
                    open: Vec::new(),
                    live: Arc::new(placeholder_clip(live_clip_id(self.lanes.len()))),
                });
                self.lanes.len() - 1
            }
        };
        &mut self.lanes[index]
    }

    /// The end of the recorded span: the head, or the loop end once the loop wrapped.
    fn extent_end(&self, head: Tick) -> Tick {
        self.laps.iter().filter_map(|l| l.end).fold(head, Tick::max)
    }

    /// Take numbers per lane and lap, counted from `base` lap by lap, lane by lane: a lap gets a
    /// take if it has notes (or, with `running`, if it is the running lap); a lane without any
    /// note keeps its first lap.
    fn numbers(&self, base: u32, running: bool) -> Vec<Vec<Option<u32>>> {
        let current = self.laps.len() - 1;
        let kept = |lane: &Lane, lap: usize| {
            !lane.laps[lap].is_empty()
                || (running && lap == current)
                || (lap == 0 && lane.laps.iter().all(Vec::is_empty))
        };
        let mut numbers = vec![vec![None; self.laps.len()]; self.lanes.len()];
        let mut next = base;
        for lap in 0..self.laps.len() {
            for (lane, slots) in self.lanes.iter().zip(numbers.iter_mut()) {
                if kept(lane, lap) {
                    slots[lap] = Some(next);
                    next = next.saturating_add(1);
                }
            }
        }
        numbers
    }
}

/// The takes Stop writes: one undo step ([`Commit::apply`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    label: String,
    clips: Vec<CommitClip>,
}

#[derive(Debug, Clone, PartialEq)]
struct CommitClip {
    track: TrackId,
    start: Tick,
    length: Tick,
    /// Name and clip-local notes of each take, oldest first.
    takes: Vec<(String, Vec<NewNote>)>,
}

impl Commit {
    /// The undo label: the name of the first clip ("Take 1"), its last take.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Name and note count of the first clip's last (active) take, for the status message.
    pub fn summary(&self) -> Option<(&str, usize)> {
        let (name, notes) = self.clips.first()?.takes.last()?;
        Some((name, notes.len()))
    }

    /// Adds the clips and their takes to `project`; returns the new clips in lane order.
    pub fn apply(&self, project: &Project) -> Result<(Project, Vec<ClipId>), EditError> {
        let mut p = project.clone();
        let mut ids = Vec::with_capacity(self.clips.len());
        for clip in &self.clips {
            let Some((last, _)) = clip.takes.last() else {
                continue;
            };
            let mut takes = clip.takes.iter();
            let Some((first, notes)) = takes.next() else {
                continue;
            };
            let spec = NewClip {
                start: clip.start,
                length: clip.length,
                name: last.clone(),
                content: NewClipContent::Midi {
                    notes: notes.clone(),
                    take_name: first.clone(),
                },
            };
            let (next, id) = edits::add_clip(&p, clip.track, spec)?;
            p = next;
            for (name, notes) in takes {
                let (next, _) = edits::add_take(&p, id, name.clone())?;
                let (next, _) = edits::add_notes(&next, id, notes)?;
                p = next;
            }
            ids.push(id);
        }
        Ok((p, ids))
    }
}

/// The end of a note started at `start` and released at `tick`: at least one tick long, and not
/// after `lap_end` (a closed lap). A release before the start (it came in the next lap) ends the
/// note at the lap end.
fn close(start: Tick, tick: Tick, lap_end: Option<Tick>) -> Tick {
    let end = match lap_end {
        Some(lap_end) if tick <= start => lap_end,
        Some(lap_end) => tick.min(lap_end),
        None => tick,
    };
    end.max(start + Tick(1))
}

/// `n` ending at `end` as a note of a clip starting at `clip_start`: cut at the clip start, gone
/// if it ended before (the playhead was moved back while recording).
fn local_note(id: NoteId, n: &Captured, end: Tick, clip_start: Tick) -> Option<Note> {
    if end <= clip_start {
        return None;
    }
    let start = n.start.max(clip_start);
    Some(Note {
        id,
        start: start - clip_start,
        length: (end - start).max(Tick(1)),
        pitch: n.pitch.min(127),
        velocity: n.velocity.clamp(1, 127),
    })
}

/// An empty clip until the first [`Recording::update`].
fn placeholder_clip(id: ClipId) -> Clip {
    Clip {
        id,
        name: String::new(),
        start: Tick::ZERO,
        length: Tick(1),
        loop_length: None,
        mute: false,
        transpose: 0,
        velocity_pct: 0,
        content: ClipContent::Midi(MidiClip {
            takes: vec![Take {
                name: String::new(),
                notes: Arc::new(Vec::new()),
            }],
            active_take: 0,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::TickRange;
    use komponist_model::edits::NewTrack;
    use komponist_model::fixtures::empty_project;

    const BAR: i64 = 3840;

    /// An empty project with two tracks, the first armed; returns the project and both ids.
    fn project() -> (Project, TrackId, TrackId) {
        let p = empty_project();
        let spec = |arm| NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: arm,
        };
        let (p, a) = edits::add_track(&p, spec(true)).unwrap();
        let (p, b) = edits::add_track(&p, spec(false)).unwrap();
        (p, a, b)
    }

    fn on(track: TrackId, tick: i64, key: u8) -> EngineEvent {
        EngineEvent::RecordedNoteOn {
            track,
            tick: Tick(tick),
            key,
            velocity: 90,
        }
    }

    fn off(track: TrackId, tick: i64, key: u8) -> EngineEvent {
        EngineEvent::RecordedNoteOff {
            track,
            tick: Tick(tick),
            key,
        }
    }

    fn wrap() -> EngineEvent {
        EngineEvent::LoopWrapped { to: Tick::ZERO }
    }

    /// The notes of `p`'s clip `id`, take `take`: (start, length, pitch).
    fn notes(p: &Project, id: ClipId, take: usize) -> Vec<(i64, i64, u8)> {
        let (_, clip) = p.find_clip(id).unwrap();
        let ClipContent::Midi(m) = &clip.content else {
            panic!("midi")
        };
        m.takes[take]
            .notes
            .iter()
            .map(|n| (n.start.0, n.length.0, n.pitch))
            .collect()
    }

    #[test]
    fn the_clip_starts_on_the_beat_and_grows_with_the_head() {
        let (p, a, b) = project();
        let mut r = Recording::new(&p, Tick(BAR + 1000));
        assert_eq!(r.start(), Tick(BAR + 960));
        assert_eq!(r.tracks().collect::<Vec<_>>(), [a]);
        assert!(r.clip(b).is_none());
        r.handle(&on(a, BAR + 1100, 60), Tick(BAR + 1100), &p);
        r.update(Tick(BAR + 1500));
        let clip = r.clip(a).unwrap();
        assert_eq!(clip.name, "● Aufnahme · Take 1");
        assert_eq!(clip.start, Tick(BAR + 960));
        assert_eq!(clip.end(), Tick(BAR + 1501));
        let ClipContent::Midi(m) = &clip.content else {
            panic!()
        };
        // The held note reaches the head.
        let n = m.active().notes[0];
        assert_eq!((n.start, n.length), (Tick(140), Tick(401)));
        assert_eq!(r.take_name(a).as_deref(), Some("Take 1"));
    }

    #[test]
    fn stop_writes_one_take_with_whole_bars() {
        let (p, a, _) = project();
        let mut r = Recording::new(&p, Tick(BAR));
        r.handle(&on(a, BAR + 100, 60), Tick(BAR + 100), &p);
        r.handle(&off(a, BAR + 580, 60), Tick(BAR + 580), &p);
        r.handle(&on(a, BAR + 960, 64), Tick(BAR + 960), &p);
        // Stopped 1.04 bars in: still one bar; the held note ends at the head.
        let head = Tick(BAR + BAR + 150);
        let commit = r.commit(&p, head).unwrap();
        assert_eq!(commit.label(), "Take 1");
        assert_eq!(commit.summary(), Some(("Take 1", 2)));
        let (q, ids) = commit.apply(&p).unwrap();
        let (_, clip) = q.find_clip(ids[0]).unwrap();
        assert_eq!((clip.start, clip.length), (Tick(BAR), Tick(BAR)));
        assert_eq!(clip.name, "Take 1");
        assert_eq!(notes(&q, ids[0], 0), [(100, 480, 60), (960, 3030, 64)]);
        // 1.1 bars: two.
        let commit = r.commit(&p, Tick(2 * BAR + 384)).unwrap();
        let (q, ids) = commit.apply(&p).unwrap();
        assert_eq!(q.find_clip(ids[0]).unwrap().1.length, Tick(2 * BAR));
        // Numbers continue after existing takes.
        let again = r.commit(&q, head).unwrap();
        assert_eq!(again.label(), "Take 2");
    }

    #[test]
    fn each_lap_is_a_take_and_late_notes_belong_to_their_lap() {
        let (mut p, a, _) = project();
        p.loop_on = true;
        p.loop_range = TickRange::new(Tick::ZERO, Tick(BAR));
        let mut r = Recording::new(&p, Tick::ZERO);
        r.handle(&on(a, 480, 60), Tick(480), &p);
        r.handle(&off(a, 960, 60), Tick(960), &p);
        // Held across the wrap: the engine ends it at the loop end.
        r.handle(&on(a, 3000, 62), Tick(3000), &p);
        r.handle(&off(a, BAR, 62), Tick(BAR - 10), &p);
        r.handle(&wrap(), Tick(20), &p);
        // Heard at 3824 before the wrap, arrived after it; released in the new lap.
        r.handle(&on(a, 3824, 67), Tick(40), &p);
        r.handle(&on(a, 1440, 64), Tick(1440), &p);
        r.handle(&off(a, 100, 67), Tick(1450), &p);
        r.handle(&off(a, 1920, 64), Tick(1920), &p);
        assert_eq!(r.laps(), 2);
        r.update(Tick(2000));
        // The drawn clip spans the loop and shows the running lap.
        let live = r.clip(a).unwrap();
        assert_eq!(live.end(), Tick(BAR));
        assert_eq!(live.name, "● Aufnahme · Take 2");
        let commit = r.commit(&p, Tick(2000)).unwrap();
        let (q, ids) = commit.apply(&p).unwrap();
        let (_, clip) = q.find_clip(ids[0]).unwrap();
        let ClipContent::Midi(m) = &clip.content else {
            panic!()
        };
        let names: Vec<&str> = m.takes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Take 1", "Take 2"]);
        assert_eq!(m.active_take, 1);
        assert_eq!((clip.name.as_str(), clip.length), ("Take 2", Tick(BAR)));
        assert_eq!(commit.label(), "Take 2");
        assert_eq!(
            notes(&q, ids[0], 0),
            [(480, 480, 60), (3000, 840, 62), (3824, 16, 67)]
        );
        assert_eq!(notes(&q, ids[0], 1), [(1440, 480, 64)]);
        assert_eq!(commit.summary(), Some(("Take 2", 1)));
    }

    #[test]
    fn empty_laps_are_dropped_and_an_empty_lane_keeps_one_take() {
        let (mut p, a, b) = project();
        p.loop_on = true;
        p.loop_range = TickRange::new(Tick::ZERO, Tick(BAR));
        p = edits::set_track_prop(&p, &[b], edits::TrackProp::Arm(true)).unwrap();
        let mut r = Recording::new(&p, Tick::ZERO);
        assert_eq!(r.tracks().collect::<Vec<_>>(), [a, b]);
        // Lap 1: only a; lap 2: nothing; lap 3: a again.
        r.handle(&on(a, 100, 60), Tick(100), &p);
        r.handle(&off(a, 200, 60), Tick(200), &p);
        r.handle(&wrap(), Tick(10), &p);
        r.update(Tick(10));
        // While running, the running lap counts: a → Take 3, b → Take 4.
        assert_eq!(r.take_name(a).as_deref(), Some("Take 3"));
        assert_eq!(r.take_name(b).as_deref(), Some("Take 4"));
        r.handle(&wrap(), Tick(10), &p);
        r.handle(&on(a, 300, 61), Tick(300), &p);
        r.handle(&off(a, 400, 61), Tick(400), &p);
        let commit = r.commit(&p, Tick(500)).unwrap();
        let (q, ids) = commit.apply(&p).unwrap();
        assert_eq!(ids.len(), 2);
        let takes = |id| {
            let (_, c) = q.find_clip(id).unwrap();
            let ClipContent::Midi(m) = &c.content else {
                panic!()
            };
            m.takes.iter().map(|t| t.name.clone()).collect::<Vec<_>>()
        };
        // Lap by lap, lane by lane: a lap 1 → 1, b (empty, kept once) → 2, a lap 3 → 3.
        assert_eq!(takes(ids[0]), ["Take 1", "Take 3"]);
        assert_eq!(takes(ids[1]), ["Take 2"]);
        assert!(notes(&q, ids[1], 0).is_empty());
    }

    #[test]
    fn deleted_tracks_drop_their_take_and_new_ones_join() {
        let (p, a, b) = project();
        let mut r = Recording::new(&p, Tick::ZERO);
        // b was armed meanwhile; its first note brings it in.
        r.handle(&on(b, 100, 60), Tick(100), &p);
        r.handle(&off(b, 200, 60), Tick(200), &p);
        assert_eq!(r.tracks().collect::<Vec<_>>(), [a, b]);
        let without_a = edits::remove_tracks(&p, &[a]).unwrap();
        let commit = r.commit(&without_a, Tick(300)).unwrap();
        let (q, ids) = commit.apply(&without_a).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(q.find_clip(ids[0]).unwrap().0.id, b);
        let gone = edits::remove_tracks(&without_a, &[b]).unwrap();
        assert!(r.commit(&gone, Tick(300)).is_none());
    }

    #[test]
    fn notes_before_the_clip_start_are_cut_or_dropped() {
        let (p, a, _) = project();
        let mut r = Recording::new(&p, Tick(BAR));
        // Latency correction put it 20 ticks before the capture start.
        r.handle(&on(a, BAR - 20, 60), Tick(BAR), &p);
        r.handle(&off(a, BAR + 100, 60), Tick(BAR + 100), &p);
        // Played after a jump back before the clip: gone.
        r.handle(&on(a, 100, 62), Tick(100), &p);
        r.handle(&off(a, 200, 62), Tick(200), &p);
        let (q, ids) = r.commit(&p, Tick(BAR + 200)).unwrap().apply(&p).unwrap();
        assert_eq!(notes(&q, ids[0], 0), [(0, 100, 60)]);
    }
}
