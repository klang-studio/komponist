//! Native CLAP audio dispatch for already verified bundles.

use std::rc::Rc;
use std::time::Duration;

use komponist_engine::processor::{ParameterEdit, ParameterEditKind};
use komponist_engine::{Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo};
use komponist_hosting::host_loop::PollLoop;
use komponist_hosting::{
    ActivateConfig, EventOut, HostedPlugin, HostedProcessor, HostingError, ProcessBlock,
};
use komponist_ipc::shm::{INPUT_EVENT_CAPACITY, MAX_BLOCK, OUTPUT_EVENT_CAPACITY};
use komponist_ipc::{PluginFormat, WireEvent, WireEventKind};
use komponist_model::DeviceRole;

use crate::bundled_clap::VerifiedClap;
use crate::external::{to_wire, wire_transport};
use crate::manager::ProcessorSetup;

/// The main-thread half of a CLAP instance. Keeping the token alive also keeps the verified
/// bundle available to the plugin while its library and resources are in use.
pub(crate) struct DirectClap {
    plugin: Box<dyn HostedPlugin>,
    callbacks: Rc<PollLoop>,
    _bundle: VerifiedClap,
}

impl DirectClap {
    pub(crate) fn load(bundle: VerifiedClap) -> Result<Self, HostingError> {
        let callbacks = PollLoop::new(false)?;
        let plugin = komponist_hosting::load(
            PluginFormat::Clap,
            bundle.bundle_path(),
            bundle.plugin_id(),
            callbacks.context(),
        )?;
        Ok(Self {
            plugin,
            callbacks,
            _bundle: bundle,
        })
    }

    pub(crate) fn plugin(&self) -> &dyn HostedPlugin {
        &*self.plugin
    }

    pub(crate) fn plugin_mut(&mut self) -> &mut dyn HostedPlugin {
        &mut *self.plugin
    }

    pub(crate) fn activate(
        &mut self,
        setup: ProcessorSetup,
        role: DeviceRole,
    ) -> Result<DirectProcessor, HostingError> {
        let max_frames = u32::try_from(setup.max_block)
            .map_err(|error| HostingError::Activate(error.to_string()))?;
        let mut audio = self.plugin.activate(ActivateConfig {
            sample_rate: f64::from(setup.sample_rate),
            min_frames: 1,
            max_frames,
        })?;
        if let Err(error) = audio.start_processing() {
            self.plugin.deactivate(audio);
            return Err(error);
        }
        Ok(DirectProcessor {
            audio,
            setup,
            role,
            latency: self.plugin.latency(),
            input_events: Vec::with_capacity(INPUT_EVENT_CAPACITY),
            output_events: vec![
                WireEvent::new(0, WireEventKind::AllNotesOff);
                OUTPUT_EVENT_CAPACITY
            ],
            output_len: 0,
            output_read: 0,
            sample_clock: 0,
        })
    }

    pub(crate) fn deactivate(&mut self, processor: DirectProcessor) {
        let mut audio = processor.audio;
        audio.stop_processing();
        self.plugin.deactivate(audio);
    }

    pub(crate) fn poll(&mut self) {
        self.callbacks.run_once(&mut *self.plugin, Duration::ZERO);
    }
}

/// The audio half of the instance can move to an engine worker. The main-thread instance
/// must remain active until this processor is returned to it for deactivation.
pub(crate) struct DirectProcessor {
    audio: Box<dyn HostedProcessor>,
    setup: ProcessorSetup,
    role: DeviceRole,
    latency: u32,
    input_events: Vec<WireEvent>,
    output_events: Vec<WireEvent>,
    output_len: usize,
    output_read: usize,
    sample_clock: u64,
}

impl DirectProcessor {
    fn process_block(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: Option<[&[f32]; 2]>,
    ) -> ProcessStatus {
        self.output_read = 0;
        self.output_len = 0;
        if events.len() > self.input_events.capacity() {
            // A dropped note-off can leave a voice active. Reset and fail the slot rather than
            // send a partial block whose state the engine cannot reproduce.
            self.audio.reset();
            return self.failed(io);
        }
        self.input_events.clear();
        self.input_events.extend(events.iter().map(to_wire));
        let transport = wire_transport(&ctx.transport);
        let [left, right] = &mut io.output;
        let mut block = ProcessBlock {
            frames: ctx.frames,
            input: io.input,
            sidechain,
            output: [left, right],
            events_in: &self.input_events,
            events_out: EventOut::new(&mut self.output_events),
            transport: &transport,
            steady_time: self.sample_clock,
        };
        let result = self.audio.process(&mut block);
        self.sample_clock = self.sample_clock.saturating_add(u64::from(ctx.frames));
        self.output_len = block.events_out.as_slice().len();
        if result == komponist_hosting::ProcessStatus::Error {
            self.output_len = 0;
            return self.failed(io);
        }
        ProcessStatus::Continue
    }

    fn failed(&self, io: &mut StereoIo<'_>) -> ProcessStatus {
        if self.role == DeviceRole::Instrument {
            for channel in &mut io.output {
                channel.fill(0.0);
            }
        } else {
            io.passthrough();
        }
        ProcessStatus::Failed
    }
}

impl Processor for DirectProcessor {
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        if !(1..=MAX_BLOCK).contains(&max_block) {
            return Err(ProcessorError::BlockTooLarge(max_block));
        }
        if self.setup
            != (ProcessorSetup {
                sample_rate,
                max_block,
            })
        {
            return Err(ProcessorError::Failed(
                "CLAP rate and block size changes require main-thread reactivation".into(),
            ));
        }
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        self.process_block(ctx, events, io, None)
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        self.process_block(ctx, events, io, Some(sidechain))
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn reset(&mut self) {
        self.audio.reset();
        self.output_read = 0;
        self.output_len = 0;
    }

    fn pop_parameter_edit(&mut self) -> Option<ParameterEdit> {
        while self.output_read < self.output_len {
            let event = self.output_events[self.output_read];
            self.output_read += 1;
            let kind = match event.decode() {
                Some(WireEventKind::ParamValue { id, value }) if value.is_finite() => {
                    ParameterEditKind::Value { id, value }
                }
                Some(WireEventKind::ParamGesture { id, begin }) => {
                    ParameterEditKind::Gesture { id, begin }
                }
                _ => continue,
            };
            return Some(ParameterEdit {
                offset: event.offset,
                kind,
            });
        }
        None
    }
}
