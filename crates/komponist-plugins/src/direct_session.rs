//! Transfers a direct CLAP audio half at a block boundary. The app's main thread owns
//! activation and destruction; the engine never waits for it or destroys audio buffers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use komponist_engine::processor::{EventProcessor, EventSink, ParameterEdit};
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_hosting::host_loop::notification_bits;
use komponist_hosting::{HostedPlugin, HostingError};
use komponist_ipc::shm::{INPUT_EVENT_CAPACITY, MAX_BLOCK};
use komponist_ipc::{RenderMode, WireEvent, WireEventKind};
use komponist_model::DeviceRole;
use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::direct::{DirectClap, DirectProcessor, write_fallback};
use crate::manager::ProcessorSetup;

struct Requested {
    setup: AtomicU64,
    revision: AtomicU64,
    prepared: AtomicU64,
    control_epoch: AtomicU64,
    latency: AtomicU32,
}

impl Requested {
    fn new(setup: ProcessorSetup, latency: u32) -> Self {
        Self {
            setup: AtomicU64::new(pack(setup)),
            revision: AtomicU64::new(0),
            prepared: AtomicU64::new(0),
            control_epoch: AtomicU64::new(0),
            latency: AtomicU32::new(latency),
        }
    }

    fn request_setup(&self, setup: ProcessorSetup) {
        let packed = pack(setup);
        if self.setup.swap(packed, Ordering::AcqRel) != packed {
            self.restart();
        }
    }

    fn restart(&self) {
        self.revision.fetch_add(1, Ordering::Release);
    }

    fn setup(&self) -> ProcessorSetup {
        let value = self.setup.load(Ordering::Acquire);
        ProcessorSetup {
            sample_rate: f32::from_bits((value >> 32) as u32),
            max_block: value as u32 as usize,
        }
    }
}

fn pack(setup: ProcessorSetup) -> u64 {
    (u64::from(setup.sample_rate.to_bits()) << 32) | setup.max_block as u64
}

struct Control {
    epoch: u64,
    wire: WireEvent,
}

struct Active {
    audio: DirectProcessor,
    revision: u64,
}

/// Main-thread ownership of the local plugin and its control state.
pub(crate) struct DirectSession {
    host: DirectClap,
    ready: Producer<Active>,
    returned: Consumer<Active>,
    control: Producer<Control>,
    requested: Arc<Requested>,
    role: DeviceRole,
    render_mode: RenderMode,
}

impl DirectSession {
    pub(crate) fn start(
        mut host: DirectClap,
        setup: ProcessorSetup,
        role: DeviceRole,
    ) -> Result<(Self, DirectEngine), HostingError> {
        let audio = host.activate(setup, role)?;
        let requested = Arc::new(Requested::new(setup, audio.latency()));
        let (ready, incoming) = RingBuffer::new(1);
        let (retire, returned) = RingBuffer::new(1);
        let (control, commands) = RingBuffer::new(INPUT_EVENT_CAPACITY);
        let session = Self {
            host,
            ready,
            returned,
            control,
            requested: requested.clone(),
            role,
            render_mode: RenderMode::Realtime,
        };
        let engine = DirectEngine {
            active: Some(Active { audio, revision: 0 }),
            incoming,
            retire,
            commands,
            controls: Vec::with_capacity(INPUT_EVENT_CAPACITY),
            control_epoch: 0,
            requested,
            role,
            offline_failures: 0,
            release_notes: false,
        };
        Ok((session, engine))
    }

    pub(crate) fn try_control<T>(
        &mut self,
        f: impl FnOnce(&mut dyn HostedPlugin) -> T,
    ) -> Option<T> {
        self.host.try_control(f)
    }

    pub(crate) fn restore<T>(
        &mut self,
        state: &[u8],
        inventory: impl FnOnce(&mut dyn HostedPlugin) -> T,
    ) -> Option<(Result<(), HostingError>, T)> {
        let requested = &self.requested;
        self.host.try_control(|plugin| {
            let result = plugin.load_state(state);
            if result.is_ok() {
                requested.control_epoch.fetch_add(1, Ordering::Release);
            }
            (result, inventory(plugin))
        })
    }

    pub(crate) fn set_param(&mut self, id: u32, value: f64) -> bool {
        self.control
            .push(Control {
                epoch: self.requested.control_epoch.load(Ordering::Acquire),
                wire: WireEvent::new(0, WireEventKind::ParamValue { id, value }),
            })
            .is_ok()
    }

    pub(crate) fn latency(&self) -> u32 {
        self.requested.latency.load(Ordering::Acquire)
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.requested.prepared.load(Ordering::Acquire)
            == self.requested.revision.load(Ordering::Acquire)
    }

    /// Finish retiring the instance once the engine has dropped its endpoint. The engine's
    /// garbage queue keeps destruction out of the RT path.
    pub(crate) fn retired(&mut self) -> bool {
        if !self.returned.is_abandoned() {
            return false;
        }
        if let Ok(active) = self.returned.pop() {
            self.host.deactivate(active.audio);
        }
        true
    }

    pub(crate) fn set_render_mode(&mut self, mode: RenderMode) {
        if self.render_mode != mode {
            self.render_mode = mode;
            self.requested.restart();
        }
    }

    pub(crate) fn poll(&mut self) -> Result<komponist_hosting::host_loop::Observed, HostingError> {
        let observed = self.host.poll();
        if observed.has(notification_bits::RESTART_REQUESTED) {
            self.requested.restart();
        }
        if observed.has(notification_bits::LATENCY_CHANGED)
            && let Some(latency) = self.host.try_control(|plugin| plugin.latency())
        {
            self.requested.latency.store(latency, Ordering::Release);
        }
        let Ok(returned) = self.returned.pop() else {
            return Ok(observed);
        };
        self.host.deactivate(returned.audio);
        if self.ready.is_abandoned() {
            return Ok(observed);
        }
        let revision = self.requested.revision.load(Ordering::Acquire);
        // The returned processor was stopped by the engine. With no audio half in flight, the
        // instance can switch render mode and configuration on the main thread.
        self.host
            .try_control(|plugin| plugin.set_render_mode(self.render_mode))
            .ok_or(HostingError::WrongState("returned CLAP instance is busy"))??;
        let audio = self.host.activate(self.requested.setup(), self.role)?;
        self.requested
            .latency
            .store(audio.latency(), Ordering::Release);
        if let Err(PushError::Full(active)) = self.ready.push(Active { audio, revision }) {
            self.host.deactivate(active.audio);
            return Err(HostingError::WrongState("CLAP activation queue is full"));
        }
        self.requested.prepared.store(revision, Ordering::Release);
        Ok(observed)
    }

    pub(crate) fn finish(mut self, mut engine: DirectEngine) -> DirectClap {
        // Only one activation exists. It can be with the engine or waiting in one queue.
        if let Some(active) = engine
            .active
            .take()
            .or_else(|| engine.incoming.pop().ok())
            .or_else(|| self.returned.pop().ok())
        {
            self.host.deactivate(active.audio);
        }
        self.host
    }
}

/// An engine-owned slot for the current audio half. The queues are fixed-capacity and an
/// activation is sent back before the main thread installs its replacement.
pub(crate) struct DirectEngine {
    active: Option<Active>,
    incoming: Consumer<Active>,
    retire: Producer<Active>,
    commands: Consumer<Control>,
    controls: Vec<WireEvent>,
    control_epoch: u64,
    requested: Arc<Requested>,
    role: DeviceRole,
    offline_failures: u64,
    release_notes: bool,
}

impl DirectEngine {
    fn current(&mut self) -> Option<&mut DirectProcessor> {
        if self.active.is_none() {
            self.active = self.incoming.pop().ok();
        }
        if self.active.as_ref()?.revision != self.requested.revision.load(Ordering::Acquire) {
            let mut old = self.active.take().expect("checked active processor");
            old.audio.stop();
            let missed = old.audio.offline_failures();
            match self.retire.push(old) {
                Ok(()) => {
                    self.offline_failures = self.offline_failures.saturating_add(missed);
                    self.release_notes |= self.role == DeviceRole::MidiEffect;
                }
                Err(PushError::Full(old)) => self.active = Some(old),
            }
            return None;
        }
        self.active.as_mut().map(|active| &mut active.audio)
    }

    fn collect_controls(&mut self) {
        let epoch = self.requested.control_epoch.load(Ordering::Acquire);
        if epoch != self.control_epoch {
            self.controls.clear();
            self.control_epoch = epoch;
        }
        while self.controls.len() < self.controls.capacity() {
            let Ok(command) = self.commands.pop() else {
                break;
            };
            if command.epoch == self.control_epoch {
                self.controls.push(command.wire);
            }
        }
    }

    fn audio(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: Option<[&[f32]; 2]>,
    ) -> ProcessStatus {
        if self.current().is_some() {
            self.collect_controls();
            let audio = &mut self
                .active
                .as_mut()
                .expect("checked active processor")
                .audio;
            let controls_fit =
                self.controls.len().saturating_add(events.len()) <= INPUT_EVENT_CAPACITY;
            let controls = if controls_fit {
                self.controls.as_slice()
            } else {
                &[]
            };
            let status = audio.process_block(
                ctx,
                events,
                controls,
                io,
                sidechain,
                Some((&self.requested.control_epoch, self.control_epoch)),
            );
            if audio.delivered() && controls_fit {
                self.controls.clear();
            }
            return status;
        }
        if ctx.offline {
            self.offline_failures += 1;
        }
        write_fallback(self.role, io);
        ProcessStatus::Continue
    }
}

impl Drop for DirectEngine {
    fn drop(&mut self) {
        if let Some(mut active) = self.active.take().or_else(|| self.incoming.pop().ok()) {
            active.audio.stop();
            // The two rings and the engine share exactly one audio activation.
            assert!(
                self.retire.push(active).is_ok(),
                "direct return queue is empty"
            );
        }
    }
}

impl Processor for DirectEngine {
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        if !(1..=MAX_BLOCK).contains(&max_block) {
            return Err(ProcessorError::BlockTooLarge(max_block));
        }
        self.requested.request_setup(ProcessorSetup {
            sample_rate,
            max_block,
        });
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        self.audio(ctx, events, io, None)
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        self.audio(ctx, events, io, Some(sidechain))
    }

    fn latency(&self) -> u32 {
        self.requested.latency.load(Ordering::Acquire)
    }

    fn offline_failures(&self) -> u64 {
        self.offline_failures
            + self
                .active
                .as_ref()
                .map_or(0, |active| active.audio.offline_failures())
    }

    fn reset(&mut self) {
        self.release_notes = false;
        if let Some(active) = self.active.as_mut() {
            Processor::reset(&mut active.audio);
        }
    }

    fn pop_parameter_edit(&mut self) -> Option<ParameterEdit> {
        self.active.as_mut()?.audio.pop_parameter_edit()
    }

    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor> {
        (self.role == DeviceRole::MidiEffect).then_some(self)
    }
}

impl EventProcessor for DirectEngine {
    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(stage) = self.current().and_then(Processor::event_processor) {
            stage.set_param(id, value);
        }
    }

    fn process_events(&mut self, ctx: &ProcessContext, input: &[Event], out: &mut EventSink<'_>) {
        if self.release_notes {
            if !out.push(0, EventKind::AllNotesOff) {
                return;
            }
            self.release_notes = false;
        }
        if self.current().is_some() {
            self.collect_controls();
            let audio = &mut self
                .active
                .as_mut()
                .expect("checked active processor")
                .audio;
            if let Some(stage) = audio.event_processor() {
                for event in self.controls.drain(..) {
                    if let Some(WireEventKind::ParamValue { id, value }) = event.decode() {
                        stage.set_param(id, value);
                    }
                }
                stage.process_events(ctx, input, out);
            }
        } else {
            self.release_notes = !out.push(0, EventKind::AllNotesOff);
            if ctx.offline {
                self.offline_failures += 1;
            }
        }
    }

    fn reset(&mut self) {
        Processor::reset(self);
    }
}
