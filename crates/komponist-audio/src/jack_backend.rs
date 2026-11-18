//! Explicit JACK routes use a single duplex client and the server's sample clock.

mod active;

use self::active::ActiveClient;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, Port, PortFlags, PortSpec};

use crate::input::MAX_INPUT_CHANNELS;
use crate::{
    AudioError, AudioProcess, CallbackInfo, EngineSlot, HostKind, InputBuffer, JackCaptureChannel,
    JackDirection, JackPort, JackRoutes, OutputBuffer, RoutedInput, StreamParams, StreamStats,
    frames_to_duration,
};

// Bounded history for aligning two differently delayed JACK ports, in frames.
pub(crate) const CAPTURE_HISTORY_FRAMES: usize = 8192;
const ROUTE_RECHECK: Duration = Duration::from_secs(1);

fn open(name: &str) -> Result<Client, AudioError> {
    Client::new(name, ClientOptions::NO_START_SERVER)
        .map(|(client, _)| client)
        .map_err(|_| AudioError::HostUnavailable(HostKind::Jack))
}

pub(crate) fn ports() -> Result<Vec<JackPort>, AudioError> {
    let mut ports = graph(&open("komponist-port-list")?);
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(ports)
}

pub(crate) fn server_present() -> bool {
    open("komponist-server-check").is_ok()
}

fn client_name() -> String {
    format!("komponist_{}", std::process::id())
}

fn graph(client: &Client) -> Vec<JackPort> {
    let audio_spec = AudioIn::default();
    let audio_type = audio_spec.jack_port_type();
    let app = client_name();
    let renamed = format!("{app}-");
    let cpal = format!("cpal_client_{}_", std::process::id());
    let mut ports = Vec::new();
    for (direction, flags) in [
        (JackDirection::Capture, PortFlags::IS_OUTPUT),
        (JackDirection::Playback, PortFlags::IS_INPUT),
    ] {
        ports.extend(
            client
                .ports(None, Some(audio_type), flags)
                .into_iter()
                .filter_map(|name| {
                    let port = canonical_port(client, &name)?;
                    // Omit our own duplex ports, including a JACK-renamed concurrent client.
                    let owner = name.split_once(':')?.0;
                    if owner == app || owner.starts_with(&renamed) || owner.starts_with(&cpal) {
                        return None;
                    }
                    let alias = port
                        .aliases()
                        .ok()
                        .into_iter()
                        .flatten()
                        .find(|alias| !alias.is_empty() && *alias != name);
                    Some(JackPort {
                        name,
                        alias,
                        direction,
                    })
                }),
        );
    }
    ports
}

#[derive(Debug)]
struct Route {
    // None uses the compatibility ports advertised by the current JACK graph.
    capture: Option<Vec<String>>,
    playback: [Option<String>; 2],
    capture_channels: usize,
}

impl Route {
    fn new(routes: &JackRoutes, automatic_capture: bool) -> Result<Self, AudioError> {
        let capture_channels = routes
            .capture
            .as_ref()
            .map_or(usize::from(automatic_capture) * 2, Vec::len);
        let capture = routes.capture.iter().flatten();
        let playback = routes.playback.iter().flatten();
        let [left, right] = &routes.playback;
        if capture_channels > MAX_INPUT_CHANNELS
            || (left.is_some() && left == right)
            || capture.chain(playback).any(|name| {
                name.as_bytes().contains(&0)
                    || !name
                        .split_once(':')
                        .is_some_and(|(client, port)| !client.is_empty() && !port.is_empty())
            })
        {
            return Err(AudioError::UnsupportedConfig(
                "invalid JACK audio route".into(),
            ));
        }
        Ok(Self {
            capture: routes.capture.clone(),
            playback: routes.playback.clone(),
            capture_channels,
        })
    }

    fn targets(&self, automatic: &Targets) -> Targets {
        let capture = self.capture.clone().unwrap_or_else(|| {
            automatic
                .capture
                .iter()
                .take(self.capture_channels)
                .cloned()
                .collect()
        });
        let playback = if self.playback.iter().all(Option::is_none) {
            automatic.playback.clone()
        } else {
            self.playback.clone()
        };
        Targets { capture, playback }
    }
}

#[derive(Default)]
struct Targets {
    capture: Vec<String>,
    playback: [Option<String>; 2],
}

fn server_default_ports(client: &Client, direction: JackDirection) -> Vec<String> {
    let audio = AudioIn::default();
    let (pattern, flags) = match direction {
        JackDirection::Capture => ("^system:capture_[0-9]+$", PortFlags::IS_OUTPUT),
        JackDirection::Playback => ("^system:playback_[0-9]+$", PortFlags::IS_INPUT),
    };
    // PipeWire-JACK matches "system:*" against its default-node metadata, even if the
    // returned names are the real device ports. A local prefix filter misses those ports.
    let mut ports = client.ports(Some(pattern), Some(audio.jack_port_type()), flags);
    if direction == JackDirection::Capture {
        ports.retain(|name| !is_monitor(name));
    }
    if ports.is_empty() {
        let mut physical = client.ports(
            None,
            Some(audio.jack_port_type()),
            flags | PortFlags::IS_PHYSICAL,
        );
        if direction == JackDirection::Capture {
            physical.retain(|name| !is_monitor(name));
        }
        // Without a server default, use a single unambiguous hardware device in server channel order.
        if let Some((owner, _)) = physical.first().and_then(|name| name.rsplit_once(':'))
            && physical.iter().all(|name| {
                name.rsplit_once(':')
                    .is_some_and(|(client, _)| client == owner)
            })
        {
            return physical;
        }
    }
    ports
}

fn is_monitor(name: &str) -> bool {
    name.rsplit_once(':').is_some_and(|(owner, port)| {
        owner.ends_with(" Monitor") || owner.ends_with(" (Monitor)") || port.starts_with("monitor_")
    })
}

fn default_targets(client: &Client, route: &Route) -> Targets {
    let mut targets = Targets::default();
    if route.capture.is_none() && route.capture_channels != 0 {
        targets.capture = server_default_ports(client, JackDirection::Capture);
    }
    if route.playback.iter().all(Option::is_none) {
        for (slot, name) in targets
            .playback
            .iter_mut()
            .zip(server_default_ports(client, JackDirection::Playback))
        {
            *slot = Some(name);
        }
    }
    targets
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum XrunScope {
    Jack,
    PipeWireGraph,
}

impl XrunScope {
    fn from_version(version: &CStr) -> Self {
        if version
            .to_bytes()
            .windows(b"(using PipeWire ".len())
            .any(|part| part == b"(using PipeWire ")
        {
            Self::PipeWireGraph
        } else {
            Self::Jack
        }
    }

    fn detect() -> Self {
        static SCOPE: OnceLock<XrunScope> = OnceLock::new();
        *SCOPE.get_or_init(|| {
            let Ok(library) = jack_sys::library() else {
                return Self::Jack;
            };
            // SAFETY: libjack exports this signature and returns a NUL-terminated string with
            // library lifetime. The library is the same handle used by the JACK Rust binding.
            unsafe {
                let Ok(version) = library
                    .get::<unsafe extern "C" fn() -> *const c_char>(b"jack_get_version_string")
                else {
                    return Self::Jack;
                };
                let value = version();
                if value.is_null() {
                    Self::Jack
                } else {
                    Self::from_version(CStr::from_ptr(value))
                }
            }
        })
    }

    fn capture_gap(self, xrun: bool, missed_period: bool, route_gap: bool) -> bool {
        // PipeWire-JACK reports changes in the driver's graph-wide XRUN counter. It can
        // notify a client whose own capture callbacks and links stayed continuous.
        missed_period || route_gap || (xrun && self == Self::Jack)
    }
}

const TRACE_XRUN: u32 = 1 << 0;
const TRACE_MISSED_PERIOD: u32 = 1 << 1;
const TRACE_LINK_GAP: u32 = 1 << 2;
const TRACE_DELIVERED_GAP: u32 = 1 << 3;
const TRACE_LINK_DISCONNECT: u32 = 1 << 4;
const TRACE_LINK_REPAIR: u32 = 1 << 5;
const TRACE_ROUTE_UNAVAILABLE: u32 = 1 << 6;
const TRACE_ROUTE_READY: u32 = 1 << 7;

/// Optional breadcrumbs shared by the process/graph callbacks and the control thread.
#[derive(Default)]
struct CaptureTrace {
    pending: AtomicU32,
    // Latest callback interval in microseconds (high bits) and JACK frame advance (low bits).
    timing: AtomicU64,
}

impl CaptureTrace {
    fn note(&self, flags: u32) {
        self.pending.fetch_or(flags, Ordering::Release);
    }

    fn note_period(&self, flags: u32, previous: Option<Cycle>, current: Cycle) {
        if flags == 0 {
            return;
        }
        if let Some(previous) = previous {
            let delta = current.first_frame.wrapping_sub(previous.first_frame);
            let elapsed = current.entered.saturating_duration_since(previous.entered);
            let micros = elapsed.as_micros().min(u128::from(u32::MAX)) as u32;
            self.timing.store(
                (u64::from(micros) << 32) | u64::from(delta),
                Ordering::Relaxed,
            );
        }
        self.note(flags);
    }

    fn report(&self, period: u32, rate: u32, ready: bool) {
        let flags = self.pending.swap(0, Ordering::AcqRel);
        if flags == 0 {
            return;
        }
        let events = [
            (TRACE_XRUN, "server-xrun"),
            (TRACE_MISSED_PERIOD, "missed-local-period"),
            (TRACE_LINK_GAP, "capture-link-gap"),
            (TRACE_DELIVERED_GAP, "input-discontinuity-sent"),
            (TRACE_LINK_DISCONNECT, "capture-link-disconnected"),
            (TRACE_LINK_REPAIR, "capture-link-repair"),
            (TRACE_ROUTE_UNAVAILABLE, "capture-route-unavailable"),
            (TRACE_ROUTE_READY, "capture-route-ready"),
        ]
        .into_iter()
        .filter_map(|(bit, name)| (flags & bit != 0).then_some(name))
        .collect::<Vec<_>>();
        let timing = self.timing.load(Ordering::Relaxed);
        eprintln!(
            "komponist: JACK capture trace events={events:?} ready={ready} period={period} rate={rate} frame_advance={} interval_us={}",
            timing as u32,
            timing >> 32,
        );
    }
}

#[derive(Default)]
struct Signals {
    shutdown: AtomicBool,
    graph_changed: AtomicBool,
    capture: Vec<CaptureState>,
    capture_revision: AtomicU64,
    xrun: AtomicBool,
    trace: Option<CaptureTrace>,
    sample_rate: AtomicU32,
    period: AtomicU32,
    active_period: AtomicBool,
    playback_latency: AtomicU32,
}

// Keep readiness, a pending discontinuity and the graph revision in one atomic. A
// control-thread reconciliation must never overwrite a newer capture-port event.
const CAPTURE_READY: u64 = 1;
const CAPTURE_GAP: u64 = 2;
const CAPTURE_REVISION: u32 = 4;

#[derive(Default)]
struct CaptureState {
    state: AtomicU64,
    // The canonical peer expected for the upcoming graph operation.
    peer: AtomicU32,
}

impl CaptureState {
    fn changed(&self) {
        let _ = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                let revision = (state as u32).wrapping_add(CAPTURE_REVISION);
                let flags = if state & (CAPTURE_READY | CAPTURE_GAP) != 0 {
                    CAPTURE_GAP
                } else {
                    0
                };
                Some((state & !u64::from(u32::MAX)) | (u64::from(revision) & !3) | flags)
            });
    }

    fn connection_event(&self, peer: jack::PortId, connected: bool) {
        // A delayed notification for the intended initial connection cannot invalidate the
        // controller's verification. Any other connection invalidates it before the next period.
        if !connected || self.peer.load(Ordering::Acquire) != peer {
            self.changed();
        }
    }

    fn snapshot(&self) -> u64 {
        self.state.load(Ordering::Acquire)
    }

    fn publish(&self, before: u64, latency: Option<u32>) -> bool {
        let mut next = u64::from(before as u32) & !CAPTURE_READY;
        if before & CAPTURE_READY != 0 && latency.is_none() {
            next |= CAPTURE_GAP;
        }
        if let Some(latency) = latency {
            next |= (u64::from(latency) << 32) | CAPTURE_READY;
        }
        self.state
            .compare_exchange(before, next, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    fn take_period(&self) -> (Option<u32>, bool) {
        let mut state = self.state.load(Ordering::Acquire);
        if state & CAPTURE_GAP != 0 {
            state = self.state.fetch_and(!CAPTURE_GAP, Ordering::AcqRel);
        }
        let latency = (state & CAPTURE_READY != 0).then_some((state >> 32) as u32);
        (latency, state & CAPTURE_GAP != 0)
    }
}

struct Notifications {
    signals: Arc<Signals>,
    stats: Arc<StreamStats>,
    capture_ports: Vec<jack::PortId>,
}

impl Notifications {
    fn graph_changed(&self) {
        self.signals.graph_changed.store(true, Ordering::Release);
    }
}

impl Notifications {
    fn shutdown(&self) {
        self.signals.shutdown.store(true, Ordering::Release);
    }

    fn sample_rate(&self, rate: jack::Frames) {
        self.signals.sample_rate.store(rate, Ordering::Release);
        self.graph_changed();
    }

    fn buffer_size(&self, frames: jack::Frames) {
        self.signals.period.store(frames, Ordering::Relaxed);
        self.graph_changed();
    }

    fn ports_connected(&self, a: jack::PortId, b: jack::PortId, connected: bool) {
        // The callback may run with JACK's process lock held. Only compare pre-resolved IDs.

        if let Some((index, peer)) =
            self.capture_ports
                .iter()
                .enumerate()
                .find_map(|(index, &port)| {
                    if port == a {
                        Some((index, b))
                    } else if port == b {
                        Some((index, a))
                    } else {
                        None
                    }
                })
        {
            self.signals.capture[index].connection_event(peer, connected);
            if !connected && let Some(trace) = &self.signals.trace {
                trace.note(TRACE_LINK_DISCONNECT);
            }
        }
        self.graph_changed();
    }

    fn xrun(&self) {
        self.stats.record_xrun();
        self.signals.xrun.store(true, Ordering::Release);
    }
}

struct PeriodRenderer<P: AudioProcess> {
    slot: EngineSlot<P>,
    output: Vec<f32>,
    input: Vec<f32>,
    prepared_frames: usize,
    history: Vec<f32>,
    history_cursor: usize,
    history_valid: [u32; MAX_INPUT_CHANNELS],
    rate: u32,
}

impl<P: AudioProcess> PeriodRenderer<P> {
    fn new(slot: EngineSlot<P>, params: &StreamParams, capture_channels: usize) -> Self {
        let prepared_frames = params.max_frames as usize;
        Self {
            slot,
            output: vec![0.0; prepared_frames * 2],
            input: vec![0.0; prepared_frames * capture_channels],
            prepared_frames,
            history: if capture_channels >= 2 {
                vec![0.0; CAPTURE_HISTORY_FRAMES * capture_channels]
            } else {
                Vec::new()
            },
            history_cursor: 0,
            history_valid: [0; MAX_INPUT_CHANNELS],
            rate: params.sample_rate,
        }
    }

    fn process(
        &mut self,
        output: [&mut [f32]; 2],
        capture: &[&[f32]],
        now: Instant,
        timing: Timing,
    ) {
        assert_no_alloc::assert_no_alloc(|| self.render(output, capture, now, timing));
    }

    fn render(
        &mut self,
        [left, right]: [&mut [f32]; 2],
        capture: &[&[f32]],
        now: Instant,
        timing: Timing,
    ) {
        let total = left.len();
        let channels = capture.len();
        for offset in (0..total).step_by(self.prepared_frames) {
            let frames = self.prepared_frames.min(total - offset);
            let end = offset + frames;
            let output = &mut self.output[..frames * 2];
            let samples = &mut self.input[..frames * channels];
            if channels != 0 {
                for (i, frame) in samples.chunks_exact_mut(channels).enumerate() {
                    for (sample, channel) in frame.iter_mut().zip(capture) {
                        *sample = channel.get(offset + i).copied().unwrap_or(0.0);
                    }
                }
            }
            let discontinuous = if offset == 0 {
                timing.capture.discontinuous
            } else {
                0
            };
            for (index, valid) in self.history_valid[..channels].iter_mut().enumerate() {
                let bit = 1 << index;
                if timing.capture.available & bit == 0 || discontinuous & bit != 0 {
                    *valid = 0;
                }
            }
            let latency = timing.capture.latencies[..channels]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
            let input = InputBuffer::new(
                samples,
                channels,
                frames_to_duration(u64::from(latency), self.rate),
                discontinuous != 0,
            )
            .with_routes(RoutedInput {
                latencies: &timing.capture.latencies[..channels],
                available: timing.capture.available,
                discontinuous,
                history: &self.history,
                history_cursor: self.history_cursor,
                history_valid: &self.history_valid[..channels],
            });
            let info = CallbackInfo {
                sample_rate: self.rate,
                frames: frames as u32,
                now: now
                    .checked_sub(frames_to_duration((total - end) as u64, self.rate))
                    .unwrap_or(now),
                // The same per-frame latency applies throughout a synchronous JACK period.
                output_latency: timing.playback_latency,
            };
            self.slot
                .process_input(&mut OutputBuffer::new(output, 2), &info, &input);
            if channels >= 2 {
                for frame in samples.chunks_exact(channels) {
                    let dst = self.history_cursor * channels;
                    self.history[dst..dst + channels].copy_from_slice(frame);
                    self.history_cursor = (self.history_cursor + 1) % CAPTURE_HISTORY_FRAMES;
                }
                for (index, valid) in self.history_valid[..channels].iter_mut().enumerate() {
                    if timing.capture.available & (1 << index) != 0 {
                        *valid = (*valid + frames as u32).min(CAPTURE_HISTORY_FRAMES as u32);
                    }
                }
            }
            for ((left, right), frame) in left[offset..end]
                .iter_mut()
                .zip(&mut right[offset..end])
                .zip(output.as_chunks::<2>().0)
            {
                *left = frame[0];
                *right = frame[1];
            }
        }
    }
}

#[derive(Clone, Copy)]
struct CapturePeriod {
    latencies: [u32; MAX_INPUT_CHANNELS],
    available: u64,
    discontinuous: u64,
}

impl Default for CapturePeriod {
    fn default() -> Self {
        Self {
            latencies: [0; MAX_INPUT_CHANNELS],
            available: 0,
            discontinuous: 0,
        }
    }
}

impl CapturePeriod {
    // Keep the last whole latency update if this callback overlapped the control thread's
    // publication. Never restore a lane that a graph notification just made unavailable.
    fn use_committed_timing(&mut self, before: u64, after: u64, committed: &mut Self) {
        if before == after && before.is_multiple_of(2) {
            *committed = *self;
        } else {
            self.available &= committed.available;
            self.latencies = committed.latencies;
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Timing {
    playback_latency: Duration,
    capture: CapturePeriod,
}

struct Duplex<P: AudioProcess> {
    render: PeriodRenderer<P>,
    input: Vec<Port<AudioIn>>,
    output: [Port<AudioOut>; 2],
    signals: Arc<Signals>,
    previous: Option<Cycle>,
    committed_capture: CapturePeriod,
    xrun_scope: XrunScope,
}

#[derive(Clone, Copy)]
struct Cycle {
    first_frame: jack::Frames,
    frames: jack::Frames,
    scheduled: Option<u64>,
    entered: Instant,
}

impl Cycle {
    fn missed_before(self, current: Self, rate: u32) -> bool {
        let advance = current.first_frame.wrapping_sub(self.first_frame);
        if advance <= self.frames || advance > rate || self.frames == 0 || current.frames == 0 {
            return false;
        }
        let skipped = advance - self.frames;
        let scheduled = self.scheduled.zip(current.scheduled);
        // Across a resize a skipped cycle may be of either size. Use the smaller quantum to
        // distinguish a full missed period from a clock rebase, and require corroboration from
        // the two scheduled starts (entry times can jitter across the buffer-size callback).
        if let Some((before, after)) = scheduled
            && self.frames != current.frames
        {
            let shorter = self.frames.min(current.frames);
            let elapsed = Duration::from_micros(after.saturating_sub(before));
            let expected = frames_to_duration(u64::from(advance), rate);
            return skipped >= shorter
                && elapsed.abs_diff(expected) <= frames_to_duration(u64::from(shorter / 2), rate);
        }
        if !skipped.is_multiple_of(current.frames) {
            return false;
        }
        // Scheduled starts are unaffected by the previous callback waking up late.
        let (elapsed, tolerance) = match scheduled {
            Some((before, after)) => (
                Duration::from_micros(after.saturating_sub(before)),
                current.frames / 2,
            ),
            None if self.frames == current.frames => (
                current.entered.saturating_duration_since(self.entered),
                self.frames,
            ),
            None => (
                current.entered.saturating_duration_since(self.entered),
                current.frames / 2,
            ),
        };
        elapsed >= frames_to_duration(u64::from(advance.saturating_sub(tolerance)), rate)
    }
}

impl<P: AudioProcess> Duplex<P> {
    fn process(&mut self, scope: &jack::ProcessScope) -> Control {
        let [left, right] = &mut self.output;
        let entered = Instant::now();
        let times = scope.cycle_times().ok();
        let cycle = Cycle {
            entered,
            first_frame: times.map_or_else(|| scope.last_frame_time(), |time| time.current_frames),
            frames: scope.n_frames(),
            scheduled: times.map(|time| time.current_usecs),
        };
        if !self.signals.active_period.load(Ordering::Relaxed) {
            self.signals.active_period.store(true, Ordering::Release);
        }
        if self.signals.sample_rate.load(Ordering::Acquire) != self.render.rate {
            // Some JACK implementations ignore the rate callback's return value. Never hand
            // new-rate samples to the old engine. The control thread replaces this stream.
            if self.previous.take().is_some() {
                for lane in &self.signals.capture {
                    lane.changed();
                }
            }
            left.as_mut_slice(scope).fill(0.0);
            right.as_mut_slice(scope).fill(0.0);
            return Control::Continue;
        }
        let xrun = self.signals.xrun.swap(false, Ordering::AcqRel);
        let previous = self.previous.replace(cycle);
        let missed_period =
            previous.is_some_and(|before| before.missed_before(cycle, self.render.rate));
        let mut input = [&[][..]; MAX_INPUT_CHANNELS];
        let mut capture = CapturePeriod::default();
        let revision = self.signals.capture_revision.load(Ordering::Acquire);
        for (index, (port, lane)) in self.input.iter().zip(&self.signals.capture).enumerate() {
            let (latency, route_gap) = lane.take_period();
            let bit = 1 << index;
            if let Some(latency) = latency {
                capture.available |= bit;
                capture.latencies[index] = latency;
                input[index] = port.as_slice(scope);
            }
            if route_gap {
                capture.discontinuous |= bit;
            }
        }
        fence(Ordering::Acquire);
        capture.use_committed_timing(
            revision,
            self.signals.capture_revision.load(Ordering::Relaxed),
            &mut self.committed_capture,
        );
        let discontinuous =
            self.xrun_scope
                .capture_gap(xrun && previous.is_some(), missed_period, false);
        let route_gap = capture.discontinuous != 0;
        if discontinuous {
            capture.discontinuous = u64::MAX;
        }
        if let Some(trace) = &self.signals.trace {
            let flags = (u32::from(xrun) * TRACE_XRUN)
                | (u32::from(missed_period) * TRACE_MISSED_PERIOD)
                | (u32::from(route_gap) * TRACE_LINK_GAP)
                | (u32::from((discontinuous || route_gap) && capture.available != 0)
                    * TRACE_DELIVERED_GAP);
            trace.note_period(flags, previous, cycle);
        }
        self.render.process(
            [left.as_mut_slice(scope), right.as_mut_slice(scope)],
            &input[..self.input.len()],
            cycle.entered,
            Timing {
                playback_latency: frames_to_duration(
                    u64::from(self.signals.playback_latency.load(Ordering::Acquire)),
                    self.render.rate,
                ),
                capture,
            },
        );
        Control::Continue
    }
}

pub(crate) struct JackStream<P: AudioProcess + 'static> {
    client: ActiveClient<P>,
    signals: Arc<Signals>,
    route: Route,
    capture: Vec<String>,
    capture_sources: Vec<JackCaptureChannel>,
    managed_capture: Vec<Option<String>>,
    playback: [String; 2],
    playback_connected: [bool; 2],
    managed_playback: [Option<String>; 2],
    next_check: Instant,
}

impl<P: AudioProcess + 'static> JackStream<P> {
    pub(crate) fn open(
        active_rate: Option<u32>,
    ) -> Result<(Client, StreamParams, u32), AudioError> {
        let client = open(&client_name())?;
        let inactive_rate = client.sample_rate();
        let params = StreamParams {
            sample_rate: prepared_rate(inactive_rate, active_rate)?,
            max_frames: client.buffer_size(),
            channels: 2,
            workgroup: None,
        };
        if !crate::backend::valid_buffer_frames(params.max_frames) {
            return Err(AudioError::UnsupportedConfig(
                "invalid JACK server period".into(),
            ));
        }
        Ok((client, params, inactive_rate))
    }

    pub(crate) fn start(
        client: Client,
        params: &StreamParams,
        inactive_rate: u32,
        routes: &JackRoutes,
        automatic_capture: bool,
        slot: EngineSlot<P>,
        stats: Arc<StreamStats>,
    ) -> Result<Self, AudioError> {
        let route = Route::new(routes, automatic_capture)?;
        let input: Vec<_> = (0..route.capture_channels)
            .map(|i| client.register_port(&format!("capture_{}", i + 1), AudioIn::default()))
            .collect::<Result<_, _>>()
            .map_err(jack_error)?;
        let output = [
            client
                .register_port("playback_L", AudioOut::default())
                .map_err(jack_error)?,
            client
                .register_port("playback_R", AudioOut::default())
                .map_err(jack_error)?,
        ];
        let capture_ports = input
            .iter()
            .map(|port| port_id(&client, port))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                AudioError::Backend("JACK: could not resolve capture port IDs".into())
            })?;
        let capture = input
            .iter()
            .map(Port::name)
            .collect::<Result<_, _>>()
            .map_err(jack_error)?;
        let playback = [
            output[0].name().map_err(jack_error)?,
            output[1].name().map_err(jack_error)?,
        ];
        let xrun_scope = XrunScope::detect();
        let trace = (std::env::var_os("KOMPONIST_JACK_CAPTURE_TRACE").as_deref()
            == Some(std::ffi::OsStr::new("1")))
        .then(CaptureTrace::default);
        if trace.is_some() {
            eprintln!(
                "komponist: JACK capture trace started xrun_scope={xrun_scope:?} rate={} period={} capture_ports={}",
                params.sample_rate, params.max_frames, route.capture_channels,
            );
        }
        let signals = Arc::new(Signals {
            trace,
            sample_rate: AtomicU32::new(inactive_rate),
            period: AtomicU32::new(params.max_frames),
            capture: (0..route.capture_channels)
                .map(|_| CaptureState::default())
                .collect(),
            ..Signals::default()
        });
        let client = ActiveClient::new(
            client,
            Notifications {
                signals: Arc::clone(&signals),
                stats,
                capture_ports,
            },
            Duplex {
                render: PeriodRenderer::new(slot, params, route.capture_channels),
                input,
                output,
                signals: Arc::clone(&signals),
                previous: None,
                committed_capture: CapturePeriod::default(),
                xrun_scope,
            },
        )?;
        let managed_capture = vec![None; route.capture_channels];
        let mut stream = Self {
            client,
            signals,
            route,
            capture,
            capture_sources: Vec::new(),
            managed_capture,
            playback,
            playback_connected: [false; 2],
            managed_playback: [None, None],
            next_check: Instant::now(),
        };
        stream.reconcile();
        Ok(stream)
    }

    // All graph enumeration and link mutations take place on the control thread.
    fn reconcile(&mut self) {
        let client = self.client.as_client();
        let graph = graph(client);
        let desired = self.route.targets(&default_targets(client, &self.route));
        self.capture_sources.clear();
        let mut pending = [(0, None); MAX_INPUT_CHANNELS];
        for (index, (local, lane)) in self.capture.iter().zip(&self.signals.capture).enumerate() {
            let target = desired.capture.get(index).map(String::as_str);
            let automatic = self.route.capture.is_none();
            let mut before = lane.snapshot();
            let invalidate = |before: &mut u64| {
                if *before & CAPTURE_READY != 0 {
                    lane.changed();
                    *before = lane.snapshot();
                    if let Some(trace) = &self.signals.trace {
                        trace.note(TRACE_LINK_REPAIR);
                    }
                }
            };
            let local_port = automatic.then(|| canonical_port(client, local)).flatten();
            let connections = local_port.as_ref().map(|port| {
                let default = canonical_peer(client, target, JackDirection::Capture, &graph);
                automatic_capture(
                    client,
                    local,
                    port,
                    default.as_ref(),
                    &mut self.managed_capture[index],
                    || invalidate(&mut before),
                )
            });
            let target = match connections.as_deref() {
                Some(connections) if !connections.is_empty() => single_connection(connections),
                _ => target,
            };
            let peer = canonical_peer(client, target, JackDirection::Capture, &graph)
                .and_then(|port| port_id(client, &port).map(|id| (port, id)));
            let new_peer = peer.as_ref().map_or(u32::MAX, |(_, id)| *id);
            if lane.peer.swap(new_peer, Ordering::AcqRel) != new_peer {
                invalidate(&mut before);
            }
            let peer = peer.as_ref().map(|(port, _)| port);
            let latency = if automatic {
                // The actual sole connection may be manual. A mix remains untouched and is
                // unavailable to the recorder, even if one of the links is the default.
                local_port.as_ref().zip(peer).and_then(|(port, peer)| {
                    let name = peer.name().ok()?;
                    (sole_peer(&port.get_connections(), Some(&name))
                        && canonical_port(client, &name)?.raw() == peer.raw())
                    .then(|| port.get_latency_range(jack::LatencyType::Capture).1)
                })
            } else {
                exclusive_port(client, local, peer, JackDirection::Capture, || {
                    invalidate(&mut before);
                })
            };
            pending[index] = (before, latency);
            let entry = target.map_or_else(
                || JackCaptureChannel::missing(local),
                |name| capture_channel(name, &graph, false),
            );
            self.capture_sources.push(entry);
        }
        // Publish related port latencies together. Graph queries and mutations above can
        // block, so this odd revision covers just the bounded series of atomic updates.
        self.signals.capture_revision.fetch_add(1, Ordering::AcqRel);
        for (index, lane) in self.signals.capture.iter().enumerate() {
            let (before, latency) = pending[index];
            if !lane.publish(before, latency) {
                self.signals.graph_changed.store(true, Ordering::Release);
            } else if let Some(trace) = &self.signals.trace
                && (before & CAPTURE_READY != 0) != latency.is_some()
            {
                trace.note(if latency.is_some() {
                    TRACE_ROUTE_READY
                } else {
                    TRACE_ROUTE_UNAVAILABLE
                });
            }
            self.capture_sources[index].connected = lane.snapshot() & CAPTURE_READY != 0;
        }
        self.signals
            .capture_revision
            .fetch_add(1, Ordering::Release);
        let mut playback_latency = 0;
        let automatic = self.route.playback.iter().all(Option::is_none);
        for (index, (local, target)) in self.playback.iter().zip(&desired.playback).enumerate() {
            let peer = canonical_peer(client, target.as_deref(), JackDirection::Playback, &graph);
            let latency = if automatic {
                automatic_playback(
                    client,
                    local,
                    peer.as_ref(),
                    &mut self.managed_playback[index],
                )
            } else {
                exclusive_port(client, local, peer.as_ref(), JackDirection::Playback, || {})
            };
            self.playback_connected[index] = latency.is_some();
            playback_latency = playback_latency.max(latency.unwrap_or(0));
        }
        self.signals
            .playback_latency
            .store(playback_latency, Ordering::Release);
        self.next_check = Instant::now() + ROUTE_RECHECK;
    }

    pub(crate) fn channels(&self) -> u16 {
        if self.available_channels() == 0 {
            0
        } else {
            self.capture.len() as u16
        }
    }

    pub(crate) fn available_channels(&self) -> u64 {
        self.signals
            .capture
            .iter()
            .enumerate()
            .fold(0, |mask, (index, lane)| {
                mask | (u64::from(lane.snapshot() & CAPTURE_READY != 0) << index)
            })
    }

    pub(crate) fn input_route_available(&self, first: u16, count: u16) -> bool {
        let first = usize::from(first);
        let end = first + usize::from(count);
        let Some(lanes) = self.signals.capture.get(first..end) else {
            return false;
        };
        let mut latencies = lanes.iter().map(|lane| {
            let state = lane.snapshot();
            (state & CAPTURE_READY != 0).then_some((state >> 32) as u32)
        });
        match (
            count,
            latencies.next().flatten(),
            latencies.next().flatten(),
        ) {
            (1, Some(_), None) => true,
            (2, Some(left), Some(right)) => left.abs_diff(right) as usize <= CAPTURE_HISTORY_FRAMES,
            _ => false,
        }
    }

    pub(crate) fn playback_connections(&self) -> [bool; 2] {
        self.playback_connected
    }

    pub(crate) fn capture_sources(&self) -> &[JackCaptureChannel] {
        &self.capture_sources
    }

    pub(crate) fn period(&self) -> u32 {
        self.signals.period.load(Ordering::Relaxed)
    }

    pub(crate) fn latency(&self, rate: u32) -> Duration {
        frames_to_duration(
            u64::from(self.signals.playback_latency.load(Ordering::Acquire)),
            rate,
        )
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.signals.sample_rate.load(Ordering::Acquire)
    }

    pub(crate) fn active_rate(&self) -> Option<u32> {
        self.signals
            .active_period
            .load(Ordering::Acquire)
            .then(|| self.sample_rate())
    }

    pub(crate) fn poll(&mut self) -> bool {
        let running = !self.signals.shutdown.load(Ordering::Acquire);
        if running
            && (self.signals.graph_changed.swap(false, Ordering::AcqRel)
                || Instant::now() >= self.next_check)
        {
            self.reconcile();
        }
        if let Some(trace) = &self.signals.trace {
            trace.report(self.period(), self.sample_rate(), self.channels() != 0);
        }
        running
    }
}

fn prepared_rate(inactive: u32, active: Option<u32>) -> Result<u32, AudioError> {
    let rate = active.unwrap_or(inactive);
    if crate::backend::valid_sample_rate(rate) {
        Ok(rate)
    } else {
        Err(AudioError::UnsupportedConfig(
            "invalid JACK server rate".into(),
        ))
    }
}

fn capture_channel(name: &str, graph: &[JackPort], connected: bool) -> JackCaptureChannel {
    graph
        .iter()
        .find(|port| port.name == name && port.direction == JackDirection::Capture)
        .map_or_else(
            || JackCaptureChannel::missing(name),
            |port| JackCaptureChannel {
                port: port.clone(),
                connected,
            },
        )
}

fn port_id<S: PortSpec>(client: &Client, port: &Port<S>) -> Option<jack::PortId> {
    // SAFETY: the control thread owns the live client and these ports.
    let uuid = unsafe { jack_sys::jack_port_uuid(port.raw()) };
    let index = unsafe { jack_sys::jack_uuid_to_index(uuid) };
    // PipeWire's UUID index is limited to 24 bits, while callback serials are 32 bits.
    // Verify the ID against the actual registered port before installing it in the callback.
    [index, (uuid as u32).wrapping_sub(1)]
        .into_iter()
        .find(|&id| {
            client
                .port_by_id(id)
                .is_some_and(|resolved| resolved.raw() == port.raw())
        })
}

fn single_connection(connections: &[String]) -> Option<&str> {
    if let [connection] = connections {
        Some(connection)
    } else {
        None
    }
}

fn canonical_port(client: &Client, name: &str) -> Option<Port<jack::Unowned>> {
    let port = client.port_by_name(name)?;
    (port.name().ok().as_deref() == Some(name)).then_some(port)
}

fn canonical_peer(
    client: &Client,
    target: Option<&str>,
    direction: JackDirection,
    graph: &[JackPort],
) -> Option<Port<jack::Unowned>> {
    // PipeWire-JACK can resolve a visible alias before the canonical name in a name lookup.
    canonical_port(client, exact_target(target, direction, graph)?)
}

fn sole_peer(connections: &[String], target: Option<&str>) -> bool {
    match target {
        Some(target) => single_connection(connections) == Some(target),
        None => connections.is_empty(),
    }
}

fn exclusive_port(
    client: &Client,
    local: &str,
    peer: Option<&Port<jack::Unowned>>,
    direction: JackDirection,
    invalidated: impl FnOnce(),
) -> Option<u32> {
    let port = canonical_port(client, local)?;
    let target = peer.and_then(|peer| peer.name().ok());
    let target = target.as_deref();
    let connections = port.get_connections();
    if !sole_peer(&connections, target) {
        invalidated();
        for other in connections
            .iter()
            .filter(|other| Some(other.as_str()) != target)
        {
            if canonical_port(client, other).is_some() {
                let (source, sink) = edge(direction, local, other);
                let _ = client.disconnect_ports_by_name(source, sink);
            }
        }
        if let Some(target) = target
            && !connections.iter().any(|other| other == target)
        {
            let (source, sink) = edge(direction, local, target);
            let _ = client.connect_ports_by_name(source, sink);
        }
    }
    let target = target?;
    if !sole_peer(&port.get_connections(), Some(target))
        || canonical_port(client, target)?.raw() != peer?.raw()
    {
        return None;
    }
    let latency_type = match direction {
        JackDirection::Capture => jack::LatencyType::Capture,
        JackDirection::Playback => jack::LatencyType::Playback,
    };
    Some(port.get_latency_range(latency_type).1)
}

fn automatic_capture_patch<'a>(
    connections: &[String],
    managed: Option<&'a str>,
    default: Option<&str>,
) -> (Option<&'a str>, bool) {
    let remove = managed.filter(|name| {
        Some(*name) != default && connections.iter().any(|connected| connected == *name)
    });
    let add = default.is_some() && connections.len() == usize::from(remove.is_some());
    (remove, add)
}

fn automatic_capture(
    client: &Client,
    local: &str,
    port: &Port<jack::Unowned>,
    default: Option<&Port<jack::Unowned>>,
    managed: &mut Option<String>,
    invalidated: impl FnOnce(),
) -> Vec<String> {
    let connections = port.get_connections();
    let default = default.and_then(|port| port.name().ok());
    if managed
        .as_ref()
        .is_some_and(|old| !connections.contains(old))
    {
        *managed = None;
    }
    let (remove, add) =
        automatic_capture_patch(&connections, managed.as_deref(), default.as_deref());
    if let Some(remove) = remove {
        invalidated();
        if client.disconnect_ports_by_name(remove, local).is_ok() {
            *managed = None;
        }
    }
    if add
        && let Some(default) = default
        && port.get_connections().is_empty()
        && client.connect_ports_by_name(&default, local).is_ok()
    {
        *managed = Some(default);
    }
    port.get_connections()
}

fn automatic_playback(
    client: &Client,
    local: &str,
    peer: Option<&Port<jack::Unowned>>,
    managed: &mut Option<String>,
) -> Option<u32> {
    let port = canonical_port(client, local)?;
    let target = peer.and_then(|peer| peer.name().ok());
    if managed.as_deref() != target.as_deref()
        && let Some(old) = managed.take()
        && canonical_port(client, &old).is_some()
    {
        let _ = client.disconnect_ports_by_name(local, &old);
    }
    if let Some(target) = target
        && port.get_connections().is_empty()
        && client.connect_ports_by_name(local, &target).is_ok()
    {
        *managed = Some(target);
    }
    // Automatic output retains manual links, including when no default is available.
    (!port.get_connections().is_empty())
        .then(|| port.get_latency_range(jack::LatencyType::Playback).1)
}

fn exact_target<'a>(
    target: Option<&'a str>,
    direction: JackDirection,
    graph: &[JackPort],
) -> Option<&'a str> {
    target.filter(|name| {
        graph
            .iter()
            .any(|port| port.name == *name && port.direction == direction)
    })
}

fn edge<'a>(direction: JackDirection, local: &'a str, remote: &'a str) -> (&'a str, &'a str) {
    match direction {
        JackDirection::Capture => (remote, local),
        JackDirection::Playback => (local, remote),
    }
}

fn jack_error(error: jack::Error) -> AudioError {
    AudioError::Backend(format!("JACK: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MidiIn, midi};

    fn port(name: &str, direction: JackDirection) -> JackPort {
        JackPort {
            name: name.to_owned(),
            alias: Some("Identical alias".to_owned()),
            direction,
        }
    }

    #[test]
    fn jack_reopen_uses_the_observed_rate_over_the_inactive_rate_hint() {
        assert_eq!(prepared_rate(48_000, None).unwrap(), 48_000);
        assert_eq!(prepared_rate(48_000, Some(44_100)).unwrap(), 44_100);
        assert!(prepared_rate(48_000, Some(0)).is_err());
    }

    #[test]
    fn playback_route_rejects_duplicate_nonempty_targets() {
        let routes = |playback| JackRoutes {
            playback,
            ..JackRoutes::default()
        };
        let port = "Speaker:left".to_owned();
        assert!(Route::new(&routes([Some(port.clone()), Some(port.clone())]), true).is_err());
        assert!(Route::new(&routes([Some(port.clone()), None]), true).is_ok());
        assert!(Route::new(&routes([Some(port), Some("Speaker:right".into())]), true).is_ok());
        assert!(Route::new(&routes([None, None]), true).is_ok());
    }

    #[test]
    fn capture_requires_the_actual_sole_peer() {
        assert!(sole_peer(&["USB:mic".into()], Some("USB:mic")));
        assert!(!sole_peer(&["FX:out".into()], Some("USB:mic")));
        assert!(!sole_peer(
            &["USB:mic".into(), "FX:out".into()],
            Some("USB:mic")
        ));
        assert!(sole_peer(&[], None));
    }

    #[test]
    fn capture_metadata_follows_route_order_and_keeps_missing_slots() {
        use JackDirection::{Capture, Playback};

        let mut graph = vec![
            port("Hotone:left", Capture),
            port("DI:right", Playback),
            port("FX:return", Capture),
        ];
        let names = ["DI:right", "Hotone:left", "FX:return"];
        let sources = names.map(|name| capture_channel(name, &graph, name == "Hotone:left"));
        assert_eq!(
            sources.each_ref().map(|channel| channel.port.name.as_str()),
            names
        );
        assert_eq!(
            sources.each_ref().map(|channel| channel.connected),
            [false, true, false]
        );
        assert!(sources[0].port.alias.is_none());
        assert_eq!(sources[1].port.alias, sources[2].port.alias);

        graph.push(port("DI:right", Capture));
        assert_eq!(
            capture_channel(names[0], &graph, true),
            JackCaptureChannel {
                port: port("DI:right", Capture),
                connected: true,
            }
        );
    }

    #[test]
    fn exact_port_routes_preserve_order_across_disconnect_reconnect_and_alias_collisions() {
        use JackDirection::{Capture, Playback};

        let settings = JackRoutes {
            capture: Some(vec!["USB:right".into(), "USB:left".into()]),
            playback: [Some("USB:speaker_2".into()), None],
        };
        let route = Route::new(&settings, true).unwrap();
        let mut graph = vec![
            port("system:capture_1", Capture),
            port("system:playback_1", Playback),
            port("USB:speaker_2", Playback),
            port("USB:left", Capture),
            port("other:right", Capture),
        ];
        assert_ne!(graph[3].label(), graph[4].label());
        let targets = route.targets(&Targets::default());
        assert_eq!(targets.capture, ["USB:right", "USB:left"]);
        assert_eq!(targets.playback, [Some("USB:speaker_2".into()), None]);
        assert_eq!(
            exact_target(Some(&targets.capture[0]), Capture, &graph),
            None
        );
        assert_eq!(exact_target(Some("other:right"), Playback, &graph), None);
        assert_eq!(exact_target(Some("Identical alias"), Capture, &graph), None);
        graph.push(port("USB:right", Capture));
        assert_eq!(
            exact_target(Some(&targets.capture[0]), Capture, &graph),
            Some("USB:right")
        );
        assert_eq!(
            edge(Capture, "komponist:capture_1", "USB:right"),
            ("USB:right", "komponist:capture_1")
        );
        assert_eq!(
            edge(Playback, "komponist:playback_L", "USB:speaker_2"),
            ("komponist:playback_L", "USB:speaker_2")
        );
    }

    #[test]
    fn auto_uses_default_canonical_ports_with_explicit_outputs_and_respects_disabled_capture() {
        let defaults = Targets {
            capture: vec![
                "Hotone MP80:capture_FL".into(),
                "Hotone MP80:capture_FR".into(),
            ],
            playback: [
                Some("Hotone MP80:playback_FL".into()),
                Some("Hotone MP80:playback_FR".into()),
            ],
        };
        let config = JackRoutes::default();
        assert!(config.is_automatic());
        let targets = Route::new(&config, true).unwrap().targets(&defaults);
        assert_eq!(targets.capture, defaults.capture);
        assert_eq!(targets.playback, defaults.playback);
        let config = JackRoutes {
            playback: [Some("Speakers:left".into()), Some("Speakers:right".into())],
            ..config
        };
        let targets = Route::new(&config, true).unwrap().targets(&defaults);
        assert_eq!(targets.capture, defaults.capture);
        assert_eq!(targets.playback, config.playback);
        let config = JackRoutes {
            capture: Some(vec![]),
            ..config
        };
        let targets = Route::new(&config, true).unwrap().targets(&defaults);
        assert!(targets.capture.is_empty());
        assert!(is_monitor("Hotone MP80 Monitor:monitor_FL"));
        assert!(is_monitor("Hotone MP80:monitor_FR"));
        assert!(!is_monitor("Hotone MP80:capture_FR"));
    }

    #[test]
    fn clock_rebases_and_quantum_changes_do_not_invent_capture_gaps() {
        let time = Instant::now();
        let previous = Cycle {
            first_frame: 1024,
            frames: 64,
            scheduled: None,
            entered: time,
        };
        let at = |first_frame, frames, elapsed| Cycle {
            first_frame,
            frames,
            scheduled: None,
            entered: time + frames_to_duration(elapsed, 48_000),
        };
        assert!(!previous.missed_before(at(1088, 64, 64), 48_000));
        assert!(!previous.missed_before(at(12_288, 64, 64), 48_000));
        assert!(!previous.missed_before(at(1, 64, 64), 48_000));
        assert!(!previous.missed_before(at(1280, 256, 256), 48_000));
        assert!(!previous.missed_before(at(1088, 256, 64), 48_000));
        assert!(previous.missed_before(at(1344, 256, 320), 48_000));
        assert!(!previous.missed_before(at(1344, 256, 64), 48_000));
        assert!(previous.missed_before(at(1152, 64, 128), 48_000));
    }

    #[test]
    fn scheduled_clock_detects_an_old_size_skip_before_a_quantum_change() {
        let time = Instant::now();
        let old = Cycle {
            first_frame: 1024,
            frames: 64,
            scheduled: Some(10_000),
            entered: time,
        };
        let resized = |first_frame, usecs| Cycle {
            first_frame,
            frames: 256,
            scheduled: Some(usecs),
            entered: time,
        };
        assert!(old.missed_before(resized(1152, 12_667), 48_000));
        assert!(!old.missed_before(resized(1088, 11_333), 48_000));
        assert!(!old.missed_before(resized(1152, 11_333), 48_000));
        assert!(!old.missed_before(resized(10_000, 12_667), 48_000));
    }

    #[test]
    fn automatic_capture_keeps_a_manual_peer_when_a_default_appears() {
        let manual = "Synth:out";
        let built_in = "system:capture_1";
        let next_default = "USB:capture_2";
        let patch = |names: &[&str], managed| {
            let connections: Vec<_> = names.iter().map(|name| (*name).to_owned()).collect();
            automatic_capture_patch(&connections, managed, Some(next_default))
        };
        assert_eq!(patch(&[manual], None), (None, false));
        assert_eq!(patch(&[], None), (None, true));
        assert_eq!(patch(&[built_in], Some(built_in)), (Some(built_in), true));
        assert_eq!(
            patch(&[built_in, manual], Some(built_in)),
            (Some(built_in), false)
        );
        assert_eq!(patch(&[next_default], Some(next_default)), (None, false));
    }

    #[test]
    fn a_late_entry_does_not_hide_the_next_skipped_period() {
        let started = Instant::now();
        let old = Cycle {
            first_frame: 1024,
            frames: 64,
            scheduled: Some(10_000),
            entered: started + frames_to_duration(48, 48_000),
        };
        let resumed = Cycle {
            first_frame: 1152,
            frames: 64,
            scheduled: Some(12_667),
            entered: started + frames_to_duration(128, 48_000),
        };
        assert!(old.missed_before(resumed, 48_000));
        assert!(
            Cycle {
                scheduled: None,
                ..old
            }
            .missed_before(
                Cycle {
                    scheduled: None,
                    ..resumed
                },
                48_000
            )
        );
    }

    #[test]
    fn capture_uses_a_whole_latency_update_and_keeps_link_gaps_immediate() {
        let mut committed = CapturePeriod {
            available: 3,
            ..CapturePeriod::default()
        };
        committed.latencies[..2].copy_from_slice(&[64, 256]);
        let mut partial = committed;
        partial.latencies[0] = 128;
        partial.use_committed_timing(2, 3, &mut committed);
        assert_eq!(&partial.latencies[..2], &[64, 256]);
        assert_eq!(partial.available, 3);

        let mut link_lost = partial;
        link_lost.available = 1;
        link_lost.discontinuous = 2;
        link_lost.use_committed_timing(3, 3, &mut committed);
        assert_eq!((link_lost.available, link_lost.discontinuous), (1, 2));

        let mut settled = partial;
        settled.latencies[..2].copy_from_slice(&[128, 320]);
        settled.use_committed_timing(4, 4, &mut committed);
        assert_eq!(&committed.latencies[..2], &[128, 320]);
    }

    #[test]
    fn an_updated_latency_keeps_the_same_connected_capture_continuous() {
        let lane = CaptureState::default();
        assert!(lane.publish(lane.snapshot(), Some(64)));
        assert!(lane.publish(lane.snapshot(), Some(256)));
        assert_eq!(lane.take_period(), (Some(256), false));
        assert!(lane.publish(lane.snapshot(), None));
        assert_eq!(lane.take_period(), (None, true));
    }

    #[test]
    fn pipewire_driver_xrun_does_not_discard_locally_continuous_capture() {
        let now = Instant::now();
        let previous = Cycle {
            first_frame: 4096,
            frames: 64,
            scheduled: None,
            entered: now,
        };
        let received = Cycle {
            first_frame: 4160,
            frames: 64,
            scheduled: None,
            entered: now + frames_to_duration(64, 48_000),
        };
        let skipped = Cycle {
            first_frame: 4224,
            frames: 64,
            scheduled: None,
            entered: now + frames_to_duration(128, 48_000),
        };
        let pipewire = XrunScope::from_version(c"3.1.6.10 (using PipeWire 1.6.10)");
        let jack = XrunScope::from_version(c"jackdmp 1.9.22");
        let continuous = previous.missed_before(received, 48_000);
        let missed_period = previous.missed_before(skipped, 48_000);
        assert!(!continuous);
        assert!(missed_period);
        assert!(!pipewire.capture_gap(true, continuous, false));
        assert!(jack.capture_gap(true, continuous, false));
        assert!(pipewire.capture_gap(true, missed_period, false));
        assert!(pipewire.capture_gap(false, missed_period, false));
        assert!(pipewire.capture_gap(false, continuous, true));
    }

    #[test]
    fn capture_link_loss_invalidates_an_in_flight_graph_result_and_records_a_gap() {
        let lane = CaptureState::default();
        lane.peer.store(7, Ordering::Release);
        assert!(lane.publish(lane.snapshot(), Some(64)));
        assert_eq!(lane.take_period(), (Some(64), false));
        lane.connection_event(7, true);
        assert_eq!(lane.take_period(), (Some(64), false), "late initial link");
        let before = lane.snapshot();
        lane.connection_event(8, true);
        assert!(!lane.publish(before, Some(64)));
        assert_eq!(lane.take_period(), (None, true), "incoming mix rejected");

        let other_lane = CaptureState::default();
        assert!(other_lane.publish(other_lane.snapshot(), Some(256)));
        assert_eq!(other_lane.take_period(), (Some(256), false));
        assert!(lane.publish(lane.snapshot(), Some(64)));
        lane.connection_event(7, false);
        assert_eq!(lane.take_period(), (None, true));
        assert_eq!(other_lane.take_period(), (Some(256), false));
    }

    struct Loopback {
        gaps: Arc<AtomicU32>,
    }

    impl AudioProcess for Loopback {
        fn prepare(&mut self, _: &StreamParams) {}

        fn process(&mut self, out: &mut OutputBuffer<'_>, _: &CallbackInfo, _: &mut MidiIn<'_>) {
            out.data_mut().fill(0.75);
        }

        fn process_input(
            &mut self,
            out: &mut OutputBuffer<'_>,
            info: &CallbackInfo,
            midi: &mut MidiIn<'_>,
            input: &InputBuffer<'_>,
        ) {
            if input.channels() == 0 {
                self.process(out, info, midi);
                return;
            }
            assert_eq!(input.frames(), out.frames());
            assert_eq!(info.sample_rate, 48_000);
            assert_eq!(info.output_latency, Duration::from_millis(2));
            assert_eq!(input.latency(), Duration::from_millis(1));
            if input.discontinuous() {
                self.gaps.fetch_add(1, Ordering::Relaxed);
            }
            out.data_mut().copy_from_slice(input.data());
        }
    }

    #[test]
    fn duplex_renderer_preserves_channel_order_and_samples_through_period_changes() {
        let params = StreamParams {
            sample_rate: 48_000,
            max_frames: 128,
            channels: 2,
            workgroup: None,
        };
        let (_, midi) = midi::queue(4);
        let gaps = Arc::new(AtomicU32::new(0));
        let (slot, _handle) = EngineSlot::new(
            Box::new(Loopback {
                gaps: Arc::clone(&gaps),
            }),
            midi,
            Arc::new(StreamStats::new()),
        );
        let mut render = PeriodRenderer::new(slot, &params, 2);
        let timing = Timing {
            playback_latency: Duration::from_millis(2),
            capture: CapturePeriod {
                latencies: [48; MAX_INPUT_CHANNELS],
                available: 3,
                discontinuous: 0,
            },
        };
        let mut next = Instant::now();
        for (frames, gap) in [
            (64, false),
            (128, false),
            (256, false),
            (256, true),
            (64, false),
        ] {
            let capture_l: Vec<_> = (0..frames).map(|frame| frame as f32).collect();
            let capture_r: Vec<_> = capture_l.iter().map(|value| -1.0 - value).collect();
            let mut output_l = vec![-1.0; frames];
            let mut output_r = vec![-1.0; frames];
            next += frames_to_duration(frames as u64, params.sample_rate);
            render.process(
                [&mut output_l, &mut output_r],
                &[&capture_r, &capture_l],
                next,
                Timing {
                    capture: CapturePeriod {
                        discontinuous: u64::from(gap),
                        ..timing.capture
                    },
                    ..timing
                },
            );
            assert_eq!(output_l, capture_r);
            assert_eq!(output_r, capture_l);
        }
        assert_eq!(
            gaps.load(Ordering::Relaxed),
            1,
            "gap reported once across a split period"
        );
        // Unavailable capture is distinct from a silent signal and does not stop playback.
        let mut output_l = [0.0; 64];
        let mut output_r = [0.0; 64];
        render.process([&mut output_l, &mut output_r], &[], next, timing);
        assert_eq!(output_l, [0.75; 64]);
        assert_eq!(output_r, [0.75; 64]);
    }
}
