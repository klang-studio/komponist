//! The read-ahead thread (ARCHITECTURE §6.9.3): keeps the ring of every audio track filled ahead
//! of its playhead and builds the heads of the segments.
//!
//! The audio thread talks to it through one SPSC queue of [`Msg`] (it never blocks, it sends only
//! when the playhead jumps) and through the rings; the streamer never waits for the audio thread.
//! The work is a plain method ([`Streamer::service`]) behind a mutex: the thread calls it in a
//! loop, and an offline render (export, tests) calls it itself before every block
//! ([`StreamShared::ensure`]), so that its result does not depend on thread timing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use super::render::{SegmentRenderer, render_head};
use super::stretch::{BadInput, until_next_seek_anchor};
use super::{AudioPlan, Heads, Ring};

/// Frames rendered per step.
pub(crate) const CHUNK_FRAMES: usize = 2048;
/// Chunks per voice per pass, so a pass stays short and every voice gets its turn.
const PASS_CHUNKS: usize = 2;
/// A ring with less free room waits (small writes are not worth the call).
const MIN_FREE: usize = 256;
/// Heads built per idle pass.
const HEADS_PER_PASS: usize = 2;
/// Sleep of the thread when nothing is left to do.
const IDLE: Duration = Duration::from_millis(2);
/// Messages the audio thread can have queued.
pub(crate) const QUEUE_CAPACITY: usize = 256;

/// Audio thread → streamer.
pub(crate) enum Msg {
    /// Start `epoch` of the plan's ring at song frame `pos` (with the loop the transport will
    /// wrap at, if any).
    Start {
        plan: Arc<AudioPlan>,
        epoch: u32,
        pos: i64,
        lp: Option<(i64, i64)>,
    },
}

/// Where the transport's position goes after `n` frames from `pos`: the loop start when they end
/// at the loop end. The audio thread and the streamer both follow this rule, so the stream the
/// streamer writes is the order the transport plays.
pub(crate) fn advance(pos: i64, n: i64, lp: Option<(i64, i64)>) -> i64 {
    match lp {
        Some((start, end)) if pos < end && pos + n == end => start,
        _ => pos + n,
    }
}

struct StreamVoice {
    plan: Arc<AudioPlan>,
    epoch: u32,
    cursor: i64,
    lp: Option<(i64, i64)>,
    /// Frames since the stream last jumped from the loop end to the loop start.
    since_wrap: Option<u32>,
    renderer: SegmentRenderer,
    /// Frames written so far (all epochs).
    produced: u64,
    failed: HashMap<usize, Retry>,
}

struct Retry {
    until: std::time::Instant,
    input: Option<BadInput>,
}

struct HeadJob {
    plan: Arc<AudioPlan>,
    done: Vec<[Box<[f32]>; 2]>,
}

/// How much the read-ahead thread works (diagnostics, the perf suite).
#[derive(Debug, Default)]
pub struct StreamStats {
    busy_ns: AtomicU64,
    frames: AtomicU64,
}

impl StreamStats {
    /// Time the thread spent rendering (passes that did something), in total.
    pub fn busy(&self) -> Duration {
        Duration::from_nanos(self.busy_ns.load(Ordering::Relaxed))
    }

    /// Frames written into rings by the thread (silence included).
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

/// The streamer's state. See the module docs.
pub(crate) struct Streamer {
    stats: Arc<StreamStats>,
    rx: rtrb::Consumer<Msg>,
    voices: Vec<StreamVoice>,
    head_queue: Vec<Arc<AudioPlan>>,
    head_job: Option<HeadJob>,
    buf: [Vec<f32>; 2],
}

impl Streamer {
    pub(crate) fn new(rx: rtrb::Consumer<Msg>, stats: Arc<StreamStats>) -> Streamer {
        Streamer {
            stats,
            rx,
            voices: Vec::new(),
            head_queue: Vec::new(),
            head_job: None,
            buf: [vec![0.0; CHUNK_FRAMES], vec![0.0; CHUNK_FRAMES]],
        }
    }

    /// Takes the queued messages. Returns whether there were any.
    pub(crate) fn drain(&mut self) -> bool {
        let mut any = false;
        while let Ok(Msg::Start {
            plan,
            epoch,
            pos,
            lp,
        }) = self.rx.pop()
        {
            any = true;
            plan.ring.begin_write(epoch);
            if !plan.offline
                && !plan.heads_ready()
                && !self.head_queue.iter().any(|p| Arc::ptr_eq(p, &plan))
            {
                self.head_queue.push(Arc::clone(&plan));
            }
            let same = |v: &StreamVoice| Arc::ptr_eq(&v.plan.ring, &plan.ring);
            match self.voices.iter_mut().find(|v| same(v)) {
                Some(v) => {
                    if v.plan.id != plan.id {
                        v.renderer.reset();
                    }
                    v.renderer.restart();
                    v.failed.clear();
                    v.plan = plan;
                    v.epoch = epoch;
                    v.cursor = pos;
                    v.lp = lp;
                    v.since_wrap = None;
                }
                None => self.voices.push(StreamVoice {
                    plan,
                    epoch,
                    cursor: pos,
                    lp,
                    since_wrap: None,
                    renderer: SegmentRenderer::new(),
                    produced: 0,
                    failed: HashMap::new(),
                }),
            }
        }
        any
    }

    /// One pass: messages, a few chunks per voice, and (when nothing else was to do) some heads.
    /// Returns whether it did anything.
    pub(crate) fn service(&mut self) -> bool {
        let mut progress = self.drain();
        for v in self.voices.iter_mut().filter(|v| !v.plan.offline) {
            for _ in 0..PASS_CHUNKS {
                let before = v.produced;
                if v.produce(&mut self.buf, CHUNK_FRAMES) {
                    progress = true;
                    self.stats
                        .frames
                        .fetch_add(v.produced - before, Ordering::Relaxed);
                } else {
                    break;
                }
            }
        }
        // A voice whose plan nobody else holds belongs to a program that is gone.
        self.voices.retain(|v| Arc::strong_count(&v.plan) > 1);
        if !progress {
            progress = self.build_heads();
        }
        progress
    }

    /// Offline: produces until `epoch` of `ring` has `upto` frames written.
    fn ensure(&mut self, ring: &Arc<Ring>, epoch: u32, upto: u32) {
        self.drain();
        let Some(v) = self
            .voices
            .iter_mut()
            .find(|v| Arc::ptr_eq(&v.plan.ring, ring) && v.epoch == epoch)
        else {
            return;
        };
        while let Some(written) = ring.written(epoch) {
            let need = upto.wrapping_sub(written) as i32;
            if need <= 0 {
                break;
            }
            // Export must not diagnose a failure from frames it never consumes.
            let limit = if v.plan.offline {
                need as usize
            } else {
                CHUNK_FRAMES
            };
            if !v.produce(&mut self.buf, limit) {
                break;
            }
        }
    }

    fn build_heads(&mut self) -> bool {
        if self
            .head_job
            .as_ref()
            .is_some_and(|j| Arc::strong_count(&j.plan) == 1)
        {
            self.head_job = None;
        }
        while self.head_job.is_none() {
            let Some(plan) = self.head_queue.pop() else {
                return false;
            };
            if !plan.heads_ready() && Arc::strong_count(&plan) > 1 {
                self.head_job = Some(HeadJob {
                    plan,
                    done: Vec::new(),
                });
            }
        }
        let job = self.head_job.as_mut().expect("set above");
        let total = job.plan.segments.len();
        for _ in 0..HEADS_PER_PASS {
            if job.done.len() < total {
                job.done.push(render_head(&job.plan, job.done.len()));
            }
        }
        if job.done.len() == total {
            let job = self.head_job.take().expect("set above");
            let _ = job.plan.heads.set(Heads { segments: job.done });
        }
        true
    }
}

impl StreamVoice {
    /// Renders and writes one chunk if the ring has room. Returns whether it wrote.
    fn produce(&mut self, buf: &mut [Vec<f32>; 2], limit: usize) -> bool {
        let plan = &*self.plan;
        let free = plan.ring.free(self.epoch);
        if free < MIN_FREE {
            return false;
        }
        let mut k = free.min(CHUNK_FRAMES).min(limit) as i64;
        if let Some((_, end)) = self.lp
            && self.cursor < end
        {
            k = k.min(end - self.cursor);
        }
        let segment = plan.locate(self.cursor);
        match segment {
            Ok(i) => k = k.min(plan.segments[i].end - self.cursor),
            Err(Some(next)) => k = k.min(next - self.cursor),
            Err(None) => {}
        }
        let mut n = k as usize;
        let [bl, br] = buf;
        let available = match segment {
            Ok(i) => {
                let seg = &plan.segments[i];
                let j = self.cursor - seg.start;
                let mut blocked = false;
                if let Some(retry) = self.failed.get(&i)
                    && std::time::Instant::now() < retry.until
                {
                    if let Some(input) = retry.input {
                        // A clean request in this same segment can proceed without waiting.
                        if !self.renderer.continues_warp(seg, j) {
                            n = n.min(until_next_seek_anchor(plan, j));
                            blocked = input.blocks(plan, seg, j, n);
                        }
                    } else {
                        blocked = true;
                    }
                }
                if blocked {
                    false
                } else {
                    match self
                        .renderer
                        .render(plan, i, j, [&mut bl[..n], &mut br[..n]])
                    {
                        Ok(()) => true,
                        Err(error) => {
                            if !plan.offline {
                                self.failed.insert(
                                    i,
                                    Retry {
                                        until: std::time::Instant::now() + super::RETRY_AFTER,
                                        input: error.input,
                                    },
                                );
                            }
                            if error.rendered > 0 {
                                // Publish the valid prefix before advancing through the fault.
                                n = error.rendered;
                                true
                            } else {
                                if error.input.is_some() {
                                    n = n.min(until_next_seek_anchor(plan, j));
                                }
                                false
                            }
                        }
                    }
                }
            }
            Err(_) => {
                bl[..n].fill(0.0);
                br[..n].fill(0.0);
                true
            }
        };
        let (l, r) = (&mut bl[..n], &mut br[..n]);
        if !available {
            l.fill(0.0);
            r.fill(0.0);
        }
        self.wrap_fades(l, r);
        if available {
            plan.ring.push(self.epoch, l, r);
        } else {
            plan.ring.push_available(self.epoch, l, r, false);
        }
        self.produced += n as u64;
        let before = self.cursor;
        let k = n as i64;
        self.cursor = advance(before, k, self.lp);
        let wrapped = matches!(self.lp, Some((_, end)) if before < end && before + k == end);
        if wrapped {
            self.renderer.restart();
            self.since_wrap = Some(0);
        } else if let Some(done) = self.since_wrap {
            let done = done.saturating_add(n as u32);
            self.since_wrap = (done < plan.edge_frames).then_some(done);
        }
        true
    }

    /// The edge fades of a loop wrap: the last `edge` frames before the loop end fade out and the
    /// first after the loop start fade in (the stream jumps there; without it the jump clicks).
    fn wrap_fades(&self, l: &mut [f32], r: &mut [f32]) {
        let Some((_, end)) = self.lp else { return };
        let edge = i64::from(self.plan.edge_frames);
        for (k, (l, r)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            let mut g = 1.0f32;
            let pos = self.cursor + k as i64;
            if self.cursor < end && end - pos <= edge {
                g *= (end - pos - 1) as f32 / edge as f32;
            }
            if let Some(done) = self.since_wrap {
                let since = i64::from(done) + k as i64;
                if since < edge {
                    g *= since as f32 / edge as f32;
                }
            }
            *l *= g;
            *r *= g;
        }
    }
}

/// The streamer and its thread, shared by the engine (offline servicing) and the thread.
pub(crate) struct StreamShared {
    pub(crate) stats: Arc<StreamStats>,
    core: Mutex<Streamer>,
    stop: AtomicBool,
    /// The thread does no work while set (a slow disk, in tests); an offline render is not held.
    stalled: AtomicBool,
}

impl StreamShared {
    /// The shared state and the queue's producer for the audio thread.
    pub(crate) fn new() -> (Arc<StreamShared>, rtrb::Producer<Msg>) {
        let (tx, rx) = rtrb::RingBuffer::new(QUEUE_CAPACITY);
        let stats = Arc::new(StreamStats::default());
        let shared = Arc::new(StreamShared {
            stats: Arc::clone(&stats),
            core: Mutex::new(Streamer::new(rx, stats)),
            stop: AtomicBool::new(false),
            stalled: AtomicBool::new(false),
        });
        (shared, tx)
    }

    /// Starts the thread; `None` if the system refuses (audio clips then underrun in realtime).
    pub(crate) fn spawn(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        let shared = Arc::clone(self);
        std::thread::Builder::new()
            .name("komponist-stream".into())
            .spawn(move || shared.run())
            .ok()
    }

    /// Holds the thread's work (or lets it go on).
    pub(crate) fn stall(&self, stalled: bool) {
        self.stalled.store(stalled, Ordering::Relaxed);
    }

    /// Asks the thread to end.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    fn run(&self) {
        while !self.stop.load(Ordering::Relaxed) {
            if self.stalled.load(Ordering::Relaxed) {
                std::thread::sleep(IDLE);
                continue;
            }
            let began = std::time::Instant::now();
            let progress = {
                let mut core = self.core.lock().unwrap_or_else(PoisonError::into_inner);
                // A decoder that panics must not take the thread's voices down in a loop: the
                // voices are dropped (silence) and the thread goes on.
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.service()))
                    .unwrap_or_else(|_| {
                        core.voices.clear();
                        core.head_job = None;
                        core.head_queue.clear();
                        false
                    })
            };
            if progress {
                let spent = began.elapsed().as_nanos() as u64;
                self.stats.busy_ns.fetch_add(spent, Ordering::Relaxed);
            } else {
                std::thread::sleep(IDLE);
            }
        }
    }

    /// Offline. Takes the queued messages now (to make room in the queue).
    pub(crate) fn drain(&self) {
        let mut core = self.core.lock().unwrap_or_else(PoisonError::into_inner);
        core.drain();
    }

    /// Offline. Renders on this thread until `epoch` of `ring` has `upto` frames written.
    pub(crate) fn ensure(&self, ring: &Arc<Ring>, epoch: u32, upto: u32) {
        let mut core = self.core.lock().unwrap_or_else(PoisonError::into_inner);
        core.ensure(ring, epoch, upto);
    }
}
