//! The audio thread's side of an audio track (RT: no allocation, locks, IO or blocking).
//!
//! A [`Voice`] lives in the engine's track state. Per segment the engine calls
//! [`Voice::prepare`] on the audio thread (it decides whether the playhead jumped and asks the
//! streamer for a new epoch of the ring), and the track's job calls [`Voice::render`] (it reads the
//! ring, or the heads where the ring is not there yet, and applies the de-click fades).
//!
//! **De-click.** The transport can jump anywhere: playback start, locate, a program that changed
//! the track. Where the content has no edge of its own the voice fades in over the 3 ms edge
//! length after a start or jump (not at a segment start, which has its edge fade already; not in an
//! offline render, which has no speakers to click), and
//! at a stop or jump it plays out the next 3 ms of the old position faded out (`tail`), copied
//! from the ring when the playhead left. A loop wrap has its fades in the stream (the streamer
//! writes them).
//!
//! Live input and monitoring (#13) come in here later: the track's source stage is
//! [`Voice::render`]'s output; monitoring mixes the hardware or bus input into the same buffer
//! before the chain, and recording taps the input in the engine, not the voice.

use std::sync::Arc;

use super::streamer::{Msg, StreamShared, advance};
use super::{AudioPlan, MAX_EDGE_FRAMES};

/// Frames of lead the ring needs before a voice counts as ready (a block and some margin).
const PRIME_FRAMES: i32 = 4096;

/// Per-track playback state on the audio thread.
pub(crate) struct Voice {
    /// Id of the plan the current epoch plays (0: none).
    plan: u64,
    /// Identity only: keeping an Arc here could free the outgoing ring on the audio thread.
    ring: usize,
    epoch: u32,
    /// The song frame the next read continues at (the transport's, after the loop rule).
    next_pos: i64,
    /// Frames of the epoch consumed.
    elapsed: u32,
    /// The loop the epoch was started with.
    lp: Option<(i64, i64)>,
    /// The voice read the last block (the transport was playing).
    active: bool,
    /// The message that asks for the epoch did not fit in the queue: ask again.
    unsent: bool,
    /// Frames of the fade-in after a start or jump done so far.
    ramp: Option<u32>,
    tail: [Box<[f32]>; 2],
    tail_len: usize,
    tail_pos: usize,
    /// Set by [`Voice::render`] for the sum phase: the last block missed frames inside a segment.
    pub(crate) underrun: bool,
    /// The ring has frames ahead of the playhead (set by [`Voice::render`]).
    pub(crate) primed: bool,
}

/// What [`Voice::prepare`] needs to know about the segment.
pub(crate) struct Prepare<'a> {
    /// Song frame of the segment's first frame.
    pub(crate) pos: i64,
    /// Frames in the segment.
    pub(crate) n: usize,
    /// The transport runs (count-in included).
    pub(crate) playing: bool,
    /// The loop the transport wraps at.
    pub(crate) lp: Option<(i64, i64)>,
    /// Messages to the streamer.
    pub(crate) tx: &'a mut rtrb::Producer<Msg>,
    /// An offline render: the streamer's work runs here, before the block.
    pub(crate) offline: Option<&'a StreamShared>,
}

impl Voice {
    /// Non-RT.
    pub(crate) fn new() -> Voice {
        let plane = || vec![0.0; MAX_EDGE_FRAMES].into_boxed_slice();
        Voice {
            plan: 0,
            ring: 0,
            epoch: 0,
            next_pos: 0,
            elapsed: 0,
            lp: None,
            active: false,
            unsent: false,
            ramp: None,
            tail: [plane(), plane()],
            tail_len: 0,
            tail_pos: 0,
            underrun: false,
            primed: true,
        }
    }

    /// RT. Forgets the plan (a track state that changed hands, or a plan the engine cannot play).
    pub(crate) fn clear(&mut self) {
        self.plan = 0;
        self.ring = 0;
        self.active = false;
        self.ramp = None;
        self.tail_len = 0;
        self.underrun = false;
        self.primed = true;
    }

    /// RT. Ends the tail at once (a hard cut).
    pub(crate) fn clear_tail(&mut self) {
        self.tail_len = 0;
    }

    /// RT. Decides whether this segment continues the stream: if the plan changed, the playhead
    /// is not where the stream goes on, or the loop changed, it starts a new epoch.
    pub(crate) fn prepare(&mut self, plan: &Arc<AudioPlan>, mut p: Prepare<'_>) {
        if plan.segments.is_empty() {
            // Nothing to stream: the track is silent and needs no ring.
            self.clear();
            return;
        }
        let was_active = self.active;
        let stale =
            self.plan != plan.id || self.unsent || self.next_pos != p.pos || self.lp != p.lp;
        if stale {
            if was_active && self.ring == Arc::as_ptr(&plan.ring) as usize {
                self.capture_tail(plan);
            }
            self.restart(plan, &mut p);
        }
        if p.playing {
            // A file has no speakers to click: an offline render starts at full level.
            if (!was_active || stale) && p.offline.is_none() {
                self.ramp = Some(0);
            }
            self.active = true;
        } else {
            if was_active && !stale {
                self.capture_tail(plan);
            }
            self.active = false;
        }
        if p.playing
            && !self.unsent
            && let Some(stream) = p.offline
        {
            // The offline render does the streamer's work itself: it reads files and allocates,
            // which only an offline render (no deadline) may do inside the engine.
            let upto = self.elapsed.wrapping_add(p.n as u32);
            assert_no_alloc::permit_alloc(|| stream.ensure(&plan.ring, self.epoch, upto));
        }
    }

    /// RT. Starts a new epoch at `p.pos`.
    fn restart(&mut self, plan: &Arc<AudioPlan>, p: &mut Prepare<'_>) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.elapsed = 0;
        self.next_pos = p.pos;
        self.lp = p.lp;
        plan.ring.begin_read(self.epoch);
        let msg = || Msg::Start {
            plan: Arc::clone(plan),
            epoch: self.epoch,
            pos: p.pos,
            lp: p.lp,
        };
        let mut sent = p.tx.push(msg()).is_ok();
        if !sent && let Some(stream) = p.offline {
            assert_no_alloc::permit_alloc(|| stream.drain());
            sent = p.tx.push(msg()).is_ok();
        }
        self.unsent = !sent;
        self.plan = if sent { plan.id } else { 0 };
        self.ring = Arc::as_ptr(&plan.ring) as usize;
    }

    /// RT. Copies the next edge-length of the old position into the tail, faded out.
    fn capture_tail(&mut self, plan: &AudioPlan) {
        self.tail_len = 0;
        self.tail_pos = 0;
        let Some(written) = plan.ring.written(self.epoch) else {
            return;
        };
        let ahead = (written.wrapping_sub(self.elapsed) as i32).max(0) as usize;
        let k = ahead.min(plan.edge_frames as usize).min(MAX_EDGE_FRAMES);
        if k == 0 {
            return;
        }
        let [l, r] = &mut self.tail;
        plan.ring.copy_out(self.elapsed, &mut l[..k], &mut r[..k]);
        for i in 0..k {
            let g = (k - 1 - i) as f32 / k as f32;
            l[i] *= g;
            r[i] *= g;
        }
        self.tail_len = k;
    }

    /// RT. The track's audio for the segment into `out` (overwritten), `n` frames from song frame
    /// `pos`. Silence when stopped (a tail may still play out) or when `silent` (a plan the engine
    /// cannot play).
    pub(crate) fn render(
        &mut self,
        plan: &AudioPlan,
        pos: i64,
        playing: bool,
        silent: bool,
        out: [&mut [f32]; 2],
    ) {
        let [out_l, out_r] = out;
        let n = out_l.len();
        self.underrun = false;
        if silent || plan.segments.is_empty() {
            self.primed = true;
            out_l.fill(0.0);
            out_r.fill(0.0);
        } else if playing {
            self.read(plan, pos, out_l, out_r);
        } else {
            // Stopped: nothing plays, but the read-ahead fills for the position the playhead is at,
            // so that starting is immediate.
            self.primed = plan
                .ring
                .written(self.epoch)
                .is_some_and(|w| (w.wrapping_sub(self.elapsed) as i32) >= PRIME_FRAMES);
            out_l.fill(0.0);
            out_r.fill(0.0);
        }
        let k = (self.tail_len - self.tail_pos).min(n);
        for i in 0..k {
            out_l[i] += self.tail[0][self.tail_pos + i];
            out_r[i] += self.tail[1][self.tail_pos + i];
        }
        self.tail_pos += k;
        if self.tail_pos >= self.tail_len {
            self.tail_len = 0;
            self.tail_pos = 0;
        }
    }

    /// RT. The ring, then the heads, then silence.
    fn read(&mut self, plan: &AudioPlan, pos: i64, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        let written = plan.ring.written(self.epoch);
        let ahead = written.map(|w| w.wrapping_sub(self.elapsed) as i32);
        self.primed = ahead.is_some_and(|a| a >= PRIME_FRAMES);
        let from_ring = ahead.map_or(0, |a| a.clamp(0, n as i32) as usize);
        if from_ring > 0 {
            plan.ring.copy_out(
                self.elapsed,
                &mut out_l[..from_ring],
                &mut out_r[..from_ring],
            );
        }
        if from_ring < n {
            self.fill_from_heads(plan, pos, from_ring, out_l, out_r);
        }
        if let Some(done) = self.ramp {
            self.ramp = self.fade_in(plan, pos, done, out_l, out_r);
        }
        self.elapsed = self.elapsed.wrapping_add(n as u32);
        self.next_pos = advance(pos, n as i64, self.lp);
        plan.ring.set_read(self.epoch, self.elapsed);
    }

    /// RT. Frames `from ..` of the block from the heads of the segments (or silence). A segment
    /// frame no head covers is an underrun.
    fn fill_from_heads(
        &mut self,
        plan: &AudioPlan,
        pos: i64,
        from: usize,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let n = out_l.len();
        let heads = plan.heads.get();
        let mut k = from;
        while k < n {
            let q = pos + k as i64;
            let remaining = n - k;
            match plan.locate(q) {
                Ok(i) => {
                    let seg = &plan.segments[i];
                    let run = remaining.min((seg.end - q) as usize);
                    let j = (q - seg.start) as usize;
                    let head = heads.and_then(|h| h.segments.get(i));
                    let covered = head.map_or(0, |h| h[0].len().saturating_sub(j)).min(run);
                    if let Some(head) = head.filter(|_| covered > 0) {
                        out_l[k..k + covered].copy_from_slice(&head[0][j..j + covered]);
                        out_r[k..k + covered].copy_from_slice(&head[1][j..j + covered]);
                    }
                    if covered < run {
                        out_l[k + covered..k + run].fill(0.0);
                        out_r[k + covered..k + run].fill(0.0);
                        self.underrun = true;
                    }
                    k += run;
                }
                Err(next) => {
                    let run = next.map_or(remaining, |s| remaining.min((s - q) as usize));
                    out_l[k..k + run].fill(0.0);
                    out_r[k..k + run].fill(0.0);
                    k += run;
                }
            }
        }
    }

    /// RT. The fade-in after a start or jump, `done` frames in. None of it at a segment start:
    /// the segment's own edge fade is the fade-in there.
    fn fade_in(
        &self,
        plan: &AudioPlan,
        pos: i64,
        done: u32,
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) -> Option<u32> {
        let edge = plan.edge_frames;
        if done == 0
            && let Ok(i) = plan.locate(pos)
            && plan.segments[i].start == pos
            && plan.segments[i].edge_in
        {
            return None;
        }
        for (k, (l, r)) in out_l.iter_mut().zip(out_r.iter_mut()).enumerate() {
            let at = done + k as u32;
            if at >= edge {
                return None;
            }
            let g = at as f32 / edge as f32;
            *l *= g;
            *r *= g;
        }
        let next = done + out_l.len() as u32;
        (next < edge).then_some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{self, NoEnv};
    use komponist_model::edits::{self, AudioClipProp, NewAudioTrack, NewClip, NewClipContent};
    use komponist_model::{AudioFile, Project, Tick};

    #[test]
    fn a_plan_change_captures_only_the_outgoing_rings_tail() {
        let (project, track) =
            edits::add_audio_track(&Project::new("Tail"), NewAudioTrack::default()).unwrap();
        let (project, clip) = edits::add_clip(
            &project,
            track,
            NewClip {
                start: Tick::ZERO,
                length: Tick(960),
                name: "Tail".into(),
                content: NewClipContent::Audio {
                    file: AudioFile::new(
                        std::path::Path::new("unused.wav"),
                        None,
                        48_000,
                        48_000,
                        2,
                    ),
                    offset: 0,
                },
            },
        )
        .unwrap();
        let original = compile::build(&project, None, &NoEnv);
        let outgoing = original.tracks[0].audio.as_ref().unwrap();
        let changed =
            edits::set_audio_clip_prop(&project, clip, AudioClipProp::GainDb(-3.0)).unwrap();
        // Incremental edits retain the ring; independently compiled plans do not.
        for shared in [false, true] {
            let next = compile::build(&changed, shared.then_some(&original), &NoEnv);
            let incoming = next.tracks[0].audio.as_ref().unwrap();
            assert_eq!(Arc::ptr_eq(&outgoing.ring, &incoming.ring), shared);
            incoming.ring.begin_read(1);
            incoming.ring.begin_write(1);
            incoming.ring.push(1, &[0.75; 256], &[0.75; 256]);
            let mut voice = Voice::new();
            voice.plan = outgoing.id;
            voice.ring = Arc::as_ptr(&outgoing.ring) as usize;
            voice.epoch = 1;
            voice.active = true;
            let (mut tx, _rx) = rtrb::RingBuffer::new(4);
            voice.prepare(
                incoming,
                Prepare {
                    pos: 0,
                    n: 64,
                    playing: true,
                    lp: None,
                    tx: &mut tx,
                    offline: None,
                },
            );
            assert_eq!(voice.tail_len, if shared { 144 } else { 0 });
            if shared {
                assert!(voice.tail[0][0] > 0.7);
                assert_eq!(voice.tail[0][143], 0.0);
            }
        }
    }
}
