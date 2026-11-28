//! What the CLAP and VST3 processors share (#243): the stereo adaptation of a plugin's audio ports
//! ([`StereoAdapter`]) and the held-note tracking behind all-notes-off ([`HeldNotes`]).
//!
//! komponist always hands a plugin stereo in and stereo out; the plugin's ports (CLAP) or buses
//! (VST3) are adapted:
//!
//! - **Input:** the main input port gets the stereo input; a mono port gets `(L + R) / 2`, a port
//!   with more than two channels gets L and R in its first two channels and silence in the rest.
//!   The sidechain port (#6) gets the block's sidechain the same way, or silence without one; a
//!   plugin may opt into the main input as the fallback for an unconnected sidechain. Every further
//!   input port is silent.
//! - **Output:** the main output port is read back; a mono port goes to both channels, a port with
//!   more channels contributes its first two; channels the plugin flags silent (VST3
//!   `silenceFlags`) are zeroed. Other output ports are rendered and discarded. A plugin without
//!   output ports passes the input through.
//!
//! Notes: a wire `NoteOn` is always a note-on, its velocity clamped into `1 ..= 127` (the wire
//! contract's range; a note end has its own kind, `NoteOff`). Channels ≥ 16 and keys ≥ 128 are
//! dropped (the wire format does not range-check them). All-notes-off becomes one note-off per
//! held note, so the event lists keep [`ALL_NOTES_OFF_ROOM`] spare slots.
//!
//! Everything is allocated in the constructors (activation, main thread); the rest is RT.

/// "No note id" in `WireEvent::note_id`.
pub(crate) const NO_NOTE_ID: u32 = u32::MAX;

/// Room in an input event list for an all-notes-off expanded to one note-off per held note.
pub(crate) const ALL_NOTES_OFF_ROOM: usize = 16 * 128;

/// Channel counts of one direction's audio ports (CLAP) or buses (VST3) and which ones are the
/// main and the sidechain port.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AudioLayout {
    pub(crate) channels: Vec<u32>,
    pub(crate) main: Option<usize>,
    /// Inputs only: the sidechain port (#6).
    pub(crate) sidechain: Option<usize>,
    /// Inputs only: an unconnected sidechain gets the main input (a plugin's opt-in).
    pub(crate) sidechain_from_main: bool,
}

impl AudioLayout {
    fn total_channels(&self) -> usize {
        self.channels.iter().map(|&c| c as usize).sum()
    }
}

/// Scratch audio for every port of one direction, channel after channel, `stride` = the largest
/// block; and the adaptation to and from stereo (module docs).
#[derive(Debug)]
pub(crate) struct StereoAdapter {
    channels: Vec<u32>,
    main: Option<usize>,
    sidechain: Option<usize>,
    sidechain_from_main: bool,
    /// Per port: the index of its first channel.
    firsts: Vec<usize>,
    stride: usize,
    /// Never resized after `new`, so pointers into it stay valid (VST3 hands them to the plugin).
    data: Vec<f32>,
}

impl StereoAdapter {
    /// Main thread, at activation: buffers for `layout` and blocks of up to `max_frames`.
    pub(crate) fn new(layout: &AudioLayout, max_frames: usize) -> StereoAdapter {
        let firsts = layout
            .channels
            .iter()
            .scan(0usize, |first, &count| {
                let this = *first;
                *first += count as usize;
                Some(this)
            })
            .collect();
        StereoAdapter {
            channels: layout.channels.clone(),
            main: layout.main,
            sidechain: layout.sidechain,
            // Only with a main port that has channels to take the signal from.
            sidechain_from_main: layout.sidechain_from_main
                && layout.main.is_some_and(|main| layout.channels[main] > 0),
            firsts,
            stride: max_frames,
            data: vec![0.0; layout.total_channels() * max_frames],
        }
    }

    /// The main port.
    pub(crate) fn main(&self) -> Option<usize> {
        self.main
    }

    /// The index of `port`'s first channel among all channels.
    pub(crate) fn first_channel(&self, port: usize) -> usize {
        self.firsts[port]
    }

    /// Main thread: a pointer to the first sample of every channel, in order. They stay valid
    /// while `self` lives (the buffer is never resized; moving `self` does not move it).
    pub(crate) fn channel_pointers(&mut self) -> Vec<*mut f32> {
        let stride = self.stride;
        let total: usize = self.channels.iter().map(|&c| c as usize).sum();
        let base = self.data.as_mut_ptr();
        // `wrapping_add`: with `stride == 0` every pointer is the (dangling) base, never read.
        (0..total)
            .map(|ch| base.wrapping_add(ch * stride))
            .collect()
    }

    /// Channel `ch` of `port`, `frames` long.
    pub(crate) fn port_channel(&mut self, port: usize, ch: usize, frames: usize) -> &mut [f32] {
        let start = (self.firsts[port] + ch) * self.stride;
        &mut self.data[start..start + frames]
    }

    /// Channel `ch` of the main port, `frames` long.
    pub(crate) fn main_channel(&mut self, ch: usize, frames: usize) -> &mut [f32] {
        let main = self.main.expect("a main port");
        self.port_channel(main, ch, frames)
    }

    fn main_channels(&self) -> usize {
        self.main.map_or(0, |m| self.channels[m] as usize)
    }

    /// RT. Per port, its channels' first `frames` samples; the shape the clack buffer builders
    /// want. No allocation.
    pub(crate) fn ports_mut(
        &mut self,
        frames: usize,
    ) -> impl ExactSizeIterator<Item = impl Iterator<Item = &mut [f32]>> {
        let stride = self.stride;
        let mut rest = self.data.as_mut_slice();
        self.channels.iter().map(move |&count| {
            let (port, tail) = std::mem::take(&mut rest).split_at_mut(count as usize * stride);
            rest = tail;
            port.chunks_exact_mut(stride.max(1))
                .map(move |c| &mut c[..frames])
        })
    }

    /// RT. Writes the stereo `src` into `port`, adapted to its channel count: a mono port gets the
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

    /// RT. Zeroes the first `frames` of every channel.
    pub(crate) fn clear(&mut self, frames: usize) {
        // `max(1)`: `chunks_exact_mut(0)` panics, and with `stride == 0` there is no sample.
        for channel in self.data.chunks_exact_mut(self.stride.max(1)) {
            channel[..frames].fill(0.0);
        }
    }

    /// RT, input side: silences every port, then writes the stereo `input` into the main port and
    /// the `sidechain` into its port, adapted to their channel counts. Without a sidechain this
    /// block, only a plugin that opted into the main-input fallback gets the main signal there.
    pub(crate) fn fill_from_stereo(
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

    /// RT, output side: copies the main port into the stereo `output` (mono to both channels);
    /// channel `c` is zeroed instead if bit `c` of `silent` is set. Without a main port the `input`
    /// passes through.
    pub(crate) fn read_to_stereo(
        &mut self,
        input: [&[f32]; 2],
        output: &mut [&mut [f32]; 2],
        frames: usize,
        silent: u64,
    ) {
        let copy = |this: &mut StereoAdapter, ch: usize, dst: &mut [f32]| {
            if silent & (1 << ch) != 0 {
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

    /// The whole scratch buffer (tests).
    #[cfg(test)]
    pub(crate) fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }
}

/// A note-on (`velocity` 1 ..= 127) or note-off (`velocity` 0) for a plugin, after [`HeldNotes`]
/// recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Note {
    pub(crate) channel: u8,
    pub(crate) key: u8,
    pub(crate) velocity: u8,
    /// The plugin's note id: `-1` for none, else the wire id's low 31 bits.
    pub(crate) note_id: i32,
}

impl Note {
    pub(crate) fn is_on(&self) -> bool {
        self.velocity > 0
    }
}

/// The note id a plugin gets for a wire note id: `-1` for [`NO_NOTE_ID`], else its low 31 bits
/// (CLAP and VST3 ids are non-negative `i32`s).
pub(crate) fn plugin_note_id(note_id: u32) -> i32 {
    if note_id == NO_NOTE_ID {
        -1
    } else {
        (note_id & i32::MAX as u32) as i32
    }
}

/// The notes a plugin holds, per key a bit per MIDI channel, so an all-notes-off can end each of
/// them with a note-off. RT: no allocation.
#[derive(Debug, Clone)]
pub(crate) struct HeldNotes {
    /// Bit `c` of `held[key]` = channel `c`.
    held: [u16; 128],
}

impl HeldNotes {
    pub(crate) fn new() -> HeldNotes {
        HeldNotes { held: [0; 128] }
    }

    /// RT. A note-on: recorded and returned, its velocity clamped into `1 ..= 127`. `None` for a
    /// channel ≥ 16 or a key ≥ 128.
    pub(crate) fn on(&mut self, channel: u8, key: u8, velocity: u8, note_id: u32) -> Option<Note> {
        let held = self.slot(channel, key)?;
        *held |= 1 << channel;
        Some(Note {
            channel,
            key,
            velocity: velocity.clamp(1, 127),
            note_id: plugin_note_id(note_id),
        })
    }

    /// RT. A note-off: recorded and returned (also for a note that was not held). `None` for a
    /// channel ≥ 16 or a key ≥ 128.
    pub(crate) fn off(&mut self, channel: u8, key: u8, note_id: u32) -> Option<Note> {
        let held = self.slot(channel, key)?;
        *held &= !(1 << channel);
        Some(Note {
            channel,
            key,
            velocity: 0,
            note_id: plugin_note_id(note_id),
        })
    }

    /// RT. A MIDI note message's note: a note-on with velocity 0 is a note-off (MIDI 1.0).
    pub(crate) fn midi(&mut self, channel: u8, key: u8, velocity: u8, on: bool) -> Option<Note> {
        if on && velocity > 0 {
            self.on(channel, key, velocity, NO_NOTE_ID)
        } else {
            self.off(channel, key, NO_NOTE_ID)
        }
    }

    /// RT. A note-off for every held note, by key, then channel; afterwards nothing is held.
    pub(crate) fn release_all(&mut self) -> impl Iterator<Item = Note> + use<> {
        let held = std::mem::replace(&mut self.held, [0; 128]);
        (0..128u8).flat_map(move |key| {
            let mut channels = held[key as usize];
            std::iter::from_fn(move || {
                if channels == 0 {
                    return None;
                }
                let channel = channels.trailing_zeros() as u8;
                channels &= channels - 1;
                Some(Note {
                    channel,
                    key,
                    velocity: 0,
                    note_id: -1,
                })
            })
        })
    }

    /// RT. Forgets every held note without note-offs.
    pub(crate) fn clear(&mut self) {
        self.held = [0; 128];
    }

    fn slot(&mut self, channel: u8, key: u8) -> Option<&mut u16> {
        if channel < 16 {
            self.held.get_mut(usize::from(key))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(channels: &[u32], main: Option<usize>, sidechain: Option<usize>) -> AudioLayout {
        AudioLayout {
            channels: channels.to_vec(),
            main,
            sidechain,
            sidechain_from_main: false,
        }
    }

    #[test]
    fn ports_mut_shapes_channels() {
        let mut ports = StereoAdapter::new(&layout(&[1, 2, 0, 1], Some(0), None), 3);
        for (i, sample) in ports.data_mut().iter_mut().enumerate() {
            *sample = i as f32;
        }
        let shapes: Vec<Vec<Vec<f32>>> = ports
            .ports_mut(2)
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

    #[test]
    fn channel_pointers_point_into_the_buffer() {
        let mut ports = StereoAdapter::new(&layout(&[2, 0, 1], Some(0), None), 4);
        let pointers = ports.channel_pointers();
        assert_eq!(pointers.len(), 3);
        let base = ports.data_mut().as_mut_ptr();
        for (ch, &pointer) in pointers.iter().enumerate() {
            assert_eq!(pointer, base.wrapping_add(ch * 4));
        }
        assert_eq!(ports.first_channel(2), 2);
        // No channels: no pointers.
        assert!(
            StereoAdapter::new(&AudioLayout::default(), 4)
                .channel_pointers()
                .is_empty()
        );
    }

    #[test]
    fn the_sidechain_goes_into_its_port_or_silence() {
        let left = [1.0, 2.0, 3.0];
        let right = [3.0, 4.0, 5.0];
        let sc = [[0.5f32; 3], [-0.5f32; 3]];
        // Main stereo, a stereo sidechain port, a mono third port.
        let mut ports = StereoAdapter::new(&layout(&[2, 2, 1], Some(0), Some(1)), 4);
        ports.data_mut().fill(9.0);
        ports.fill_from_stereo([&left, &right], Some([&sc[0], &sc[1]]), 3);
        assert_eq!(ports.main_channel(0, 3), &left);
        assert_eq!(ports.main_channel(1, 3), &right);
        assert_eq!(ports.port_channel(1, 0, 3), &sc[0]);
        assert_eq!(ports.port_channel(1, 1, 3), &sc[1]);
        assert_eq!(ports.port_channel(2, 0, 3), &[0.0; 3]);
        // No sidechain this block: the port is silent again.
        ports.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(ports.port_channel(1, 0, 3), &[0.0; 3]);
        assert_eq!(ports.port_channel(1, 1, 3), &[0.0; 3]);
        // A mono sidechain port gets the mid signal.
        let mut ports = StereoAdapter::new(&layout(&[2, 1], Some(0), Some(1)), 3);
        ports.fill_from_stereo([&left, &right], Some([&left, &right]), 3);
        assert_eq!(ports.port_channel(1, 0, 3), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn an_opted_in_sidechain_uses_main_only_until_a_source_is_connected() {
        let input = [[1.0, 2.0], [3.0, 4.0]];
        let mut opted = layout(&[2, 2], Some(0), Some(1));
        opted.sidechain_from_main = true;
        let mut ports = StereoAdapter::new(&opted, 2);
        ports.fill_from_stereo([&input[0], &input[1]], None, 2);
        assert_eq!(ports.port_channel(1, 0, 2), &input[0]);
        assert_eq!(ports.port_channel(1, 1, 2), &input[1]);
        let silence = [0.0; 2];
        ports.fill_from_stereo([&input[0], &input[1]], Some([&silence, &silence]), 2);
        assert_eq!(ports.port_channel(1, 0, 2), &silence);
        assert_eq!(ports.port_channel(1, 1, 2), &silence);
        // A main port without channels has nothing to fall back to.
        let mut empty_main = layout(&[0, 2], Some(0), Some(1));
        empty_main.sidechain_from_main = true;
        let mut ports = StereoAdapter::new(&empty_main, 2);
        ports.data_mut().fill(9.0);
        ports.fill_from_stereo([&input[0], &input[1]], None, 2);
        assert_eq!(ports.port_channel(1, 0, 2), &silence);
    }

    #[test]
    fn stereo_input_adapts_to_the_main_port() {
        let left = [1.0, 2.0, 3.0];
        let right = [3.0, 4.0, 5.0];
        // Sidechain (2 ch) first, mono main second: main gets the mid signal, the rest is silent.
        let mut ports = StereoAdapter::new(&layout(&[2, 1], Some(1), None), 4);
        ports.data_mut().fill(9.0);
        ports.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(ports.main_channel(0, 3), &[2.0, 3.0, 4.0]);
        assert_eq!(&ports.data_mut()[0..3], &[0.0; 3]);
        assert_eq!(&ports.data_mut()[4..7], &[0.0; 3]);
        // Four channels: L and R, then silence.
        let mut ports = StereoAdapter::new(&layout(&[4], Some(0), None), 3);
        ports.fill_from_stereo([&left, &right], None, 3);
        assert_eq!(ports.main_channel(0, 3), &left);
        assert_eq!(ports.main_channel(1, 3), &right);
        assert_eq!(ports.main_channel(3, 3), &[0.0; 3]);
    }

    #[test]
    fn the_main_output_adapts_to_stereo() {
        let input = [[7.0f32; 2], [8.0f32; 2]];
        let input = [&input[0][..], &input[1][..]];
        let (mut l, mut r) = ([0.0f32; 2], [0.0f32; 2]);
        // Mono to both channels.
        let mut ports = StereoAdapter::new(&layout(&[1], Some(0), None), 2);
        ports.main_channel(0, 2).copy_from_slice(&[0.5, -0.5]);
        ports.read_to_stereo(input, &mut [&mut l, &mut r], 2, 0);
        assert_eq!((l, r), ([0.5, -0.5], [0.5, -0.5]));
        // A mono channel flagged silent: both are zeroed.
        ports.read_to_stereo(input, &mut [&mut l, &mut r], 2, 0b1);
        assert_eq!((l, r), ([0.0; 2], [0.0; 2]));
        // A channel flagged silent is zeroed, the other copied.
        let mut ports = StereoAdapter::new(&layout(&[2], Some(0), None), 2);
        ports.data_mut().fill(0.25);
        ports.read_to_stereo(input, &mut [&mut l, &mut r], 2, 0b10);
        assert_eq!((l, r), ([0.25; 2], [0.0; 2]));
        // No output port: the input passes through.
        let mut ports = StereoAdapter::new(&AudioLayout::default(), 2);
        ports.read_to_stereo(input, &mut [&mut l, &mut r], 2, 0);
        assert_eq!((l, r), ([7.0; 2], [8.0; 2]));
    }

    /// `max_frames == 0` (CLAP used to panic in `clear`, VST3 did not): nothing to do, no panic.
    #[test]
    fn a_zero_frame_adapter_is_inert() {
        let mut ports = StereoAdapter::new(&layout(&[2], Some(0), None), 0);
        ports.clear(0);
        ports.fill_from_stereo([&[], &[]], None, 0);
        assert_eq!(ports.ports_mut(0).map(Iterator::count).sum::<usize>(), 0);
    }

    #[test]
    fn the_adapter_allocates_nothing_while_processing() {
        let left = [1.0f32; 4];
        let right = [2.0f32; 4];
        let mut ports = StereoAdapter::new(&layout(&[2, 1, 1], Some(0), Some(1)), 4);
        let (mut l, mut r) = ([0.0f32; 4], [0.0f32; 4]);
        assert_no_alloc::assert_no_alloc(|| {
            ports.fill_from_stereo([&left, &right], Some([&right, &left]), 4);
            let channels: usize = ports.ports_mut(4).map(Iterator::count).sum();
            ports.clear(4);
            ports.read_to_stereo([&left, &right], &mut [&mut l, &mut r], 4, 0);
            assert_eq!(channels, 4);
        });
    }

    #[test]
    fn notes_are_held_until_released() {
        let mut held = HeldNotes::new();
        let on = held.on(2, 60, 100, 7).unwrap();
        assert_eq!(
            on,
            Note {
                channel: 2,
                key: 60,
                velocity: 100,
                note_id: 7
            }
        );
        assert!(on.is_on());
        held.on(0, 60, 1, NO_NOTE_ID).unwrap();
        held.on(15, 127, 127, u32::MAX - 1).unwrap();
        held.on(1, 0, 64, NO_NOTE_ID).unwrap();
        // Ended before the all-notes-off: not released again.
        let off = held.off(1, 0, 9).unwrap();
        assert!(!off.is_on());
        assert_eq!(off.note_id, 9);
        let released: Vec<(u8, u8)> = held.release_all().map(|n| (n.key, n.channel)).collect();
        // By key, then channel.
        assert_eq!(released, [(60, 0), (60, 2), (127, 15)]);
        assert!(held.release_all().all(|n| !n.is_on() && n.note_id == -1));
        assert_eq!(held.release_all().count(), 0);
    }

    #[test]
    fn out_of_range_notes_are_dropped() {
        let mut held = HeldNotes::new();
        assert!(held.on(16, 60, 100, NO_NOTE_ID).is_none());
        assert!(held.on(0, 128, 100, NO_NOTE_ID).is_none());
        assert!(held.off(255, 255, NO_NOTE_ID).is_none());
        assert!(held.on(0, 0, 1, NO_NOTE_ID).is_some() && held.on(15, 127, 1, 0).is_some());
        assert_eq!(held.release_all().count(), 2);
    }

    /// The divergence #243 resolved: a wire note-on with velocity 0 (outside the wire contract)
    /// stays a note-on (CLAP read it as a note-off, VST3 clamped it), and above 127 it is clamped.
    #[test]
    fn a_wire_note_on_is_always_a_note_on_with_a_valid_velocity() {
        let mut held = HeldNotes::new();
        assert_eq!(held.on(0, 60, 0, NO_NOTE_ID).unwrap().velocity, 1);
        assert_eq!(held.on(0, 61, 200, NO_NOTE_ID).unwrap().velocity, 127);
        assert_eq!(held.release_all().count(), 2);
        // A MIDI note-on with velocity 0 is a note-off.
        held.on(3, 62, 90, NO_NOTE_ID);
        assert!(!held.midi(3, 62, 0, true).unwrap().is_on());
        assert!(held.midi(3, 62, 5, true).unwrap().is_on());
        assert!(!held.midi(3, 62, 5, false).unwrap().is_on());
        assert_eq!(held.release_all().count(), 0);
    }

    #[test]
    fn note_ids_map_to_non_negative_plugin_ids() {
        assert_eq!(plugin_note_id(NO_NOTE_ID), -1);
        assert_eq!(plugin_note_id(0), 0);
        assert_eq!(plugin_note_id(u32::MAX - 1), i32::MAX - 1);
        assert_eq!(plugin_note_id((1 << 31) | 5), 5);
    }

    #[test]
    fn held_notes_allocate_nothing() {
        let mut held = HeldNotes::new();
        let released = assert_no_alloc::assert_no_alloc(|| {
            for key in 0..128 {
                for channel in 0..16 {
                    held.on(channel, key, 100, NO_NOTE_ID);
                }
            }
            let released = held.release_all().count();
            held.on(0, 1, 1, 1);
            held.clear();
            released + held.release_all().count()
        });
        assert_eq!(released, ALL_NOTES_OFF_ROOM);
    }
}
