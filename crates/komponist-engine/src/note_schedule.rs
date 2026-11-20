//! A track's sorted notes and compact placements for large MIDI clip loops.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;
use std::sync::Arc;

use komponist_model::Tick;

use crate::program::SeqNote;

/// A sequence sorted by timeline tick and MIDI key, with clip order breaking ties.
#[derive(Debug, Default)]
pub struct NoteSchedule {
    flat: Box<[SeqNote]>,
    // Only needed when we merge the flat notes with repeating clips.
    origins: Box<[usize]>,
    repeating: Box<[RepeatedMidi]>,
}

impl NoteSchedule {
    /// Wraps already sorted timeline notes.
    pub fn from_sorted(notes: Vec<SeqNote>) -> Self {
        Self {
            flat: notes.into_boxed_slice(),
            ..Self::default()
        }
    }

    pub(crate) fn compile(
        mut flat: Vec<(SeqNote, usize)>,
        mut repeating: Vec<RepeatedMidi>,
    ) -> Self {
        flat.sort_by_key(|(note, _)| (note.start, note.key));
        if repeating.is_empty() {
            return Self::from_sorted(flat.into_iter().map(|(note, _)| note).collect());
        }
        let (flat, origins): (Vec<_>, Vec<_>) = flat.into_iter().unzip();
        repeating.sort_unstable_by_key(|clip| clip.start);
        index_ends(&mut repeating);
        Self {
            flat: flat.into_boxed_slice(),
            origins: origins.into_boxed_slice(),
            repeating: repeating.into_boxed_slice(),
        }
    }

    /// The number of scheduled note starts, including repetitions.
    pub fn len(&self) -> usize {
        self.repeating
            .iter()
            .fold(self.flat.len(), |len, clip| len.saturating_add(clip.len()))
    }

    /// Whether the schedule has any notes.
    pub fn is_empty(&self) -> bool {
        self.flat.is_empty() && self.repeating.is_empty()
    }

    /// Number of notes stored, including each repeating clip's template just once.
    pub fn stored_notes(&self) -> usize {
        self.flat.len()
            + self
                .repeating
                .iter()
                .map(|clip| clip.notes.len())
                .sum::<usize>()
    }

    /// Visits the sequence in playback order, expanding repetitions on demand.
    pub fn iter(&self) -> impl Iterator<Item = SeqNote> + '_ {
        self.view()
            .merge(0..self.flat.len(), Tick::ZERO..Tick(i64::MAX), None, None)
    }

    /// Trims notes at the export range's end. Large loops stay compact.
    pub(crate) fn trimmed(&self, end: Tick) -> Option<Self> {
        let inside = |note: &SeqNote| note.start.0.saturating_add(note.length.0) <= end.0;
        if self.flat.iter().all(inside) && self.repeating.iter().all(|clip| clip.end <= end) {
            return None;
        }
        let keep = self.flat.partition_point(|note| note.start < end);
        let flat = self.flat[..keep]
            .iter()
            .map(|note| SeqNote {
                length: Tick(note.length.0.min(end.0 - note.start.0)),
                ..*note
            })
            .collect();
        let mut repeating: Vec<_> = self
            .repeating
            .iter()
            .filter_map(|clip| {
                let mut clip = clip.clone();
                clip.end = clip.end.min(end);
                (clip.len() > 0).then_some(clip)
            })
            .collect();
        index_ends(&mut repeating);
        Some(Self {
            flat,
            origins: self.origins[..keep.min(self.origins.len())].into(),
            repeating: repeating.into_boxed_slice(),
        })
    }

    pub(crate) fn view(&self) -> NoteView<'_> {
        NoteView {
            flat: &self.flat,
            origins: &self.origins,
            repeating: &self.repeating,
        }
    }
}

fn index_ends(clips: &mut [RepeatedMidi]) {
    let mut latest = Tick(i64::MIN);
    for clip in clips {
        latest = latest.max(clip.end);
        clip.latest_end = latest;
    }
}

/// A MIDI clip that stores only the notes of its active take's first period.
#[derive(Debug, Clone)]
pub(crate) struct RepeatedMidi {
    start: Tick,
    end: Tick,
    // Maximum end of this or any earlier clip. It bounds the overlapping clip search.
    latest_end: Tick,
    period: i64,
    origin: usize,
    notes: Arc<[SeqNote]>,
}

impl RepeatedMidi {
    pub(crate) fn new(
        start: Tick,
        length: Tick,
        period: Tick,
        origin: usize,
        mut notes: Vec<SeqNote>,
    ) -> Self {
        notes.sort_by_key(|n| (n.start, n.key));
        Self {
            start,
            end: Tick(start.0.saturating_add(length.0)),
            latest_end: Tick::ZERO,
            period: period.0,
            origin,
            notes: notes.into(),
        }
    }

    /// How many notes precede this tick among all repetitions, including negative starts.
    fn count_before(&self, tick: Tick) -> u128 {
        let duration = tick.min(self.end).0.saturating_sub(self.start.0).max(0);
        let full = duration / self.period;
        let rem = duration % self.period;
        full as u128 * self.notes.len() as u128
            + self.notes.partition_point(|note| note.start.0 < rem) as u128
    }

    pub(crate) fn len(&self) -> usize {
        usize::try_from(self.count_before(self.end) - self.count_before(Tick::ZERO))
            .unwrap_or(usize::MAX)
    }

    fn repeat_at(&self, tick: Tick) -> Tick {
        let elapsed = tick.0.saturating_sub(self.start.0).max(0);
        Tick(self.start.0 + elapsed / self.period * self.period)
    }

    /// The next instance in `ticks` after the previously returned repeating note, if any.
    fn candidate(
        &self,
        ticks: &Range<Tick>,
        after: Option<NoteKey>,
        chase: Option<Tick>,
    ) -> Option<NoteKey> {
        let end = self.end.min(ticks.end);
        let mut first = ticks.start.max(after.map_or(ticks.start, |key| key.start));
        if let Some(floor) = chase {
            // A note cannot carry across the end of a period.
            if self.end <= floor {
                return None;
            }
            first = first.max(self.repeat_at(floor));
        }
        if first >= end || self.start >= end {
            return None;
        }
        let mut repeat = self.repeat_at(first);
        let since = first.0.saturating_sub(repeat.0);
        let index = if let Some(last) = after.filter(|key| key.start == first) {
            if last.origin == self.origin {
                last.index + 1
            } else {
                self.notes.partition_point(|note| {
                    (note.start.0, note.key, self.origin) <= (since, last.key, last.origin)
                })
            }
        } else {
            self.notes.partition_point(|note| note.start.0 < since)
        };
        let index = if index == self.notes.len() {
            repeat = Tick(repeat.0.saturating_add(self.period));
            0
        } else {
            index
        };
        let note = self.notes.get(index)?;
        let start = Tick(repeat.0.saturating_add(note.start.0));
        if start >= end {
            return None;
        }
        Some(NoteKey {
            start,
            key: note.key,
            origin: self.origin,
            index,
        })
    }

    fn placed(&self, key: NoteKey) -> SeqNote {
        let note = self.notes[key.index];
        SeqNote {
            start: key.start,
            length: Tick(note.length.0.min(self.end.0 - key.start.0)),
            ..note
        }
    }

    pub(crate) fn flatten(&self, out: &mut Vec<(SeqNote, usize)>) {
        let ticks = Tick::ZERO..self.end;
        let mut after = None;
        while let Some(key) = self.candidate(&ticks, after, None) {
            after = Some(key);
            out.push((self.placed(key), self.origin));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
struct NoteKey {
    start: Tick,
    key: u8,
    origin: usize,
    index: usize,
}

/// Selection scratch shared by tracks on the thread that sequences the segment.
pub(crate) struct RepeatScratch {
    heap: BinaryHeap<Reverse<(NoteKey, usize)>>,
    #[cfg(test)]
    examined: usize,
}

impl RepeatScratch {
    pub(crate) fn new() -> Self {
        Self {
            heap: BinaryHeap::with_capacity(crate::MAX_EVENTS_PER_BLOCK),
            #[cfg(test)]
            examined: 0,
        }
    }

    fn candidate(
        &mut self,
        clip: &RepeatedMidi,
        ticks: &Range<Tick>,
        after: Option<NoteKey>,
        chase: Option<Tick>,
    ) -> Option<NoteKey> {
        #[cfg(test)]
        {
            self.examined += 1;
        }
        clip.candidate(ticks, after, chase)
    }

    /// RT. Uses the heap if its preallocated capacity can hold every current candidate.
    fn start(&mut self, clips: &[RepeatedMidi], ticks: &Range<Tick>, chase: Option<Tick>) -> bool {
        self.heap.clear();
        #[cfg(test)]
        {
            self.examined = 0;
        }
        for (index, clip) in clips.iter().enumerate() {
            if let Some(key) = self.candidate(clip, ticks, None, chase) {
                if self.heap.len() == self.heap.capacity() {
                    return false;
                }
                self.heap.push(Reverse((key, index)));
            }
        }
        true
    }

    fn first(&self) -> Option<(NoteKey, usize)> {
        self.heap.peek().map(|&Reverse(candidate)| candidate)
    }

    fn advance(&mut self, clips: &[RepeatedMidi], ticks: &Range<Tick>, chase: Option<Tick>) {
        let Some(Reverse((last, index))) = self.heap.pop() else {
            return;
        };
        if let Some(next) = self.candidate(&clips[index], ticks, Some(last), chase) {
            self.heap.push(Reverse((next, index)));
        }
    }
}

/// Borrowed input to the sequencer. The ordinary path still uses its sorted note slice.
#[derive(Clone, Copy)]
pub(crate) struct NoteView<'a> {
    pub(crate) flat: &'a [SeqNote],
    origins: &'a [usize],
    repeating: &'a [RepeatedMidi],
}

impl<'a> NoteView<'a> {
    #[cfg(test)]
    pub(crate) fn from_slice(flat: &'a [SeqNote]) -> Self {
        Self {
            flat,
            origins: &[],
            repeating: &[],
        }
    }

    pub(crate) fn has_repeats(self) -> bool {
        !self.repeating.is_empty()
    }

    fn next_repeat(
        clips: &[RepeatedMidi],
        ticks: &Range<Tick>,
        after: Option<NoteKey>,
        chase: Option<Tick>,
    ) -> Option<(NoteKey, usize)> {
        clips
            .iter()
            .enumerate()
            .filter_map(|(index, clip)| clip.candidate(ticks, after, chase).map(|key| (key, index)))
            .min()
    }

    pub(crate) fn merge<'scratch>(
        self,
        mut flat: Range<usize>,
        ticks: Range<Tick>,
        chase: Option<Tick>,
        scratch: Option<&'scratch mut RepeatScratch>,
    ) -> impl Iterator<Item = SeqNote> + 'scratch
    where
        'a: 'scratch,
    {
        let first = chase.map_or(ticks.start, |floor| floor.max(ticks.start));
        let from = self
            .repeating
            .partition_point(|clip| clip.latest_end <= first);
        let to = self
            .repeating
            .partition_point(|clip| clip.start < ticks.end);
        let clips = self.repeating.get(from..to).unwrap_or(&[]);
        // Many overlapping clips are uncommon. The heap advances just the selected clip.
        // If the heap fills, scanning still handles every supported project in exact order.
        let mut scratch = scratch.and_then(|scratch| {
            (!clips.is_empty() && scratch.start(clips, &ticks, chase)).then_some(scratch)
        });
        let mut repeating = match &scratch {
            Some(scratch) => scratch.first(),
            None => Self::next_repeat(clips, &ticks, None, chase),
        };
        std::iter::from_fn(move || {
            let regular = (!flat.is_empty()).then(|| self.flat[flat.start]);
            if let Some((key, index)) = repeating
                && regular.is_none_or(|flat_note| {
                    // A clip is represented either flat or compact, so the clip order
                    // fully breaks ties between the two sources.
                    (key.start, key.key, key.origin)
                        < (flat_note.start, flat_note.key, self.origins[flat.start])
                })
            {
                repeating = match scratch.as_deref_mut() {
                    Some(scratch) => {
                        scratch.advance(clips, &ticks, chase);
                        scratch.first()
                    }
                    None => Self::next_repeat(clips, &ticks, Some(key), chase),
                };
                return Some(clips[index].placed(key));
            }
            regular.inspect(|_| flat.start += 1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_overlaps_replace_only_the_selected_cursor() {
        const CLIPS: usize = 256;
        let template = |origin: usize| SeqNote {
            start: Tick::ZERO,
            length: Tick(1),
            key: 60 + (origin % 2) as u8,
            velocity: 1 + (origin % 127) as u8,
            probability: 100,
            channel: 0,
        };
        let clips = (0..CLIPS)
            .map(|origin| {
                RepeatedMidi::new(
                    Tick::ZERO,
                    Tick(1000),
                    Tick(40),
                    origin,
                    vec![template(origin)],
                )
            })
            .collect();
        let schedule = NoteSchedule::compile(Vec::new(), clips);
        let mut expected: Vec<_> = (0..CLIPS)
            .map(|origin| SeqNote {
                start: Tick(400),
                ..template(origin)
            })
            .collect();
        expected.sort_by_key(|note| note.key);
        let mut scratch = RepeatScratch::new();
        let mut actual = Vec::with_capacity(CLIPS);
        let mut collect = |scratch: &mut RepeatScratch| {
            actual.clear();
            assert_no_alloc::assert_no_alloc(|| {
                actual.extend(schedule.view().merge(
                    0..0,
                    Tick(400)..Tick(401),
                    None,
                    Some(scratch),
                ));
            });
            assert_eq!(actual, expected);
        };
        collect(&mut scratch);
        assert_eq!(scratch.examined, CLIPS * 2);

        // Force the fallback without allocating thousands of test clips. It preserves the
        // same order even when more clips overlap than the scratch can hold.
        scratch.heap = BinaryHeap::with_capacity(8);
        collect(&mut scratch);
        assert_eq!(scratch.examined, 9);
    }
}
