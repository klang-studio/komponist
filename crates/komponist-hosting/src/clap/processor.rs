//! The audio-thread half of a CLAP instance ([`HostedProcessor`]).
//!
//! komponist always hands over stereo in and stereo out. The plugin's audio ports are adapted:
//!
//! - **Input:** the main input port (the first port flagged main, else port 0) gets the stereo
//!   input; a mono port gets `(L + R) / 2`, a port with more than two channels gets L and R in its
//!   first two channels and silence in the rest. The sidechain port (the first other input port
//!   with channels, #6) gets the block's sidechain the same way, or silence without one. A plugin
//!   may request the main-input fallback for an unconnected sidechain. Every further input port
//!   is silent.
//! - **Output:** the main output port is read back; a mono port goes to both channels, a port with
//!   more channels contributes its first two. Other output ports are rendered and discarded. A
//!   plugin without output ports passes the input through.
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

use super::events::{self, InEvent, InputEventList, NO_NOTE_ID, WireSink};
use super::host::{AudioThreadScope, KpHost};
use super::signal_taps::SignalTaps;
use crate::plugin::{HostedProcessor, HostingError, ProcessBlock, ProcessStatus};

/// Room in the event list for an all-notes-off expanded to one note-off per held note.
const ALL_NOTES_OFF_ROOM: usize = 16 * 128;

/// Channel counts of one direction's audio ports and which one is the main port.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PortLayout {
    pub(crate) channels: Vec<u32>,
    pub(crate) main: Option<usize>,
    /// Inputs only: the sidechain port, the first port besides the main one with at least one
    /// channel (#6). CLAP has no sidechain flag, so the non-main input is taken.
    pub(crate) sidechain: Option<usize>,
    pub(crate) sidechain_from_main: bool,
}

impl PortLayout {
    /// Reads the audio ports of `plugin` (main thread, inactive).
    pub(crate) fn query(
        ext: Option<PluginAudioPorts>,
        plugin: &PluginMainThreadHandle,
        is_input: bool,
    ) -> PortLayout {
        let Some(ext) = ext else {
            return PortLayout::default();
        };
        let mut layout = PortLayout::default();
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
            layout.sidechain = layout.sidechain_port();
        }
        layout
    }

    /// The first port besides the main one with at least one channel.
    fn sidechain_port(&self) -> Option<usize> {
        (0..self.channels.len()).find(|&p| Some(p) != self.main && self.channels[p] > 0)
    }

    fn total_channels(&self) -> usize {
        self.channels.iter().map(|&c| c as usize).sum()
    }
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

/// Scratch audio for all ports of one direction, `stride` = max frames per channel.
struct PortBuffers {
    ports: AudioPorts,
    channels: Vec<u32>,
    main: Option<usize>,
    sidechain: Option<usize>,
    sidechain_from_main: bool,
    /// Per port: the index of its first channel in `data` (in channels).
    firsts: Vec<usize>,
    stride: usize,
    data: Vec<f32>,
}

impl PortBuffers {
    fn new(layout: &PortLayout, max_frames: usize) -> PortBuffers {
        let total = layout.total_channels();
        let firsts = layout
            .channels
            .iter()
            .scan(0usize, |first, &count| {
                let this = *first;
                *first += count as usize;
                Some(this)
            })
            .collect();
        PortBuffers {
            ports: AudioPorts::with_capacity(total, layout.channels.len()),
            channels: layout.channels.clone(),
            main: layout.main,
            sidechain: layout.sidechain,
            sidechain_from_main: layout.sidechain_from_main
                && layout.main.is_some_and(|main| layout.channels[main] > 0),
            firsts,
            stride: max_frames,
            data: vec![0.0; total * max_frames],
        }
    }

    fn main_channels(&self) -> usize {
        self.main.map_or(0, |m| self.channels[m] as usize)
    }

    /// Channel `ch` of port `port`, `frames` long.
    fn port_channel(&mut self, port: usize, ch: usize, frames: usize) -> &mut [f32] {
        let start = (self.firsts[port] + ch) * self.stride;
        &mut self.data[start..start + frames]
    }

    /// Channel `ch` of the main port, `frames` long.
    fn main_channel(&mut self, ch: usize, frames: usize) -> &mut [f32] {
        let main = self.main.expect("a main port");
        self.port_channel(main, ch, frames)
    }

    /// Writes the stereo `src` into `port`, adapted to its channel count: a mono port gets the
    /// mid signal, a port with more channels L and R and (already) silence in the rest.
    fn write_stereo(&mut self, port: usize, src: [&[f32]; 2], frames: usize) {
        match self.channels[port] {
            0 => {}
            1 => {
                let mono = self.port_channel(port, 0, frames);
                for ((m, l), r) in mono.iter_mut().zip(src[0]).zip(src[1]) {
                    *m = 0.5 * (l + r);
                }
            }
            _ => {
                for (ch, samples) in src.iter().enumerate() {
                    self.port_channel(port, ch, frames)
                        .copy_from_slice(&samples[..frames]);
                }
            }
        }
    }

    /// Zeroes the first `frames` of every channel.
    fn clear(&mut self, frames: usize) {
        for channel in self.data.chunks_exact_mut(self.stride) {
            channel[..frames].fill(0.0);
        }
    }

    /// Input side: silences every port, then writes the stereo `input` into the main port and the
    /// sidechain into its port, adapted to their channel counts. If disconnected, only a plugin
    /// that opts into the main-input fallback receives the main signal on that port.
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
        let sidechain = sidechain.or(self.sidechain_from_main.then_some(input));
        if let (Some(port), Some(sidechain)) = (self.sidechain, sidechain) {
            self.write_stereo(port, sidechain, frames);
        }
    }

    /// Output side: copies the main port into the stereo `output` (mono to both channels).
    /// Without a main port the `input` passes through.
    fn read_to_stereo(&mut self, input: [&[f32]; 2], output: &mut [&mut [f32]; 2], frames: usize) {
        match self.main_channels() {
            0 => {
                for (dst, src) in output.iter_mut().zip(input) {
                    dst[..frames].copy_from_slice(&src[..frames]);
                }
            }
            1 => {
                let mono = self.main_channel(0, frames);
                output[0][..frames].copy_from_slice(mono);
                output[1][..frames].copy_from_slice(mono);
            }
            _ => {
                for (ch, dst) in output.iter_mut().enumerate() {
                    dst[..frames].copy_from_slice(self.main_channel(ch, frames));
                }
            }
        }
    }
}

/// Splits `data` into per-port channel lists of `frames` samples; the shape the clack buffer
/// builders want. No allocation.
fn split_ports<'a>(
    data: &'a mut [f32],
    channels: &'a [u32],
    stride: usize,
    frames: usize,
) -> impl ExactSizeIterator<Item = impl Iterator<Item = &'a mut [f32]>> {
    let mut rest = data;
    channels.iter().map(move |&count| {
        let (port, tail) = std::mem::take(&mut rest).split_at_mut(count as usize * stride);
        rest = tail;
        port.chunks_exact_mut(stride).map(move |c| &mut c[..frames])
    })
}

/// The audio-thread half of an active CLAP instance.
pub(crate) struct ClapProcessor {
    audio: PluginAudioProcessor<KpHost>,
    inputs: PortBuffers,
    outputs: PortBuffers,
    notes: NoteTarget,
    note_output: bool,
    events: InputEventList,
    /// Held notes: bit `c` of `held[key]` = channel `c`.
    held: [u16; 128],
    max_frames: u32,
    signal_taps: Option<SignalTaps>,
}

impl ClapProcessor {
    /// Main thread, at activation: allocates everything `process` needs.
    pub(crate) fn new(
        audio: StoppedPluginAudioProcessor<KpHost>,
        inputs: &PortLayout,
        outputs: &PortLayout,
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
            held: [0; 128],
            max_frames,
            signal_taps: None,
        }
    }

    pub(super) fn set_signal_taps(&mut self, taps: Option<SignalTaps>) {
        self.signal_taps = taps;
    }

    /// A note on (`velocity > 0`) or off. Channels ≥ 16 and keys ≥ 128 are dropped: the wire
    /// format does not range-check them, and they index `held`.
    fn note_event(&mut self, time: u32, channel: u8, key: u8, velocity: u8, note_id: u32) {
        if !valid_note(channel, key) {
            return;
        }
        let on = velocity > 0;
        let bit = 1u16 << channel;
        if on {
            self.held[key as usize] |= bit;
        } else {
            self.held[key as usize] &= !bit;
        }
        let event = match self.notes {
            NoteTarget::None => return,
            NoteTarget::Clap(port) => {
                let note_id = if note_id == NO_NOTE_ID {
                    -1
                } else {
                    (note_id & i32::MAX as u32) as i32
                };
                let pckn = Pckn::from_raw(port as i16, i16::from(channel), i16::from(key), note_id);
                if on {
                    InEvent::NoteOn(NoteOnEvent::new(time, pckn, f64::from(velocity) / 127.0))
                } else {
                    InEvent::NoteOff(NoteOffEvent::new(time, pckn, 0.0))
                }
            }
            NoteTarget::Midi(port) => {
                let status = if on { 0x90 } else { 0x80 } | channel;
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
                }) => self.note_event(time, channel, key, velocity, note_id),
                Some(WireEventKind::NoteOff {
                    channel,
                    key,
                    note_id,
                }) => self.note_event(time, channel, key, 0, note_id),
                Some(WireEventKind::AllNotesOff) => {
                    for key in 0..128u8 {
                        let mut channels = self.held[key as usize];
                        while channels != 0 {
                            let channel = channels.trailing_zeros() as u8;
                            channels &= channels - 1;
                            self.note_event(time, channel, key, 0, NO_NOTE_ID);
                        }
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

/// Whether `channel` and `key` are a MIDI channel (0 ..= 15) and key (0 ..= 127).
fn valid_note(channel: u8, key: u8) -> bool {
    channel < 16 && key < 128
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
            .fill_from_stereo(block.input, block.sidechain, frames);
        self.outputs.clear(frames);
        let transport = transport_event(block.transport);

        let result = {
            let inputs = &mut self.inputs;
            let outputs = &mut self.outputs;
            let in_buffers = inputs.ports.with_input_buffers(
                split_ports(&mut inputs.data, &inputs.channels, inputs.stride, frames).map(
                    |channels| AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(
                            channels.map(InputChannel::variable),
                        ),
                    },
                ),
            );
            let mut out_buffers = outputs.ports.with_output_buffers(
                split_ports(&mut outputs.data, &outputs.channels, outputs.stride, frames).map(
                    |channels| AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_output_only(channels),
                    },
                ),
            );
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
                self.outputs
                    .read_to_stereo(block.input, &mut block.output, frames);
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

    fn read_signal_taps(&mut self, mask: u32, out: &mut komponist_ipc::signal_taps::BlockTaps) {
        if let Some(taps) = &self.signal_taps {
            let _audio = AudioThreadScope::enter();
            taps.read(self.audio.plugin_handle().as_raw(), mask, out);
        }
    }

    fn reset(&mut self) {
        let _audio = AudioThreadScope::enter();
        self.audio.reset();
        self.held = [0; 128];
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

    #[test]
    fn split_ports_shapes_channels() {
        let mut data: Vec<f32> = (0..12).map(|i| i as f32).collect();
        let channels = [1u32, 2, 0, 1];
        let shapes: Vec<Vec<Vec<f32>>> = split_ports(&mut data, &channels, 3, 2)
            .map(|port| port.map(|c| c.to_vec()).collect())
            .collect();
        assert_eq!(
            shapes,
            vec![
                vec![vec![0.0, 1.0]],
                vec![vec![3.0, 4.0], vec![6.0, 7.0]],
                vec![],
                vec![vec![9.0, 10.0]],
            ]
        );
    }

    fn layout(channels: &[u32], main: Option<usize>) -> PortLayout {
        let mut layout = PortLayout {
            channels: channels.to_vec(),
            main,
            ..PortLayout::default()
        };
        layout.sidechain = layout.sidechain_port();
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
    fn the_sidechain_goes_into_its_port_or_silence() {
        let left = [1.0, 2.0, 3.0];
        let right = [3.0, 4.0, 5.0];
        let sc = [[0.5f32; 3], [-0.5f32; 3]];
        // Main stereo, a stereo sidechain port, a mono third port.
        let mut ports = PortBuffers::new(&layout(&[2, 2, 1], Some(0)), 4);
        ports.data.fill(9.0);
        ports.fill_from_stereo([&left, &right], Some([&sc[0], &sc[1]]), 3);
        assert_eq!(ports.main_channel(1, 3), &right);
        assert_eq!(ports.port_channel(1, 0, 3), &sc[0]);
        assert_eq!(ports.port_channel(1, 1, 3), &sc[1]);
        assert_eq!(ports.port_channel(2, 0, 3), &[0.0; 3]);
        // No sidechain this block: the port is silent again.
        ports.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(ports.port_channel(1, 0, 3), &[0.0; 3]);
        assert_eq!(ports.port_channel(1, 1, 3), &[0.0; 3]);
        // A mono sidechain port gets the mid signal.
        let mut ports = PortBuffers::new(&layout(&[2, 1], Some(0)), 3);
        ports.fill_from_stereo([&left, &right], Some([&left, &right]), 3);
        assert_eq!(ports.port_channel(1, 0, 3), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn opted_in_sidechain_uses_main_only_until_a_source_is_connected() {
        let input = [[1.0, 2.0], [3.0, 4.0]];
        let mut layout = layout(&[2, 2], Some(0));
        layout.sidechain_from_main = true;
        let mut ports = PortBuffers::new(&layout, 2);

        ports.fill_from_stereo([&input[0], &input[1]], None, 2);
        assert_eq!(ports.port_channel(1, 0, 2), &input[0]);
        assert_eq!(ports.port_channel(1, 1, 2), &input[1]);

        let silence = [0.0; 2];
        ports.fill_from_stereo([&input[0], &input[1]], Some([&silence, &silence]), 2);
        assert_eq!(ports.port_channel(1, 0, 2), &silence);
        assert_eq!(ports.port_channel(1, 1, 2), &silence);
    }

    #[test]
    fn stereo_input_adapts_to_the_main_port() {
        let left = [1.0, 2.0, 3.0];
        let right = [3.0, 4.0, 5.0];
        // Sidechain (2 ch) first, mono main second: main gets the mid signal, the rest is silent.
        let mut ports = PortBuffers::new(&layout(&[2, 1], Some(1)), 4);
        ports.data.fill(9.0);
        ports.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(ports.main_channel(0, 3), &[2.0, 3.0, 4.0]);
        assert_eq!(&ports.data[0..3], &[0.0; 3]);
        assert_eq!(&ports.data[4..7], &[0.0; 3]);
        // Four channels: L and R, then silence.
        let mut ports = PortBuffers::new(&layout(&[4], Some(0)), 3);
        ports.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(ports.main_channel(0, 3), &left);
        assert_eq!(ports.main_channel(1, 3), &right);
        assert_eq!(ports.main_channel(3, 3), &[0.0; 3]);
    }

    #[test]
    fn main_output_adapts_to_stereo() {
        let input = [[7.0f32; 2], [8.0f32; 2]];
        let input = [&input[0][..], &input[1][..]];
        let (mut l, mut r) = ([0.0f32; 2], [0.0f32; 2]);
        let mut ports = PortBuffers::new(&layout(&[1], Some(0)), 2);
        ports.main_channel(0, 2).copy_from_slice(&[0.5, -0.5]);
        ports.read_to_stereo(input, &mut [&mut l, &mut r], 2);
        assert_eq!((l, r), ([0.5, -0.5], [0.5, -0.5]));
        // No output port: the input passes through.
        let mut ports = PortBuffers::new(&PortLayout::default(), 2);
        ports.read_to_stereo(input, &mut [&mut l, &mut r], 2);
        assert_eq!((l, r), ([7.0; 2], [8.0; 2]));
    }

    #[test]
    fn out_of_range_notes_are_rejected() {
        assert!(valid_note(0, 0) && valid_note(15, 127));
        assert!(!valid_note(16, 60) && !valid_note(0, 128) && !valid_note(255, 255));
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
