//! The audio-thread half of a CLAP instance ([`HostedProcessor`]).
//!
//! komponist always hands over stereo in and stereo out; the plugin's audio ports are adapted by
//! the [`StereoAdapter`] it shares with the VST3 layer (`crate::adapter`). The main port of a
//! direction is the first port flagged main, else port 0; the sidechain port is the first other
//! input port with channels (#6; CLAP has no sidechain flag). A plugin may request the main-input
//! fallback for an unconnected sidechain. Held notes ([`HeldNotes`]) turn an all-notes-off into
//! note-offs; MIDI messages go to the plugin as they are.
//!
//! Buffers, port descriptors and the event list are allocated in [`ClapProcessor::new`]
//! (activation, main thread); [`HostedProcessor::process`] only copies and converts.

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::note_ports::{NoteDialect, NotePortInfoBuffer, PluginNotePorts};
use clack_host::events::EventFlags;
use clack_host::events::event_types::{
    MidiEvent, NoteOffEvent, NoteOnEvent, TransportEvent, TransportFlags,
};
use clack_host::prelude::*;
use clack_host::utils::BeatTime;
use komponist_ipc::shm::{
    INPUT_EVENT_CAPACITY, TRANSPORT_LOOPING, TRANSPORT_PLAYING, TRANSPORT_RECORDING,
};
use komponist_ipc::{WireEventKind, WireTransport};

use super::events::{self, InEvent, InputEventList, WireSink};
use super::host::{AudioThreadScope, KpHost};
use super::signal_taps::SignalTaps;
use crate::adapter::{ALL_NOTES_OFF_ROOM, AudioLayout, HeldNotes, Note, StereoAdapter};
use crate::plugin::{HostedProcessor, HostingError, ProcessBlock, ProcessStatus};

/// Reads the audio ports of one direction of `plugin` (main thread, inactive): channel counts, the
/// main port and, for inputs, the sidechain port. `sidechain_from_main` is the caller's.
pub(crate) fn port_layout(
    ext: Option<PluginAudioPorts>,
    plugin: &PluginMainThreadHandle,
    is_input: bool,
) -> AudioLayout {
    let Some(ext) = ext else {
        return AudioLayout::default();
    };
    let mut layout = AudioLayout::default();
    let mut buffer = AudioPortInfoBuffer::new();
    for index in 0..ext.count(plugin, is_input) {
        let Some(info) = ext.get(plugin, index, is_input, &mut buffer) else {
            continue;
        };
        if layout.main.is_none() && info.flags.contains(AudioPortFlags::IS_MAIN) {
            layout.main = Some(layout.channels.len());
        }
        layout.channels.push(info.channel_count);
    }
    if layout.main.is_none() && !layout.channels.is_empty() {
        layout.main = Some(0);
    }
    if is_input {
        layout.sidechain = sidechain_port(&layout);
    }
    layout
}

/// The sidechain port: the first port besides the main one with at least one channel (#6). CLAP
/// has no sidechain flag, so the non-main input is taken.
fn sidechain_port(layout: &AudioLayout) -> Option<usize> {
    (0..layout.channels.len()).find(|&p| Some(p) != layout.main && layout.channels[p] > 0)
}

/// How notes reach the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteTarget {
    /// No note input port: note events are dropped.
    None,
    /// CLAP note events on this port.
    Clap(u16),
    /// MIDI 1.0 messages on this port.
    Midi(u16),
}

impl NoteTarget {
    /// Picks the first note input port and its dialect (CLAP preferred over MIDI).
    pub(crate) fn query(
        ext: Option<PluginNotePorts>,
        plugin: &PluginMainThreadHandle,
    ) -> NoteTarget {
        let Some(ext) = ext else {
            return NoteTarget::None;
        };
        if ext.count(plugin, true) == 0 {
            return NoteTarget::None;
        }
        let mut buffer = NotePortInfoBuffer::new();
        let Some(info) = ext.get(plugin, 0, true, &mut buffer) else {
            return NoteTarget::None;
        };
        if info.supported_dialects.supports(NoteDialect::Clap) {
            NoteTarget::Clap(0)
        } else if info.supported_dialects.supports(NoteDialect::Midi) {
            NoteTarget::Midi(0)
        } else {
            NoteTarget::None
        }
    }
}

/// The clack port descriptors and the stereo-adapted scratch audio of one direction.
struct PortBuffers {
    ports: AudioPorts,
    audio: StereoAdapter,
}

impl PortBuffers {
    fn new(layout: &AudioLayout, max_frames: usize) -> PortBuffers {
        let total = layout.channels.iter().map(|&c| c as usize).sum();
        PortBuffers {
            ports: AudioPorts::with_capacity(total, layout.channels.len()),
            audio: StereoAdapter::new(layout, max_frames),
        }
    }
}

/// The audio-thread half of an active CLAP instance.
pub(crate) struct ClapProcessor {
    audio: PluginAudioProcessor<KpHost>,
    inputs: PortBuffers,
    outputs: PortBuffers,
    notes: NoteTarget,
    note_output: bool,
    events: InputEventList,
    held: HeldNotes,
    max_frames: u32,
    signal_taps: Option<SignalTaps>,
}

impl ClapProcessor {
    /// Main thread, at activation: allocates everything `process` needs.
    pub(crate) fn new(
        audio: StoppedPluginAudioProcessor<KpHost>,
        inputs: &AudioLayout,
        outputs: &AudioLayout,
        notes: NoteTarget,
        note_output: bool,
        max_frames: u32,
    ) -> ClapProcessor {
        ClapProcessor {
            audio: audio.into(),
            inputs: PortBuffers::new(inputs, max_frames as usize),
            outputs: PortBuffers::new(outputs, max_frames as usize),
            notes,
            note_output,
            events: InputEventList::with_capacity(INPUT_EVENT_CAPACITY + ALL_NOTES_OFF_ROOM),
            held: HeldNotes::new(),
            max_frames,
            signal_taps: None,
        }
    }

    pub(super) fn set_signal_taps(&mut self, taps: Option<SignalTaps>) {
        self.signal_taps = taps;
    }

    /// A note on or off that [`HeldNotes`] recorded, as an event on the note port.
    fn push_note(&mut self, time: u32, note: Note) {
        let Note {
            channel,
            key,
            velocity,
            note_id,
        } = note;
        let event = match self.notes {
            NoteTarget::None => return,
            NoteTarget::Clap(port) => {
                let pckn = Pckn::from_raw(port as i16, i16::from(channel), i16::from(key), note_id);
                if note.is_on() {
                    InEvent::NoteOn(NoteOnEvent::new(time, pckn, f64::from(velocity) / 127.0))
                } else {
                    InEvent::NoteOff(NoteOffEvent::new(time, pckn, 0.0))
                }
            }
            NoteTarget::Midi(port) => {
                let status = if note.is_on() { 0x90 } else { 0x80 } | channel;
                InEvent::Midi(MidiEvent::new(time, port, [status, key, velocity]))
            }
        };
        self.events.push(event);
    }

    /// Converts the block's wire events into `self.events`.
    fn convert_events(&mut self, block: &ProcessBlock<'_>) {
        self.events.clear();
        let last = block.frames.saturating_sub(1);
        for wire in block.events_in {
            let time = wire.offset.min(last);
            match wire.decode() {
                Some(WireEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                    note_id,
                }) => {
                    if let Some(note) = self.held.on(channel, key, velocity, note_id) {
                        self.push_note(time, note);
                    }
                }
                Some(WireEventKind::NoteOff {
                    channel,
                    key,
                    note_id,
                }) => {
                    if let Some(note) = self.held.off(channel, key, note_id) {
                        self.push_note(time, note);
                    }
                }
                Some(WireEventKind::AllNotesOff) => {
                    for note in self.held.release_all() {
                        self.push_note(time, note);
                    }
                }
                Some(WireEventKind::Midi(data)) => {
                    if let NoteTarget::Clap(port) | NoteTarget::Midi(port) = self.notes {
                        self.events
                            .push(InEvent::Midi(MidiEvent::new(time, port, data)));
                    }
                }
                Some(WireEventKind::ParamValue { id, value }) => {
                    if let Some(event) = events::param_value_event(time, id, value) {
                        self.events.push(event);
                    }
                }
                // Gestures only travel from the plugin to the host.
                Some(WireEventKind::ParamGesture { .. }) | None => {}
            }
        }
    }
}

/// The CLAP transport for `t`; `None` without a tempo (no transport known).
fn transport_event(t: &WireTransport) -> Option<TransportEvent> {
    if t.tempo_bpm <= 0.0 {
        return None;
    }
    let mut flags = TransportFlags::HAS_TEMPO
        | TransportFlags::HAS_BEATS_TIMELINE
        | TransportFlags::HAS_TIME_SIGNATURE;
    if t.flags & TRANSPORT_PLAYING != 0 {
        flags |= TransportFlags::IS_PLAYING;
    }
    if t.flags & TRANSPORT_RECORDING != 0 {
        flags |= TransportFlags::IS_RECORDING;
    }
    if t.flags & TRANSPORT_LOOPING != 0 {
        flags |= TransportFlags::IS_LOOP_ACTIVE;
    }
    Some(TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(t.position_beats),
        song_pos_seconds: BeatTime::from_int(0),
        tempo: t.tempo_bpm,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_float(t.loop_start_beats),
        loop_end_beats: BeatTime::from_float(t.loop_end_beats),
        loop_start_seconds: BeatTime::from_int(0),
        loop_end_seconds: BeatTime::from_int(0),
        bar_start: BeatTime::from_float(t.bar_start_beats),
        bar_number: t.bar_number,
        time_signature_numerator: t.meter_num.max(1),
        time_signature_denominator: t.meter_den.max(1),
    })
}

impl HostedProcessor for ClapProcessor {
    fn start_processing(&mut self) -> Result<(), HostingError> {
        let _audio = AudioThreadScope::enter();
        if self.audio.is_started() {
            return Ok(());
        }
        self.audio
            .start_processing()
            .map(|_| ())
            .map_err(|_| HostingError::Activate("the plugin refused to start processing".into()))
    }

    fn stop_processing(&mut self) {
        let _audio = AudioThreadScope::enter();
        if self.audio.is_started() {
            let _ = self.audio.stop_processing();
        }
    }

    fn process(&mut self, block: &mut ProcessBlock<'_>) -> ProcessStatus {
        let _audio = AudioThreadScope::enter();
        let frames = block.frames as usize;
        if block.frames == 0
            || block.frames > self.max_frames
            || block.input.iter().any(|c| c.len() < frames)
            || block
                .sidechain
                .is_some_and(|sc| sc.iter().any(|c| c.len() < frames))
            || block.output.iter().any(|c| c.len() < frames)
        {
            return ProcessStatus::Error;
        }
        if !self.audio.is_started() {
            return ProcessStatus::Error;
        }

        self.convert_events(block);
        self.inputs
            .audio
            .fill_from_stereo(block.input, block.sidechain, frames);
        self.outputs.audio.clear(frames);
        let transport = transport_event(block.transport);

        let result = {
            let inputs = &mut self.inputs;
            let outputs = &mut self.outputs;
            let in_buffers = inputs
                .ports
                .with_input_buffers(inputs.audio.ports_mut(frames).map(|channels| {
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(
                            channels.map(InputChannel::variable),
                        ),
                    }
                }));
            let mut out_buffers =
                outputs
                    .ports
                    .with_output_buffers(outputs.audio.ports_mut(frames).map(|channels| {
                        AudioPortBuffer {
                            latency: 0,
                            channels: AudioPortBufferType::f32_output_only(channels),
                        }
                    }));
            let in_buffers = if in_buffers.frames_count().is_none()
                && out_buffers.frames_count().is_none()
            {
                // Without audio channels Clack has no channel length to infer the process
                // frame count from. Event-only CLAPs still need the engine's block length.
                // SAFETY: The already validated port descriptors remain live, and no channel
                // buffer in either direction has a frame count to bound.
                unsafe {
                    InputAudioBuffers::from_raw_buffers(in_buffers.as_raw_buffers(), block.frames)
                }
            } else {
                in_buffers
            };
            let events_out = &mut block.events_out;
            let mut sink = WireSink::process(block.frames, self.note_output, |event| {
                events_out.push(event)
            });
            let Ok(started) = self.audio.as_started_mut() else {
                return ProcessStatus::Error;
            };
            started.process(
                &in_buffers,
                &mut out_buffers,
                &InputEvents::from_buffer(&self.events),
                &mut OutputEvents::from_buffer(&mut sink),
                Some(block.steady_time),
                transport.as_ref(),
            )
        };

        match result {
            Ok(status) => {
                // CLAP has no per-channel silence flags to honour.
                self.outputs
                    .audio
                    .read_to_stereo(block.input, &mut block.output, frames, 0);
                match status {
                    clack_host::process::ProcessStatus::Sleep => ProcessStatus::Sleep,
                    _ => ProcessStatus::Continue,
                }
            }
            Err(_) => ProcessStatus::Error,
        }
    }

    fn begin_signal_taps(&mut self, mask: u32) {
        if let Some(taps) = &self.signal_taps {
            let _audio = AudioThreadScope::enter();
            taps.begin(self.audio.plugin_handle().as_raw(), mask);
        }
    }

    fn read_signal_taps(
        &mut self,
        mask: u32,
        out: &mut komponist_plugin_api::signal_taps::BlockTaps,
    ) {
        if let Some(taps) = &self.signal_taps {
            let _audio = AudioThreadScope::enter();
            taps.read(self.audio.plugin_handle().as_raw(), mask, out);
        }
    }

    fn reset(&mut self) {
        let _audio = AudioThreadScope::enter();
        self.audio.reset();
        // CLAP's reset clears the plugin's voices itself: no note-offs.
        self.held.clear();
    }
}

impl Drop for ClapProcessor {
    fn drop(&mut self) {
        // The bridge stops processing on the RT thread before handing the processor back; this
        // only catches a processor dropped while still started.
        if self.audio.is_started() {
            let _audio = AudioThreadScope::enter();
            let _ = self.audio.stop_processing();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(channels: &[u32], main: Option<usize>) -> AudioLayout {
        let mut layout = AudioLayout {
            channels: channels.to_vec(),
            main,
            ..AudioLayout::default()
        };
        layout.sidechain = sidechain_port(&layout);
        layout
    }

    #[test]
    fn the_sidechain_port_is_the_first_other_input_with_channels() {
        assert_eq!(layout(&[2], Some(0)).sidechain, None);
        assert_eq!(layout(&[2, 2], Some(0)).sidechain, Some(1));
        assert_eq!(layout(&[2, 1], Some(1)).sidechain, Some(0));
        assert_eq!(layout(&[2, 0, 1], Some(0)).sidechain, Some(2));
        assert_eq!(layout(&[], None).sidechain, None);
    }

    #[test]
    fn transport_needs_a_tempo() {
        assert!(transport_event(&WireTransport::default()).is_none());
        let t = WireTransport {
            flags: TRANSPORT_PLAYING | TRANSPORT_LOOPING,
            meter_num: 3,
            meter_den: 4,
            tempo_bpm: 120.0,
            position_beats: 6.5,
            bar_start_beats: 6.0,
            bar_number: 2,
            ..WireTransport::default()
        };
        let e = transport_event(&t).unwrap();
        assert!(
            e.flags
                .contains(TransportFlags::IS_PLAYING | TransportFlags::IS_LOOP_ACTIVE)
        );
        assert!(!e.flags.contains(TransportFlags::IS_RECORDING));
        assert_eq!(e.song_pos_beats.to_float(), 6.5);
        assert_eq!(e.bar_number, 2);
        assert_eq!(
            (e.time_signature_numerator, e.time_signature_denominator),
            (3, 4)
        );
    }
}
