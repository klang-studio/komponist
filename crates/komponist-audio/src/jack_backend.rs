//! Explicit JACK routes use a single duplex client and the server's sample clock.

use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, Port, PortFlags, PortSpec};

use crate::input::MAX_INPUT_CHANNELS;
use crate::{
    AudioError, AudioProcess, CallbackInfo, EngineSlot, HostKind, InputBuffer, JackCaptureChannel,
    JackDirection, JackPort, JackRoutes, OutputBuffer, StreamParams, StreamStats,
    frames_to_duration,
};

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
                    let port = client.port_by_name(&name)?;
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
        if capture_channels > MAX_INPUT_CHANNELS
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
    capture: CaptureState,
    xrun: AtomicBool,
    trace: Option<CaptureTrace>,
    sample_rate: AtomicU32,
    period: AtomicU32,
    // Playback in the low 32 bits, capture in the high 32 bits, both in frames.
    port_latencies: AtomicU64,
}

// Keep readiness, a pending discontinuity and the graph revision in one atomic. A
// control-thread reconciliation must never overwrite a newer capture-port event.
const CAPTURE_READY: u64 = 1;
const CAPTURE_GAP: u64 = 2;
const CAPTURE_REVISION: u64 = 4;

#[derive(Default)]
struct CaptureState(AtomicU64);

impl CaptureState {
    fn changed(&self) {
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |state| {
                let gap = if state & CAPTURE_READY != 0 {
                    CAPTURE_GAP
                } else {
                    0
                };
                Some((state.wrapping_add(CAPTURE_REVISION) & !CAPTURE_READY) | gap)
            });
    }

    fn snapshot(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    fn publish(&self, before: u64, ready: bool) -> bool {
        let after = if ready {
            before | CAPTURE_READY
        } else if before & CAPTURE_READY != 0 {
            (before & !CAPTURE_READY) | CAPTURE_GAP
        } else {
            before
        };
        self.0
            .compare_exchange(before, after, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    }

    fn take_period(&self) -> (bool, bool) {
        let state = self.0.fetch_and(!CAPTURE_GAP, Ordering::AcqRel);
        (state & CAPTURE_READY != 0, state & CAPTURE_GAP != 0)
    }
}

struct Notifications {
    signals: Arc<Signals>,
    stats: Arc<StreamStats>,
    rate: u32,
    capture_ports: Vec<usize>,
}

impl Notifications {
    fn graph_changed(&self) {
        self.signals.graph_changed.store(true, Ordering::Release);
    }
}

impl jack::NotificationHandler for Notifications {
    unsafe fn shutdown(&mut self, _: jack::ClientStatus, _: &str) {
        self.signals.shutdown.store(true, Ordering::Release);
    }

    fn sample_rate(&mut self, _: &Client, rate: jack::Frames) -> Control {
        self.signals.sample_rate.store(rate, Ordering::Release);
        if rate == self.rate {
            Control::Continue
        } else {
            Control::Quit
        }
    }

    fn port_registration(&mut self, _: &Client, _: jack::PortId, _: bool) {
        self.graph_changed();
    }

    fn ports_connected(
        &mut self,
        client: &Client,
        a: jack::PortId,
        b: jack::PortId,
        connected: bool,
    ) {
        // Connect notifications may reach us after the controller already verified the route.
        // Only a disconnect can invalidate a ready route here.
        if !connected
            && [a, b].into_iter().any(|id| {
                client
                    .port_by_id(id)
                    .is_some_and(|port| self.capture_ports.contains(&(port.raw() as usize)))
            })
        {
            self.signals.capture.changed();
            if let Some(trace) = &self.signals.trace {
                trace.note(TRACE_LINK_DISCONNECT);
            }
        }
        self.graph_changed();
    }

    fn graph_reorder(&mut self, _: &Client) -> Control {
        self.graph_changed();
        Control::Continue
    }

    fn xrun(&mut self, _: &Client) -> Control {
        self.stats.record_xrun();
        self.signals.xrun.store(true, Ordering::Release);
        Control::Continue
    }
}

struct PeriodRenderer<P: AudioProcess> {
    slot: EngineSlot<P>,
    output: Vec<f32>,
    input: Vec<f32>,
    prepared_frames: usize,
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
                        *sample = channel[offset + i];
                    }
                }
            }
            let input = InputBuffer::new(
                samples,
                channels,
                timing.capture_latency,
                timing.discontinuous && offset == 0,
            );
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

#[derive(Clone, Copy, Default)]
struct Timing {
    capture_latency: Duration,
    playback_latency: Duration,
    discontinuous: bool,
}

struct Duplex<P: AudioProcess> {
    render: PeriodRenderer<P>,
    input: Vec<Port<AudioIn>>,
    output: [Port<AudioOut>; 2],
    signals: Arc<Signals>,
    previous: Option<Cycle>,
    xrun_scope: XrunScope,
}

#[derive(Clone, Copy)]
struct Cycle {
    first_frame: jack::Frames,
    frames: jack::Frames,
    entered: Instant,
}

impl Cycle {
    fn missed_before(self, current: Self, rate: u32) -> bool {
        // PipeWire reports the current graph driver's clock. A new driver or quantum can
        // rebase that position without dropping this client's input. Corroborate a forward
        // jump in a stable period with elapsed monotonic time.
        let advance = current.first_frame.wrapping_sub(self.first_frame);
        self.frames == current.frames
            && advance > self.frames
            && advance <= rate
            && current.entered.saturating_duration_since(self.entered)
                >= frames_to_duration(u64::from(self.frames) * 3 / 2, rate)
    }
}

impl<P: AudioProcess + 'static> jack::ProcessHandler for Duplex<P> {
    fn process(&mut self, _: &Client, scope: &jack::ProcessScope) -> Control {
        let [left, right] = &mut self.output;
        let cycle = Cycle {
            entered: Instant::now(),
            first_frame: scope.last_frame_time(),
            frames: scope.n_frames(),
        };
        let xrun = self.signals.xrun.swap(false, Ordering::AcqRel);
        let previous = self.previous.replace(cycle);
        let missed_period =
            previous.is_some_and(|before| before.missed_before(cycle, self.render.rate));
        let mut input = [&[][..]; MAX_INPUT_CHANNELS];
        // JACK supplies silence for unconnected inputs. Route state establishes whether that
        // silence is valid capture.
        let (capture_ready, route_gap) = self.signals.capture.take_period();
        let channels = if capture_ready { self.input.len() } else { 0 };
        let discontinuous =
            self.xrun_scope
                .capture_gap(xrun && previous.is_some(), missed_period, route_gap);
        if let Some(trace) = &self.signals.trace {
            let flags = (u32::from(xrun) * TRACE_XRUN)
                | (u32::from(missed_period) * TRACE_MISSED_PERIOD)
                | (u32::from(route_gap) * TRACE_LINK_GAP)
                | (u32::from(discontinuous && channels != 0) * TRACE_DELIVERED_GAP);
            trace.note_period(flags, previous, cycle);
        }
        for (samples, port) in input.iter_mut().zip(&self.input[..channels]) {
            *samples = port.as_slice(scope);
        }
        let latencies = self.signals.port_latencies.load(Ordering::Acquire);
        let capture_frames = latencies >> 32;
        let playback_frames = u64::from(latencies as u32);
        self.render.process(
            [left.as_mut_slice(scope), right.as_mut_slice(scope)],
            &input[..channels],
            cycle.entered,
            Timing {
                capture_latency: frames_to_duration(capture_frames, self.render.rate),
                playback_latency: frames_to_duration(playback_frames, self.render.rate),
                discontinuous,
            },
        );
        Control::Continue
    }

    fn buffer_size(&mut self, _: &Client, frames: jack::Frames) -> Control {
        self.signals.period.store(frames, Ordering::Relaxed);
        self.signals.graph_changed.store(true, Ordering::Release);
        Control::Continue
    }
}

pub(crate) struct JackStream<P: AudioProcess + 'static> {
    client: jack::AsyncClient<Notifications, Duplex<P>>,
    signals: Arc<Signals>,
    route: Route,
    capture: Vec<String>,
    capture_sources: Vec<JackCaptureChannel>,
    playback: [String; 2],
    next_check: Instant,
}

impl<P: AudioProcess + 'static> JackStream<P> {
    pub(crate) fn open() -> Result<(Client, StreamParams), AudioError> {
        let client = open(&client_name())?;
        let params = StreamParams {
            sample_rate: client.sample_rate(),
            max_frames: client.buffer_size(),
            channels: 2,
            workgroup: None,
        };
        if !crate::backend::valid_buffer_frames(params.max_frames)
            || !crate::backend::valid_sample_rate(params.sample_rate)
        {
            return Err(AudioError::UnsupportedConfig(
                "invalid JACK server timing".into(),
            ));
        }
        Ok((client, params))
    }

    pub(crate) fn start(
        client: Client,
        params: &StreamParams,
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
        let capture_ports = input.iter().map(|port| port.raw() as usize).collect();
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
            sample_rate: AtomicU32::new(params.sample_rate),
            period: AtomicU32::new(params.max_frames),
            ..Signals::default()
        });
        let client = client
            .activate_async(
                Notifications {
                    signals: Arc::clone(&signals),
                    stats,
                    rate: params.sample_rate,
                    capture_ports,
                },
                Duplex {
                    render: PeriodRenderer::new(slot, params, route.capture_channels),
                    input,
                    output,
                    signals: Arc::clone(&signals),
                    previous: None,
                    xrun_scope,
                },
            )
            .map_err(jack_error)?;
        let mut stream = Self {
            client,
            signals,
            route,
            capture,
            capture_sources: Vec::new(),
            playback,
            next_check: Instant::now(),
        };
        stream.reconcile();
        Ok(stream)
    }

    // All graph enumeration and link mutations take place on the control thread.
    fn reconcile(&mut self) {
        let client = self.client.as_client();
        let before = self.signals.capture.snapshot();
        let graph = graph(client);
        let desired = self.route.targets(&default_targets(client, &self.route));
        let mut ready = !desired.capture.is_empty();
        let mut capture_latency = 0;
        self.capture_sources.clear();
        for (index, local) in self.capture.iter().enumerate() {
            let target = desired.capture.get(index).map(String::as_str);
            let latency = reconcile_port(
                client,
                local,
                target,
                JackDirection::Capture,
                &graph,
                Some(&self.signals),
            );
            if let Some(name) = target {
                self.capture_sources
                    .push(capture_channel(name, &graph, latency.is_some()));
            }
            if self.route.capture.is_some() || target.is_some() {
                ready &= latency.is_some();
                capture_latency = capture_latency.max(latency.unwrap_or(0));
            }
        }
        let mut playback_latency = 0;
        for (local, target) in self.playback.iter().zip(&desired.playback) {
            let latency = reconcile_port(
                client,
                local,
                target.as_deref(),
                JackDirection::Playback,
                &graph,
                None,
            );
            playback_latency = playback_latency.max(latency.unwrap_or(0));
        }
        self.signals.port_latencies.store(
            (u64::from(capture_latency) << 32) | u64::from(playback_latency),
            Ordering::Release,
        );
        if !self.signals.capture.publish(before, ready) {
            self.signals.graph_changed.store(true, Ordering::Release);
        } else if let Some(trace) = &self.signals.trace
            && (before & CAPTURE_READY != 0) != ready
        {
            trace.note(if ready {
                TRACE_ROUTE_READY
            } else {
                TRACE_ROUTE_UNAVAILABLE
            });
        }
        self.next_check = Instant::now() + ROUTE_RECHECK;
    }

    pub(crate) fn channels(&self) -> u16 {
        if self.signals.capture.snapshot() & CAPTURE_READY != 0 {
            self.capture.len() as u16
        } else {
            0
        }
    }

    pub(crate) fn capture_sources(&self) -> &[JackCaptureChannel] {
        &self.capture_sources
    }

    pub(crate) fn period(&self) -> u32 {
        self.signals.period.load(Ordering::Relaxed)
    }

    pub(crate) fn latency(&self, rate: u32) -> Duration {
        frames_to_duration(
            u64::from(self.signals.port_latencies.load(Ordering::Acquire) as u32),
            rate,
        )
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.signals.sample_rate.load(Ordering::Acquire)
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

fn reconcile_port(
    client: &Client,
    local: &str,
    target: Option<&str>,
    direction: JackDirection,
    graph: &[JackPort],
    signals: Option<&Signals>,
) -> Option<u32> {
    let target = exact_target(target, direction, graph);
    let port = client.port_by_name(local)?;
    // A client/session manager can attach to our ports. Treat the route as exclusive, so capture
    // cannot accidentally mix with another link. Only touch links on ports we own.
    let connections = port.get_connections();
    if let Some(signals) = signals
        && signals.capture.snapshot() & CAPTURE_READY != 0
        && match target {
            Some(target) => connections.len() != 1 || connections[0] != target,
            None => !connections.is_empty(),
        }
    {
        // A ready channel lost its chosen link or picked up a second link. Latch the gap
        // before repairing it, even if the server never notified us.
        signals.capture.changed();
        if let Some(trace) = &signals.trace {
            trace.note(TRACE_LINK_REPAIR);
        }
    }
    for other in connections {
        if target != Some(other.as_str()) {
            let (source, sink) = edge(direction, local, &other);
            let _ = client.disconnect_ports_by_name(source, sink);
        }
    }
    let target = target?;
    if !port.is_connected_to(target).unwrap_or(false) {
        let (source, sink) = edge(direction, local, target);
        let _ = client.connect_ports_by_name(source, sink);
    }
    if !port.is_connected_to(target).unwrap_or(false) || port.connected_count().ok() != Some(1) {
        return None;
    }
    let latency_type = match direction {
        JackDirection::Capture => jack::LatencyType::Capture,
        JackDirection::Playback => jack::LatencyType::Playback,
    };
    Some(port.get_latency_range(latency_type).1)
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
            entered: time,
        };
        let at = |first_frame, frames, elapsed| Cycle {
            first_frame,
            frames,
            entered: time + frames_to_duration(elapsed, 48_000),
        };
        assert!(!previous.missed_before(at(1088, 64, 64), 48_000));
        assert!(!previous.missed_before(at(12_288, 64, 64), 48_000));
        assert!(!previous.missed_before(at(1, 64, 64), 48_000));
        assert!(!previous.missed_before(at(1280, 256, 256), 48_000));
        assert!(previous.missed_before(at(1152, 64, 128), 48_000));
    }

    #[test]
    fn pipewire_driver_xrun_does_not_discard_locally_continuous_capture() {
        let now = Instant::now();
        let previous = Cycle {
            first_frame: 4096,
            frames: 64,
            entered: now,
        };
        let received = Cycle {
            first_frame: 4160,
            frames: 64,
            entered: now + frames_to_duration(64, 48_000),
        };
        let skipped = Cycle {
            first_frame: 4224,
            frames: 64,
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
        let state = CaptureState::default();
        let start = state.snapshot();
        assert!(!state.take_period().0);
        assert!(state.publish(start, true));
        assert_eq!(state.take_period(), (true, false));

        let before_unplug = state.snapshot();
        state.changed();
        assert!(!state.publish(before_unplug, true));
        assert_eq!(state.take_period(), (false, true));
        assert_eq!(state.take_period(), (false, false));
        assert!(state.publish(state.snapshot(), true));
        assert_eq!(state.take_period(), (true, false));

        // A fast disconnect/reconnect between process callbacks still reports the lost interval.
        state.changed();
        assert!(state.publish(state.snapshot(), true));
        assert_eq!(state.take_period(), (true, true));
        assert_eq!(state.take_period(), (true, false));
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
            capture_latency: Duration::from_millis(1),
            playback_latency: Duration::from_millis(2),
            discontinuous: false,
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
                    discontinuous: gap,
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
