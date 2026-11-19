//! The audio-thread half of a VST3 instance ([`HostedProcessor`]) around `IAudioProcessor`.
//!
//! komponist hands over stereo in and stereo out; the format layer asks for a stereo main bus in
//! each direction (`setBusArrangements`) and adapts what the plugin settles on, like the CLAP layer:
//!
//! - **Input:** the main input bus gets the stereo input (a mono bus `(L + R) / 2`, more than two
//!   channels L and R and silence). The sidechain bus (the first `kAux` input bus, #6) gets the
//!   block's sidechain the same way, or silence without one. Every other input bus gets silence.
//!   An instrument without audio inputs gets `numInputs = 0`.
//! - **Output:** the main output bus is read back (mono to both channels, more channels: the first
//!   two; channels flagged silent are zeroed). Other output buses are rendered into scratch
//!   buffers and discarded. Without output buses the input passes through.
//!
//! Every bus gets buffers, active or not (VST3 FAQ: inactive buses receive silence / "nirvana"
//! buffers). Notes go to event bus 0. Parameter values from the app, GUI edits from the main
//! thread (a ring, see [`super::host::Edits`]) and MIDI controllers mapped through `IMidiMapping`
//! go in as `inputParameterChanges`. GUI edits and their gestures (`beginEdit` / `performEdit` /
//! `endEdit`) are also echoed as output events of the block that applies them, so the engine sees
//! them with the audio as it does CLAP's. `outputParameterChanges` come back as `ParamValue`
//! events. The bridge reconciles the last DSP output per parameter with mirrored block input
//! before updating the controller. Standalone hosting sends output to the controller through
//! a ring and `MainThreadCallback`.
//!
//! Everything is allocated in [`Vst3Processor::new`] (activation, main thread); `process` only
//! copies and converts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use komponist_ipc::shm::{
    INPUT_EVENT_CAPACITY, TRANSPORT_LOOPING, TRANSPORT_PLAYING, TRANSPORT_RECORDING,
};
use komponist_ipc::{WireEvent, WireEventKind, WireTransport};
use vst3::ComPtr;
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::{
    kBarPositionValid, kContTimeValid, kCycleActive, kCycleValid, kPlaying, kProjectTimeMusicValid,
    kRecording, kTempoValid, kTimeSigValid,
};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, IAudioProcessor, IAudioProcessorTrait, IEventList,
    IParameterChanges, ProcessContext, ProcessData,
};
use vst3::Steinberg::kResultOk;

use super::events::{
    CTRL_AFTERTOUCH, CTRL_PITCH_BEND, EventList, MidiMap, ParameterChanges, RtCom, note_off,
    note_on, poly_pressure,
};
use super::host::{Edit, Edits, ParamChange, QueuedEdit};
use super::module::Module;
use crate::host::{HostNotification, HostNotify};
use crate::plugin::{HostedProcessor, HostingError, ProcessBlock, ProcessStatus};

/// "No note id" in [`WireEvent::note_id`].
const NO_NOTE_ID: u32 = u32::MAX;
/// Room in the input event list for an all-notes-off expanded to one note-off per held note.
const ALL_NOTES_OFF_ROOM: usize = 16 * 128;
/// Distinct parameters per block and points per parameter, input side.
const PARAMS_IN: (usize, usize) = (512, 32);
/// The same for the plugin's output.
const PARAMS_OUT: (usize, usize) = (256, 16);
/// Events the plugin may emit per block (ignored, but the list must accept them).
const EVENTS_OUT: usize = 512;
/// Output parameter changes waiting for the controller.
pub(crate) const TO_CONTROLLER_RING: usize = 1024;
/// CC 123, all notes off.
const CC_ALL_NOTES_OFF: u8 = 123;

/// Channel counts of one direction's audio buses and which one is the main bus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BusLayout {
    pub(crate) channels: Vec<u32>,
    pub(crate) main: Option<usize>,
    /// Inputs only: the sidechain bus (#6).
    pub(crate) sidechain: Option<usize>,
}

impl BusLayout {
    fn total_channels(&self) -> usize {
        self.channels.iter().map(|&c| c as usize).sum()
    }
}

/// Scratch audio and `AudioBusBuffers` for all buses of one direction.
struct BusBuffers {
    channels: Vec<u32>,
    main: Option<usize>,
    sidechain: Option<usize>,
    /// Per bus: the index of its first channel in `data`.
    firsts: Vec<usize>,
    stride: usize,
    data: Vec<f32>,
    /// One pointer per channel into `data`, grouped by bus; `buses` point into it.
    _pointers: Vec<*mut f32>,
    buses: Vec<AudioBusBuffers>,
}

impl BusBuffers {
    fn new(layout: &BusLayout, max_frames: usize) -> BusBuffers {
        let total = layout.total_channels();
        let mut data = vec![0.0f32; total * max_frames];
        let base = data.as_mut_ptr();
        // SAFETY: every offset is within `data` (channel index < total).
        let pointers: Vec<*mut f32> = (0..total)
            .map(|ch| unsafe { base.add(ch * max_frames) })
            .collect();
        let mut buses = Vec::with_capacity(layout.channels.len());
        let mut firsts = Vec::with_capacity(layout.channels.len());
        let mut first = 0usize;
        for &count in &layout.channels {
            firsts.push(first);
            let channel_buffers = if count == 0 {
                std::ptr::null_mut()
            } else {
                pointers[first..].as_ptr().cast_mut()
            };
            buses.push(AudioBusBuffers {
                numChannels: count as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: channel_buffers,
                },
            });
            first += count as usize;
        }
        BusBuffers {
            channels: layout.channels.clone(),
            main: layout.main,
            sidechain: layout.sidechain,
            firsts,
            stride: max_frames,
            data,
            _pointers: pointers,
            buses,
        }
    }

    fn main_channels(&self) -> usize {
        self.main.map_or(0, |m| self.channels[m] as usize)
    }

    /// Channel `ch` of bus `bus`, `frames` long.
    fn bus_channel(&mut self, bus: usize, ch: usize, frames: usize) -> &mut [f32] {
        let start = (self.firsts[bus] + ch) * self.stride;
        &mut self.data[start..start + frames]
    }

    fn main_channel(&mut self, ch: usize, frames: usize) -> &mut [f32] {
        let main = self.main.expect("a main bus");
        self.bus_channel(main, ch, frames)
    }

    /// Writes the stereo `src` into `bus`, adapted to its channel count: a mono bus gets the mid
    /// signal, a bus with more channels L and R and (already) silence in the rest.
    fn write_stereo(&mut self, bus: usize, src: [&[f32]; 2], frames: usize) {
        match self.channels[bus] {
            0 => {}
            1 => {
                let mono = self.bus_channel(bus, 0, frames);
                for ((m, l), r) in mono.iter_mut().zip(src[0]).zip(src[1]) {
                    *m = 0.5 * (l + r);
                }
            }
            _ => {
                for (ch, samples) in src.iter().enumerate() {
                    self.bus_channel(bus, ch, frames)
                        .copy_from_slice(&samples[..frames]);
                }
            }
        }
    }

    /// Zeroes the first `frames` of every channel and clears the silence flags.
    fn clear(&mut self, frames: usize) {
        for channel in self.data.chunks_exact_mut(self.stride.max(1)) {
            channel[..frames].fill(0.0);
        }
        for bus in &mut self.buses {
            bus.silenceFlags = 0;
        }
    }

    /// Input side: silences every bus, then writes the stereo `input` into the main bus and the
    /// `sidechain` (if any) into the sidechain bus.
    fn fill_from_stereo(
        &mut self,
        input: [&[f32]; 2],
        sidechain: Option<[&[f32]; 2]>,
        frames: usize,
    ) {
        self.clear(frames);
        if let Some(main) = self.main {
            self.write_stereo(main, input, frames);
        }
        if let (Some(bus), Some(sidechain)) = (self.sidechain, sidechain) {
            self.write_stereo(bus, sidechain, frames);
        }
    }

    /// Output side: the main bus into the stereo `output`; channels the plugin flagged silent are
    /// zeroed. Without a main bus the `input` passes through.
    fn read_to_stereo(&mut self, input: [&[f32]; 2], output: &mut [&mut [f32]; 2], frames: usize) {
        let silence = self.main.map_or(0, |m| self.buses[m].silenceFlags);
        let copy = |this: &mut BusBuffers, ch: usize, dst: &mut [f32]| {
            if silence & (1 << ch.min(63)) != 0 {
                dst[..frames].fill(0.0);
            } else {
                dst[..frames].copy_from_slice(this.main_channel(ch, frames));
            }
        };
        match self.main_channels() {
            0 => {
                for (dst, src) in output.iter_mut().zip(input) {
                    dst[..frames].copy_from_slice(&src[..frames]);
                }
            }
            1 => {
                let [left, right] = output;
                copy(self, 0, left);
                right[..frames].copy_from_slice(&left[..frames]);
            }
            _ => {
                for (ch, dst) in output.iter_mut().enumerate() {
                    copy(self, ch, dst);
                }
            }
        }
    }
}

/// The `ProcessContext` for `t` at the start of a block.
pub(crate) fn process_context(t: &WireTransport, sample_rate: f64, steady: u64) -> ProcessContext {
    // SAFETY: a plain C struct; zero is a valid value for every field.
    let mut ctx: ProcessContext = unsafe { std::mem::zeroed() };
    ctx.sampleRate = sample_rate;
    ctx.continousTimeSamples = steady as i64;
    ctx.tempo = 120.0;
    ctx.timeSigNumerator = 4;
    ctx.timeSigDenominator = 4;
    let mut state = kContTimeValid;
    if t.tempo_bpm > 0.0 {
        state |=
            kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid | kCycleValid;
        ctx.tempo = t.tempo_bpm;
        ctx.timeSigNumerator = i32::from(t.meter_num.max(1));
        ctx.timeSigDenominator = i32::from(t.meter_den.max(1));
        ctx.projectTimeMusic = t.position_beats;
        ctx.barPositionMusic = t.bar_start_beats;
        ctx.cycleStartMusic = t.loop_start_beats;
        ctx.cycleEndMusic = t.loop_end_beats;
        ctx.projectTimeSamples =
            (t.position_beats * 60.0 / t.tempo_bpm * sample_rate).round() as i64;
        if t.flags & TRANSPORT_PLAYING != 0 {
            state |= kPlaying;
        }
        if t.flags & TRANSPORT_RECORDING != 0 {
            state |= kRecording;
        }
        if t.flags & TRANSPORT_LOOPING != 0 {
            state |= kCycleActive;
        }
    }
    ctx.state = super::enum_u32(state);
    ctx
}

/// What the processor needs from the plugin at activation.
pub(crate) struct ProcessorSetup {
    pub(crate) audio: ComPtr<IAudioProcessor>,
    pub(crate) module: Arc<Module>,
    pub(crate) inputs: BusLayout,
    pub(crate) outputs: BusLayout,
    /// The plugin has an event input bus.
    pub(crate) notes: bool,
    pub(crate) process_mode: i32,
    pub(crate) sample_rate: f64,
    pub(crate) max_frames: u32,
    pub(crate) notify: Arc<dyn HostNotify>,
    pub(crate) midi_map: Arc<MidiMap>,
    pub(crate) edits: Arc<Edits>,
    pub(crate) from_main: rtrb::Consumer<QueuedEdit>,
    /// Changes made while inactive, applied at offset 0 of the first block.
    pub(crate) initial: Vec<ParamChange>,
    pub(crate) initial_generation: u64,
    pub(crate) to_controller: rtrb::Producer<ParamChange>,
    /// Cleared when the processor is dropped (the plugin may then call `setActive(false)`).
    pub(crate) alive: Arc<AtomicBool>,
}

/// The audio-thread half of an active VST3 instance.
pub(crate) struct Vst3Processor {
    // COM pointers into the plugin library first: they drop before `_module` (field order).
    audio: ComPtr<IAudioProcessor>,
    params_in: RtCom<ParameterChanges>,
    params_out: RtCom<ParameterChanges>,
    events_in: RtCom<EventList>,
    events_out: RtCom<EventList>,
    inputs: BusBuffers,
    outputs: BusBuffers,
    context: ProcessContext,
    notes: bool,
    process_mode: i32,
    sample_rate: f64,
    max_frames: u32,
    notify: Arc<dyn HostNotify>,
    midi_map: Arc<MidiMap>,
    edits: Arc<Edits>,
    from_main: rtrb::Consumer<QueuedEdit>,
    initial: Vec<ParamChange>,
    initial_generation: u64,
    to_controller: rtrb::Producer<ParamChange>,
    host_controller_updates: Option<Vec<ParamChange>>,
    /// Held notes: bit `c` of `held[key]` = channel `c`.
    held: [u16; 128],
    started: bool,
    release_held: bool,
    alive: Arc<AtomicBool>,
    _module: Arc<Module>,
}

// SAFETY: the raw channel pointers in the bus buffers point into this processor's own `data`
// vectors (moved along with it; heap buffers do not move); the COM objects are `RtCom`s, used by
// the thread that owns the processor; the plugin's IAudioProcessor may be called from the thread
// that processes (VST3 threading model).
unsafe impl Send for Vst3Processor {}

impl Vst3Processor {
    /// Main thread, at activation: allocates everything `process` needs.
    pub(crate) fn new(setup: ProcessorSetup) -> Vst3Processor {
        let max = setup.max_frames as usize;
        Vst3Processor {
            audio: setup.audio,
            params_in: RtCom::new(ParameterChanges::new(PARAMS_IN.0, PARAMS_IN.1)),
            params_out: RtCom::new(ParameterChanges::new(PARAMS_OUT.0, PARAMS_OUT.1)),
            events_in: RtCom::new(EventList::with_capacity(
                INPUT_EVENT_CAPACITY + ALL_NOTES_OFF_ROOM,
            )),
            events_out: RtCom::new(EventList::with_capacity(EVENTS_OUT)),
            inputs: BusBuffers::new(&setup.inputs, max),
            outputs: BusBuffers::new(&setup.outputs, max),
            context: process_context(&WireTransport::default(), setup.sample_rate, 0),
            notes: setup.notes,
            process_mode: setup.process_mode,
            sample_rate: setup.sample_rate,
            max_frames: setup.max_frames,
            notify: setup.notify,
            midi_map: setup.midi_map,
            edits: setup.edits,
            from_main: setup.from_main,
            initial: setup.initial,
            initial_generation: setup.initial_generation,
            to_controller: setup.to_controller,
            host_controller_updates: None,
            held: [0; 128],
            started: false,
            release_held: false,
            alive: setup.alive,
            _module: setup.module,
        }
    }

    /// A note on (`velocity > 0`) or off; out-of-range channels and keys are dropped.
    fn note(&mut self, time: u32, channel: u8, key: u8, velocity: u8, note_id: u32) {
        if channel >= 16 || key >= 128 {
            return;
        }
        let bit = 1u16 << channel;
        let id = if note_id == NO_NOTE_ID {
            -1
        } else {
            (note_id & i32::MAX as u32) as i32
        };
        if velocity > 0 {
            self.held[key as usize] |= bit;
            if self.notes {
                self.events_in
                    .push(note_on(time, channel, key, velocity, id));
            }
        } else {
            self.held[key as usize] &= !bit;
            if self.notes {
                self.events_in.push(note_off(time, channel, key, 0, id));
            }
        }
    }

    /// Note-offs for every held note.
    fn release_all(&mut self, time: u32) {
        for key in 0..128u8 {
            let mut channels = self.held[key as usize];
            while channels != 0 {
                let channel = channels.trailing_zeros() as u8;
                channels &= channels - 1;
                self.note(time, channel, key, 0, NO_NOTE_ID);
            }
        }
    }

    /// A MIDI 1.0 channel message: notes and poly pressure as events, controllers, channel
    /// pressure and pitch bend through the plugin's MIDI mapping.
    fn midi(&mut self, time: u32, [status, d1, d2]: [u8; 3]) {
        let channel = status & 0x0F;
        let (d1, d2) = (d1 & 0x7F, d2 & 0x7F);
        let mapped = |this: &Self, controller: usize, value: f64| {
            if let Some(id) = this.midi_map.get(channel, controller) {
                this.params_in.add(id, time, value);
                true
            } else {
                false
            }
        };
        match status & 0xF0 {
            0x80 => self.note(time, channel, d1, 0, NO_NOTE_ID),
            0x90 => self.note(time, channel, d1, d2, NO_NOTE_ID),
            0xA0 if self.notes => {
                self.events_in.push(poly_pressure(time, channel, d1, d2));
            }
            0xB0 => {
                if !mapped(self, usize::from(d1), f64::from(d2) / 127.0) && d1 == CC_ALL_NOTES_OFF {
                    self.release_all(time);
                }
            }
            0xD0 => {
                mapped(self, CTRL_AFTERTOUCH, f64::from(d1) / 127.0);
            }
            0xE0 => {
                let bend = (u16::from(d2) << 7) | u16::from(d1);
                mapped(self, CTRL_PITCH_BEND, f64::from(bend) / 16383.0);
            }
            _ => {}
        }
    }

    /// Applies the main thread's changes at offset 0; GUI edits and gestures are echoed into
    /// `echo` (the block's output events) so they travel with the audio like CLAP's.
    fn apply_main_thread_edits(&mut self, mut echo: Option<&mut crate::plugin::EventOut<'_>>) {
        let generation = self.edits.generation();
        if self.initial_generation == generation {
            for (id, value) in self.initial.drain(..) {
                self.params_in.add(id, 0, value);
            }
        } else {
            self.initial.clear();
        }
        while let Ok(queued) = self.from_main.pop() {
            if queued.generation != generation {
                continue;
            }
            let event = match queued.edit {
                Edit::Set(id, value) => {
                    self.params_in.add(id, 0, value);
                    None
                }
                Edit::Perform(id, value) => {
                    self.params_in.add(id, 0, value);
                    Some(WireEventKind::ParamValue { id, value })
                }
                Edit::Gesture(id, begin) => Some(WireEventKind::ParamGesture { id, begin }),
            };
            if let (Some(echo), Some(kind)) = (echo.as_deref_mut(), event) {
                echo.push(WireEvent::new(0, kind));
            }
        }
    }

    /// Fills the input lists for `block`.
    fn convert_inputs(&mut self, block: &mut ProcessBlock<'_>) {
        self.params_in.clear();
        self.events_in.clear();
        self.apply_main_thread_edits(Some(&mut block.events_out));
        if std::mem::take(&mut self.release_held) {
            self.release_all(0);
        }
        let last = block.frames.saturating_sub(1);
        for wire in block.events_in {
            let time = wire.offset.min(last);
            match wire.decode() {
                Some(WireEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                    note_id,
                }) => self.note(time, channel, key, velocity.max(1), note_id),
                Some(WireEventKind::NoteOff {
                    channel,
                    key,
                    note_id,
                }) => self.note(time, channel, key, 0, note_id),
                Some(WireEventKind::AllNotesOff) => self.release_all(time),
                Some(WireEventKind::Midi(data)) => self.midi(time, data),
                Some(WireEventKind::ParamValue { id, value }) => {
                    self.params_in.add(id, time, value.clamp(0.0, 1.0));
                }
                // Gestures only travel from the plugin to the host.
                Some(WireEventKind::ParamGesture { .. }) | None => {}
            }
        }
    }

    /// Hands the plugin's output parameter changes to the bridge (events) and the controller.
    fn collect_outputs(&mut self, events_out: &mut crate::plugin::EventOut<'_>) {
        let mut to_controller = false;
        if let Some(updates) = &mut self.host_controller_updates {
            updates.clear();
        }
        for queue in self.params_out.queues() {
            let id = queue.id();
            let points = queue.points();
            for &(offset, value) in points.iter() {
                events_out.push(WireEvent::new(
                    offset.max(0) as u32,
                    WireEventKind::ParamValue { id, value },
                ));
            }
            if let Some(&(_, value)) = points.last() {
                if let Some(updates) = &mut self.host_controller_updates {
                    if value.is_finite() {
                        updates.push((id, value));
                    }
                } else {
                    to_controller |= self.to_controller.push((id, value)).is_ok();
                }
            }
        }
        if let Some(updates) = &mut self.host_controller_updates {
            updates.sort_unstable_by_key(|&(id, _)| id);
        }
        if to_controller {
            self.notify.notify(HostNotification::MainThreadCallback);
        }
        self.params_out.clear();
        self.events_out.clear();
    }

    /// A parameter flush: one `process` call without audio (`numSamples = 0`, no buses) that
    /// hands the pending changes to the plugin (VST3 FAQ, Processing). Main thread, right after
    /// activation; the processor is dropped afterwards.
    pub(crate) fn flush(&mut self) {
        self.set_processing(true);
        self.params_in.clear();
        self.events_in.clear();
        self.apply_main_thread_edits(None);
        let mut data = ProcessData {
            processMode: self.process_mode,
            symbolicSampleSize: super::enum_i32(kSample32),
            numSamples: 0,
            numInputs: 0,
            numOutputs: 0,
            inputs: std::ptr::null_mut(),
            outputs: std::ptr::null_mut(),
            inputParameterChanges: self.params_in.ptr::<IParameterChanges>(),
            outputParameterChanges: self.params_out.ptr::<IParameterChanges>(),
            inputEvents: std::ptr::null_mut(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut self.context,
        };
        // SAFETY: active and processing; the pointers in `data` stay valid during the call.
        unsafe { self.audio.process(&mut data) };
        self.collect_outputs(&mut crate::plugin::EventOut::new(&mut []));
        self.set_processing(false);
    }

    fn set_processing(&mut self, on: bool) {
        // Results are not checked: the SDK's default returns kNotImplemented.
        // SAFETY: the plugin is active; called on the processing thread.
        unsafe { self.audio.setProcessing(u8::from(on)) };
        self.started = on;
    }
}

impl HostedProcessor for Vst3Processor {
    fn use_host_controller_sync(&mut self) {
        self.host_controller_updates = Some(Vec::with_capacity(PARAMS_OUT.0));
    }

    fn controller_updates(&self) -> &[(u32, f64)] {
        self.host_controller_updates.as_deref().unwrap_or_default()
    }

    fn start_processing(&mut self) -> Result<(), HostingError> {
        if !self.started {
            self.set_processing(true);
        }
        Ok(())
    }

    fn stop_processing(&mut self) {
        if self.started {
            self.set_processing(false);
        }
    }

    fn process(&mut self, block: &mut ProcessBlock<'_>) -> ProcessStatus {
        let frames = block.frames as usize;
        if block.frames == 0
            || block.frames > self.max_frames
            || block.input.iter().any(|c| c.len() < frames)
            || block
                .sidechain
                .is_some_and(|sc| sc.iter().any(|c| c.len() < frames))
            || block.output.iter().any(|c| c.len() < frames)
            || !self.started
        {
            return ProcessStatus::Error;
        }
        self.convert_inputs(block);
        self.inputs
            .fill_from_stereo(block.input, block.sidechain, frames);
        self.outputs.clear(frames);
        self.context = process_context(block.transport, self.sample_rate, block.steady_time);

        let mut data = ProcessData {
            processMode: self.process_mode,
            symbolicSampleSize: super::enum_i32(kSample32),
            numSamples: frames as i32,
            numInputs: self.inputs.buses.len() as i32,
            numOutputs: self.outputs.buses.len() as i32,
            inputs: self.inputs.buses.as_mut_ptr(),
            outputs: self.outputs.buses.as_mut_ptr(),
            inputParameterChanges: self.params_in.ptr::<IParameterChanges>(),
            outputParameterChanges: self.params_out.ptr::<IParameterChanges>(),
            inputEvents: if self.notes {
                self.events_in.ptr::<IEventList>()
            } else {
                std::ptr::null_mut()
            },
            outputEvents: self.events_out.ptr::<IEventList>(),
            processContext: &mut self.context,
        };
        if data.numInputs == 0 {
            data.inputs = std::ptr::null_mut();
        }
        if data.numOutputs == 0 {
            data.outputs = std::ptr::null_mut();
        }
        // SAFETY: active and processing; every pointer in `data` stays valid during the call.
        let result = unsafe { self.audio.process(&mut data) };
        self.collect_outputs(&mut block.events_out);
        if result != kResultOk {
            return ProcessStatus::Error;
        }
        self.outputs
            .read_to_stereo(block.input, &mut block.output, frames);
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        // VST3 has no reset; `setProcessing(false)` / `(true)` is where plugins clear their
        // buffers (JUCE, nice-plug). Held notes also get note-offs in the next block.
        if self.started {
            self.set_processing(false);
            self.set_processing(true);
        }
        self.release_held = true;
    }
}

impl Drop for Vst3Processor {
    fn drop(&mut self) {
        // The bridge stops processing on the RT thread before handing the processor back; this
        // only catches a processor dropped while still started.
        if self.started {
            self.set_processing(false);
        }
        // Changes that never reached a block are applied by the next processor.
        let generation = self.edits.generation();
        let mut leftover = std::mem::take(&mut self.initial);
        if self.initial_generation != generation {
            leftover.clear();
        }
        while let Ok(queued) = self.from_main.pop() {
            if queued.generation == generation
                && let Edit::Set(id, value) | Edit::Perform(id, value) = queued.edit
            {
                leftover.push((id, value));
            }
        }
        self.edits.disconnect(leftover);
        self.alive.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(channels: &[u32], main: Option<usize>) -> BusLayout {
        BusLayout {
            channels: channels.to_vec(),
            main,
            sidechain: None,
        }
    }

    #[test]
    fn the_sidechain_goes_into_its_bus_or_silence() {
        let left = [1.0, 2.0, 3.0];
        let right = [3.0, 4.0, 5.0];
        let sc = [[0.5f32; 3], [-0.5f32; 3]];
        let mut buses = BusBuffers::new(
            &BusLayout {
                sidechain: Some(1),
                ..layout(&[2, 2, 1], Some(0))
            },
            4,
        );
        buses.data.fill(9.0);
        buses.fill_from_stereo([&left, &right], Some([&sc[0], &sc[1]]), 3);
        assert_eq!(buses.main_channel(0, 3), &left);
        assert_eq!(buses.bus_channel(1, 0, 3), &sc[0]);
        assert_eq!(buses.bus_channel(1, 1, 3), &sc[1]);
        assert_eq!(buses.bus_channel(2, 0, 3), &[0.0; 3]);
        buses.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(buses.bus_channel(1, 0, 3), &[0.0; 3]);
        // A mono sidechain bus gets the mid signal.
        let mut buses = BusBuffers::new(
            &BusLayout {
                sidechain: Some(1),
                ..layout(&[2, 1], Some(0))
            },
            3,
        );
        buses.fill_from_stereo([&left, &right], Some([&left, &right]), 3);
        assert_eq!(buses.bus_channel(1, 0, 3), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn bus_buffers_point_into_their_data() {
        let buffers = BusBuffers::new(&layout(&[2, 0, 1], Some(0)), 4);
        assert_eq!(buffers.buses.len(), 3);
        assert_eq!(buffers.buses[0].numChannels, 2);
        let channels = unsafe { buffers.buses[0].__field0.channelBuffers32 };
        let second = unsafe { *channels.add(1) };
        assert_eq!(second, unsafe { buffers.data.as_ptr().add(4) }.cast_mut());
        assert!(unsafe { buffers.buses[1].__field0.channelBuffers32 }.is_null());
        let third = unsafe { *buffers.buses[2].__field0.channelBuffers32 };
        assert_eq!(third, unsafe { buffers.data.as_ptr().add(8) }.cast_mut());
    }

    #[test]
    fn stereo_adapts_to_the_main_bus() {
        let left = [1.0, 2.0, 3.0];
        let right = [3.0, 4.0, 5.0];
        // Sidechain (2 ch) first, mono main second.
        let mut buses = BusBuffers::new(&layout(&[2, 1], Some(1)), 4);
        buses.data.fill(9.0);
        buses.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(buses.main_channel(0, 3), &[2.0, 3.0, 4.0]);
        assert_eq!(&buses.data[0..3], &[0.0; 3]);

        // Mono output to both channels; a channel flagged silent is zeroed.
        let input = [[7.0f32; 2], [8.0f32; 2]];
        let input = [&input[0][..], &input[1][..]];
        let (mut l, mut r) = ([0.0f32; 2], [0.0f32; 2]);
        let mut out = BusBuffers::new(&layout(&[1], Some(0)), 2);
        out.main_channel(0, 2).copy_from_slice(&[0.5, -0.5]);
        out.read_to_stereo(input, &mut [&mut l, &mut r], 2);
        assert_eq!((l, r), ([0.5, -0.5], [0.5, -0.5]));
        let mut out = BusBuffers::new(&layout(&[2], Some(0)), 2);
        out.data.fill(0.25);
        out.buses[0].silenceFlags = 0b10;
        out.read_to_stereo(input, &mut [&mut l, &mut r], 2);
        assert_eq!((l, r), ([0.25; 2], [0.0; 2]));
        // No output bus: the input passes through.
        let mut out = BusBuffers::new(&BusLayout::default(), 2);
        out.read_to_stereo(input, &mut [&mut l, &mut r], 2);
        assert_eq!((l, r), ([7.0; 2], [8.0; 2]));
    }

    #[test]
    fn transport_maps_to_the_process_context() {
        let ctx = process_context(&WireTransport::default(), 48_000.0, 512);
        assert_eq!(ctx.state, super::super::enum_u32(kContTimeValid));
        assert_eq!(ctx.continousTimeSamples, 512);
        let t = WireTransport {
            flags: TRANSPORT_PLAYING | TRANSPORT_LOOPING,
            meter_num: 3,
            meter_den: 4,
            tempo_bpm: 120.0,
            position_beats: 6.5,
            bar_start_beats: 6.0,
            bar_number: 2,
            loop_start_beats: 4.0,
            loop_end_beats: 8.0,
            ..WireTransport::default()
        };
        let ctx = process_context(&t, 48_000.0, 0);
        for flag in [kPlaying, kCycleActive, kTempoValid, kProjectTimeMusicValid] {
            assert!(ctx.state & super::super::enum_u32(flag) != 0, "{flag:#x}");
        }
        assert_eq!(ctx.state & super::super::enum_u32(kRecording), 0);
        assert_eq!(ctx.projectTimeSamples, 156_000);
        assert_eq!((ctx.timeSigNumerator, ctx.timeSigDenominator), (3, 4));
        assert_eq!((ctx.cycleStartMusic, ctx.cycleEndMusic), (4.0, 8.0));
    }
}
