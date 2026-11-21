//! Native CLAP audio dispatch for already verified bundles.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    processing: Arc<AtomicBool>,
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
            processing: Arc::new(AtomicBool::new(false)),
            _bundle: bundle,
        })
    }

    /// Main-thread operations that cannot overlap processing. The owner can retry on its next
    /// tick if an engine worker is in a plugin call.
    pub(crate) fn try_control<T>(
        &mut self,
        control: impl FnOnce(&mut dyn HostedPlugin) -> T,
    ) -> Option<T> {
        let _guard = ProcessingGuard::try_enter(&self.processing)?;
        Some(control(&mut *self.plugin))
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
            processing: self.processing.clone(),
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
            reset_pending: false,
            offline_failures: 0,
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

struct ProcessingGuard<'a>(&'a AtomicBool);

impl<'a> ProcessingGuard<'a> {
    fn try_enter(processing: &'a AtomicBool) -> Option<Self> {
        processing
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(processing))
    }
}

impl Drop for ProcessingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The audio half of the instance can move to an engine worker. The main-thread instance
/// must remain active until this processor is returned to it for deactivation.
pub(crate) struct DirectProcessor {
    audio: Box<dyn HostedProcessor>,
    processing: Arc<AtomicBool>,
    setup: ProcessorSetup,
    role: DeviceRole,
    latency: u32,
    input_events: Vec<WireEvent>,
    output_events: Vec<WireEvent>,
    output_len: usize,
    output_read: usize,
    sample_clock: u64,
    reset_pending: bool,
    offline_failures: u64,
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
        let start = self.sample_clock;
        self.sample_clock = self.sample_clock.saturating_add(u64::from(ctx.frames));
        let Some(_guard) = ProcessingGuard::try_enter(&self.processing) else {
            // A main-thread state operation owns the plugin. Missing a note-off must not leave
            // voices held when processing resumes.
            self.reset_pending |= self.role == DeviceRole::Instrument;
            if ctx.offline {
                self.offline_failures += 1;
            }
            write_fallback(self.role, io);
            return ProcessStatus::Continue;
        };
        if std::mem::take(&mut self.reset_pending) {
            self.audio.reset();
        }
        if events.len() > self.input_events.capacity() {
            // A dropped note-off can leave a voice active. Reset and fail the slot rather than
            // send a partial block whose state the engine cannot reproduce.
            self.audio.reset();
            write_fallback(self.role, io);
            return ProcessStatus::Failed;
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
            steady_time: start,
        };
        let result = self.audio.process(&mut block);
        self.output_len = block.events_out.as_slice().len();
        if result == komponist_hosting::ProcessStatus::Error {
            self.output_len = 0;
            write_fallback(self.role, io);
            return ProcessStatus::Failed;
        }
        ProcessStatus::Continue
    }
}

fn write_fallback(role: DeviceRole, io: &mut StereoIo<'_>) {
    if role == DeviceRole::Instrument {
        for channel in &mut io.output {
            channel.fill(0.0);
        }
    } else {
        io.passthrough();
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

    fn offline_failures(&self) -> u64 {
        self.offline_failures
    }

    fn reset(&mut self) {
        self.reset_pending = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processing_permission_never_blocks_the_other_caller() {
        let processing = AtomicBool::new(false);
        let guard = ProcessingGuard::try_enter(&processing).unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| assert!(ProcessingGuard::try_enter(&processing).is_none()))
                .join()
                .unwrap();
        });
        drop(guard);
        assert!(ProcessingGuard::try_enter(&processing).is_some());
    }
}
