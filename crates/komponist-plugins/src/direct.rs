//! Native CLAP audio dispatch for already verified bundles.

use std::cell::UnsafeCell;
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use komponist_engine::processor::{EventSink, ParameterEdit, ParameterEditKind};
use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_hosting::host_loop::{Observed, PollLoop, notification_bits};
use komponist_hosting::{
    ActivateConfig, EventOut, HostedPlugin, HostedProcessor, HostingError, ProcessBlock,
};
use komponist_ipc::device_assets::{Bindings, OfflineRequest, OfflineStatus, Snapshot, edit_state};
use komponist_ipc::shm::{INPUT_EVENT_CAPACITY, MAX_BLOCK, OUTPUT_EVENT_CAPACITY};
use komponist_ipc::{PluginFormat, WireEvent, WireEventKind};
use komponist_model::DeviceRole;

use crate::bundled_clap::VerifiedClap;
use crate::bundled_effects;
use crate::external::{midi_event_kind, to_wire, wire_transport};
use crate::manager::ProcessorSetup;

pub(crate) const METADATA_REFRESH: u32 =
    notification_bits::PARAMS_CHANGED | notification_bits::NOTE_NAMES_CHANGED;
const CONTROL_REFRESH: u32 = METADATA_REFRESH | notification_bits::LATENCY_CHANGED;

/// The last successful delivery of a durable edit, indexed by CLAP parameter ID.
#[derive(Clone, Copy)]
pub(crate) struct DeliveredEdit {
    serial: NonZeroU32,
    load_epoch: u64,
}

impl DeliveredEdit {
    /// Older deliveries were superseded. A delivery in the reporting epoch can be
    /// settled to the plugin's final value, unless playback currently owns the control.
    pub(crate) fn settles_at(self, edit: NonZeroU32, preset_epoch: u64, driven: bool) -> bool {
        self.serial == edit
            && (self.load_epoch < preset_epoch || self.load_epoch == preset_epoch && !driven)
    }
}

type DeliveredEdits = BTreeMap<u32, Option<DeliveredEdit>>;

#[derive(Default)]
struct EditDelivery(UnsafeCell<DeliveredEdits>);

// SAFETY: Both sides access this table under the instance's ProcessingGuard,
// or before the engine receives its first processor.
unsafe impl Sync for EditDelivery {}

/// The main-thread half of a CLAP instance. Keeping the token alive also keeps the verified
/// bundle available to the plugin while its library and resources are in use.
pub(crate) struct DirectClap {
    plugin: Box<dyn HostedPlugin>,
    callbacks: Rc<PollLoop>,
    processing: Arc<AtomicBool>,
    edit_delivery: Arc<EditDelivery>,
    refresh: PendingRefresh,
    signal_taps: Option<Arc<crate::signal_taps::SignalTaps>>,
    bundle: VerifiedClap,
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
        let signal_taps =
            crate::signal_taps::SignalTaps::new(plugin.signal_taps(), 48_000.0).map(Arc::new);
        Ok(Self {
            plugin,
            callbacks,
            processing: Arc::new(AtomicBool::new(false)),
            edit_delivery: Arc::default(),
            refresh: PendingRefresh::default(),
            signal_taps,
            bundle,
        })
    }

    pub(crate) fn is_from_bundle(&self, path: &std::path::Path) -> bool {
        self.bundle.original_path() == path
    }

    pub(crate) fn signal_tap(&self, key: &str) -> Option<&crate::signal_taps::SignalTap> {
        self.signal_taps.as_deref()?.get(key)
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

    pub(crate) fn try_read_preset_values<T>(
        &mut self,
        read: impl FnOnce(&mut dyn HostedPlugin, &DeliveredEdits) -> T,
    ) -> Option<T> {
        let _guard = ProcessingGuard::try_enter(&self.processing)?;
        // SAFETY: The guard excludes processing and all updates of the delivery table.
        Some(read(&mut *self.plugin, unsafe {
            &*self.edit_delivery.0.get()
        }))
    }

    pub(crate) fn try_refresh<T>(
        &mut self,
        mask: u32,
        read: impl FnOnce(&mut dyn HostedPlugin, u32) -> T,
    ) -> Option<T> {
        self.refresh.try_read(&self.processing, mask, |pending| {
            if pending & notification_bits::PARAMS_CHANGED != 0 {
                // SAFETY: try_read holds the processing guard.
                let delivered = unsafe { &mut *self.edit_delivery.0.get() };
                Self::refresh_delivery_slots(&*self.plugin, delivered);
            }
            read(&mut *self.plugin, pending)
        })
    }

    pub(crate) fn clear_refresh(&mut self, mask: u32) {
        self.refresh.0 &= !mask;
    }

    pub(crate) fn device_display(&self) -> Option<komponist_ipc::device_canvas::DisplayList> {
        self.plugin.device_display()
    }

    /// Host-side data only: the plugin is not called, so processing need not stop.
    pub(crate) fn take_preset_reports(&mut self) -> Vec<komponist_ipc::clap_presets::PresetReport> {
        self.plugin.take_preset_reports()
    }

    pub(crate) fn asset_slots(&self) -> (Option<Snapshot>, bool, bool, bool) {
        (
            self.plugin.asset_slots(),
            self.plugin.supports_asset_slots(),
            self.plugin.supports_offline_preparation(),
            self.plugin.supports_asset_edit_state(),
        )
    }

    pub(crate) fn prepare_asset_edit_state(
        &self,
        edits: &edit_state::Edits,
        state: Option<&[u8]>,
    ) -> edit_state::PreparedChange {
        self.plugin.prepare_asset_edit_state(edits, state)
    }

    pub(crate) fn set_asset_bindings(&mut self, bindings: &Bindings) -> bool {
        self.plugin.set_asset_bindings(bindings)
    }

    pub(crate) fn prepare_offline(
        &mut self,
        request: Option<OfflineRequest>,
    ) -> Option<OfflineStatus> {
        self.try_control(|plugin| plugin.prepare_offline(request))
            .unwrap_or(Some(OfflineStatus::Preparing))
    }

    pub(crate) fn waveform_markers(
        &self,
        slot: u32,
    ) -> Option<komponist_ipc::waveform_markers::Snapshot> {
        self.plugin.waveform_markers(slot)
    }

    pub(crate) fn preview_waveform(&self, slot: u32, time: Option<f64>) {
        self.plugin.preview_waveform(slot, time);
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
        let latency = self.plugin.latency();
        self.clear_refresh(notification_bits::LATENCY_CHANGED);
        let signal_taps = self.signal_taps.as_ref().map(|taps| {
            taps.set_rate(setup.sample_rate);
            (
                Arc::clone(taps),
                Box::new(komponist_ipc::signal_taps::BlockTaps::EMPTY),
            )
        });
        // SAFETY: On activation no audio half is being called by the engine.
        Self::refresh_delivery_slots(&*self.plugin, unsafe { &mut *self.edit_delivery.0.get() });
        Ok(DirectProcessor {
            audio,
            processing: self.processing.clone(),
            edit_delivery: self.edit_delivery.clone(),
            preset_load_epoch: self.plugin.preset_load_epoch(),
            setup,
            role,
            latency,
            input_events: Vec::with_capacity(INPUT_EVENT_CAPACITY),
            output_events: vec![
                WireEvent::new(0, WireEventKind::AllNotesOff);
                OUTPUT_EVENT_CAPACITY
            ],
            output_len: 0,
            dropped: 0,
            delivered: false,
            output_read: 0,
            sample_clock: 0,
            reset_pending: false,
            offline_failures: 0,
            signal_taps,
            midi: (role == DeviceRole::MidiEffect).then(|| MidiStage::new(setup.max_block)),
        })
    }

    fn refresh_delivery_slots(plugin: &dyn HostedPlugin, delivered: &mut DeliveredEdits) {
        let mut old = std::mem::take(delivered);
        delivered.extend(
            plugin
                .params()
                .into_iter()
                .map(|param| (param.id, old.remove(&param.id).flatten())),
        );
    }

    pub(crate) fn deactivate(&mut self, processor: DirectProcessor) {
        let mut audio = processor.audio;
        audio.stop_processing();
        self.plugin.deactivate(audio);
    }

    pub(crate) fn poll(&mut self) -> Observed {
        self.callbacks.run_once(&mut *self.plugin, Duration::ZERO);
        let observed = self.callbacks.take_observed();
        self.refresh.0 |= observed.notifications & CONTROL_REFRESH;
        observed
    }
}

#[derive(Default)]
struct PendingRefresh(u32);

impl PendingRefresh {
    fn try_read<T>(
        &mut self,
        processing: &AtomicBool,
        mask: u32,
        read: impl FnOnce(u32) -> T,
    ) -> Option<T> {
        let pending = self.0 & mask;
        if pending == 0 {
            return None;
        }
        // The host loop has already drained this notification. Retain it while audio is busy.
        let _guard = ProcessingGuard::try_enter(processing)?;
        let result = read(pending);
        self.0 &= !pending;
        Some(result)
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
    edit_delivery: Arc<EditDelivery>,
    preset_load_epoch: Option<Arc<AtomicU64>>,
    setup: ProcessorSetup,
    role: DeviceRole,
    latency: u32,
    input_events: Vec<WireEvent>,
    output_events: Vec<WireEvent>,
    output_len: usize,
    dropped: u32,
    delivered: bool,
    output_read: usize,
    sample_clock: u64,
    reset_pending: bool,
    offline_failures: u64,
    signal_taps: Option<(
        Arc<crate::signal_taps::SignalTaps>,
        Box<komponist_ipc::signal_taps::BlockTaps>,
    )>,
    midi: Option<MidiStage>,
}

struct MidiStage {
    audio: [Vec<f32>; 2],
    edits: VecDeque<ParameterEdit>,
    downstream_active: bool,
    release_due: bool,
}

impl MidiStage {
    fn new(max_block: usize) -> Self {
        Self {
            audio: std::array::from_fn(|_| vec![0.0; max_block]),
            edits: VecDeque::with_capacity(OUTPUT_EVENT_CAPACITY),
            downstream_active: false,
            release_due: false,
        }
    }
}

/// Host controls go before stage automation at the same frame. Both belong to the
/// same restore epoch and are checked while holding the processing permission.
#[derive(Default)]
pub(crate) struct ControlEvents<'a> {
    pub(crate) persistent: &'a [WireEvent],
    pub(crate) stage: &'a [WireEvent],
    pub(crate) epoch: Option<(&'a AtomicU64, u64)>,
}

impl DirectProcessor {
    pub(crate) fn delivered(&self) -> bool {
        self.delivered
    }

    pub(crate) fn stop(&mut self) {
        self.audio.stop_processing();
    }

    pub(crate) fn process_block(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        controls: ControlEvents<'_>,
        io: &mut StereoIo<'_>,
        sidechain: Option<[&[f32]; 2]>,
        meter: Option<&bundled_effects::Meter>,
    ) -> ProcessStatus {
        self.output_read = 0;
        self.output_len = 0;
        self.dropped = 0;
        self.delivered = false;
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
        let controls = if controls
            .epoch
            .is_some_and(|(epoch, stamp)| epoch.load(Ordering::Acquire) != stamp)
        {
            ControlEvents::default()
        } else {
            controls
        };
        if std::mem::take(&mut self.reset_pending) {
            self.audio.reset();
        }
        if events.len() + controls.persistent.len() + controls.stage.len()
            > self.input_events.capacity()
        {
            // A dropped note-off can leave a voice active. Reset and fail the slot rather than
            // send a partial block whose state the engine cannot reproduce.
            self.audio.reset();
            write_fallback(self.role, io);
            return ProcessStatus::Failed;
        }
        self.input_events.clear();
        self.input_events.extend_from_slice(controls.persistent);
        self.input_events.extend_from_slice(controls.stage);
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
        let meter_requested = meter.is_some_and(|meter| {
            if meter.requests_reduction() {
                self.audio
                    .begin_effect_meter(meter.requests_detector(ctx.frames as usize));
                true
            } else {
                false
            }
        });
        let tap_mask = self
            .signal_taps
            .as_ref()
            .filter(|_| !ctx.offline)
            .map_or(0, |(taps, _)| taps.begin(ctx.frames as usize));
        if tap_mask != 0 {
            self.audio.begin_signal_taps(tap_mask);
        }
        let result = self.audio.process(&mut block);
        let readings = meter_requested
            .then(|| self.audio.end_effect_meter())
            .flatten();
        self.output_len = block.events_out.as_slice().len();
        self.dropped = block.events_out.dropped();
        if result == komponist_hosting::ProcessStatus::Error {
            self.output_len = 0;
            write_fallback(self.role, io);
            return ProcessStatus::Failed;
        }
        if let Some(epoch) = &self.preset_load_epoch {
            // A block overlapping the notification belongs to the new epoch.
            let load_epoch = epoch.load(Ordering::Acquire);
            // SAFETY: The processing guard covers this whole block.
            let delivered = unsafe { &mut *self.edit_delivery.0.get() };
            for event in &self.input_events {
                if let Some(serial) = NonZeroU32::new(event.reserved)
                    && let Some(WireEventKind::ParamValue { id, .. }) = event.decode()
                    && let Some(slot) = delivered.get_mut(&id)
                {
                    *slot = Some(DeliveredEdit { serial, load_epoch });
                }
            }
        }
        if let Some(meter) = meter {
            meter.record(io, readings);
        }
        if tap_mask != 0
            && let Some((taps, payload)) = &mut self.signal_taps
        {
            payload.clear();
            self.audio.read_signal_taps(tap_mask, payload);
            taps.record(payload);
        }
        self.delivered = true;
        ProcessStatus::Continue
    }
}

pub(crate) fn write_fallback(role: DeviceRole, io: &mut StereoIo<'_>) {
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
        self.process_block(ctx, events, ControlEvents::default(), io, None, None)
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        self.process_block(
            ctx,
            events,
            ControlEvents::default(),
            io,
            Some(sidechain),
            None,
        )
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn offline_failures(&self) -> u64 {
        self.offline_failures
    }

    fn reset(&mut self) {
        if let Some(midi) = self.midi.as_mut() {
            midi.downstream_active = false;
            midi.release_due = false;
        }
        self.reset_pending = true;
        self.output_read = 0;
        self.output_len = 0;
    }

    fn pop_parameter_edit(&mut self) -> Option<ParameterEdit> {
        if let Some(midi) = self.midi.as_mut() {
            return midi.edits.pop_front();
        }
        while self.output_read < self.output_len {
            let event = self.output_events[self.output_read];
            self.output_read += 1;
            if let Some(edit) = parameter_edit(&event, 0) {
                return Some(edit);
            }
        }
        None
    }
}

fn parameter_edit(event: &WireEvent, base: u32) -> Option<ParameterEdit> {
    let kind = match event.decode()? {
        WireEventKind::ParamValue { id, value } if value.is_finite() => {
            ParameterEditKind::Value { id, value }
        }
        WireEventKind::ParamGesture { id, begin } => ParameterEditKind::Gesture { id, begin },
        _ => return None,
    };
    Some(ParameterEdit {
        offset: base.saturating_add(event.offset),
        kind,
    })
}

impl DirectProcessor {
    pub(crate) fn process_midi_block(
        &mut self,
        ctx: &ProcessContext,
        input: &[Event],
        output: &mut EventSink<'_>,
        controls: ControlEvents<'_>,
    ) {
        static SILENCE: [f32; MAX_BLOCK] = [0.0; MAX_BLOCK];
        let len = ctx.frames as usize;
        if len > self.setup.max_block {
            self.delivered = false;
            return;
        }
        let mut midi = self.midi.take().expect("MIDI effect has stage buffers");
        let [left, right] = &mut midi.audio;
        let status = self.process_block(
            ctx,
            input,
            controls,
            &mut StereoIo {
                input: [&SILENCE[..len]; 2],
                output: [&mut left[..len], &mut right[..len]],
            },
            None,
            None,
        );
        let events = &self.output_events[..self.output_len];
        let mut note_count = 0;
        for event in events {
            if let Some(edit) = parameter_edit(event, output.block_offset()) {
                if midi.edits.len() < midi.edits.capacity() {
                    midi.edits.push_back(edit);
                }
            } else if event.offset < ctx.frames && midi_event_kind(event).is_some() {
                note_count += 1;
            }
        }
        let complete = self.delivered
            && self.dropped == 0
            && note_count + usize::from(midi.release_due) <= output.room();
        if !complete {
            midi.release_due |= midi.downstream_active;
            self.reset_pending = true;
            if (self.delivered || status == ProcessStatus::Failed) && ctx.offline {
                self.offline_failures += 1;
            }
        }
        if midi.release_due && output.push(0, EventKind::AllNotesOff) {
            midi.release_due = false;
            midi.downstream_active = false;
        }
        if complete {
            for event in events {
                if event.offset < ctx.frames
                    && let Some(kind) = midi_event_kind(event)
                {
                    output.push(event.offset, kind);
                    match kind {
                        EventKind::NoteOn { .. } => midi.downstream_active = true,
                        EventKind::AllNotesOff => midi.downstream_active = false,
                        _ => {}
                    }
                }
            }
        }
        self.midi = Some(midi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notification_bits::{LATENCY_CHANGED, NOTE_NAMES_CHANGED, PARAMS_CHANGED};

    #[test]
    fn a_preset_settles_matching_older_edits_and_unowned_overlaps() {
        let early = NonZeroU32::new(1).unwrap();
        let repeat = NonZeroU32::new(2).unwrap();
        let delivered = DeliveredEdit {
            serial: early,
            load_epoch: 4,
        };

        assert!(delivered.settles_at(early, 5, true));
        assert!(!delivered.settles_at(repeat, 5, false));
        assert!(delivered.settles_at(early, 4, false));
        assert!(!delivered.settles_at(early, 4, true));
        assert!(!delivered.settles_at(early, 3, false));
    }

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

    #[test]
    fn metadata_and_latency_refreshes_survive_a_busy_audio_worker() {
        let processing = &AtomicBool::new(false);
        let mut refresh = PendingRefresh(PARAMS_CHANGED | LATENCY_CHANGED);
        std::thread::scope(|scope| {
            let (entered, wait_for_worker) = std::sync::mpsc::sync_channel(0);
            let (release, wait_for_host) = std::sync::mpsc::sync_channel(0);
            scope.spawn(move || {
                let _guard = ProcessingGuard::try_enter(processing).unwrap();
                entered.send(()).unwrap();
                let _ = wait_for_host.recv();
            });
            wait_for_worker.recv().unwrap();
            assert_eq!(
                refresh.try_read(processing, METADATA_REFRESH, |bits| bits),
                None
            );
            assert_eq!(
                refresh.try_read(processing, LATENCY_CHANGED, |bits| bits),
                None
            );
            refresh.0 |= NOTE_NAMES_CHANGED;
            release.send(()).unwrap();
        });

        assert_eq!(
            refresh.try_read(processing, METADATA_REFRESH, |bits| bits),
            Some(PARAMS_CHANGED | NOTE_NAMES_CHANGED),
        );
        assert_eq!(
            refresh.try_read(processing, LATENCY_CHANGED, |bits| bits),
            Some(LATENCY_CHANGED)
        );
        assert_eq!(
            refresh.try_read(processing, CONTROL_REFRESH, |bits| bits),
            None
        );

        refresh.0 = NOTE_NAMES_CHANGED;
        assert_eq!(
            refresh.try_read(processing, METADATA_REFRESH, |bits| bits),
            Some(NOTE_NAMES_CHANGED)
        );
    }
}
