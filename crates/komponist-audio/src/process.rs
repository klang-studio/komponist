//! The callback contract between an audio stream and whatever it drives.
//!
//! `komponist-audio` does not know the engine. The app wraps `komponist_engine::Engine` in its own
//! type (orphan rule) and implements [`AudioProcess`] for it: map [`MidiIn`] events to
//! `komponist_engine::MidiInEvent`s, call `Engine::process` with planar buffers, write them with
//! [`OutputBuffer::write_stereo`].

use std::time::{Duration, Instant};

use crate::midi::{MidiReceiver, MidiStamped};

/// What a stream drives: the RT callback plus a non-RT preparation step.
///
/// `Send + 'static` because it moves into the stream's callback (another thread) inside an
/// [`crate::EngineSlot`] and comes back when the stream is torn down or detached.
pub trait AudioProcess: Send + 'static {
    /// Non-RT. Called on the UI thread with the parameters of a new stream before its first
    /// [`AudioProcess::process`] call: at start, after every rebuild (buffer or device change) and
    /// before [`crate::AudioSystem::attach`]. A changed sample rate must be applied here (the engine
    /// re-activates its processors).
    fn prepare(&mut self, params: &StreamParams);

    /// RT, audio thread. Fills `out` (every frame of every channel). Must not allocate, free, lock,
    /// log, do IO or block. `midi` yields the live MIDI messages that arrived before this callback.
    fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo, midi: &mut MidiIn<'_>);
}

/// Parameters of the stream an [`AudioProcess`] is prepared for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamParams {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Largest number of frames one callback may ask for (the requested buffer size, or the
    /// backend's maximum when it varies). Callbacks may be shorter.
    pub max_frames: u32,
    /// Output channels of the device (≥ 1). Channel 0/1 carry left/right.
    pub channels: u16,
}

/// Timing of one callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallbackInfo {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Frames requested by this callback.
    pub frames: u32,
    /// `Instant::now()` at callback entry; the reference for MIDI frame mapping.
    pub now: Instant,
    /// Time until frame 0 of this callback reaches the DAC (cpal playback − callback timestamp,
    /// plus device latency where known). Used for the latency display and recording correction.
    pub output_latency: Duration,
}

/// The interleaved output buffer of one callback.
#[derive(Debug)]
pub struct OutputBuffer<'a> {
    data: &'a mut [f32],
    channels: usize,
}

impl<'a> OutputBuffer<'a> {
    /// Wraps an interleaved buffer of `channels` channels (`data.len()` a multiple of `channels`).
    pub fn new(data: &'a mut [f32], channels: usize) -> OutputBuffer<'a> {
        OutputBuffer { data, channels }
    }

    /// Frames in the buffer.
    pub fn frames(&self) -> usize {
        self.data.len() / self.channels
    }

    /// Channels per frame.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The raw interleaved samples.
    pub fn data_mut(&mut self) -> &mut [f32] {
        self.data
    }

    /// RT. Writes planar stereo (`left.len() == right.len() == frames`) into the interleaved
    /// buffer: channel 0 = left, 1 = right, further channels silent; a mono device gets
    /// `(l + r) / 2`.
    pub fn write_stereo(&mut self, left: &[f32], right: &[f32]) {
        todo!("WP-13")
    }

    /// RT. Silences every sample.
    pub fn silence(&mut self) {
        self.data.fill(0.0);
    }
}

/// Live MIDI input of one callback.
///
/// Events are stamped with `Instant::now()` when midir delivers them. [`MidiIn::frame_of`] maps a
/// stamp into this callback: events received during the previous buffer period
/// `[now − period, now)` map linearly onto frames `0 .. frames`, earlier ones to 0. This adds one
/// buffer period of constant latency instead of jitter (about 1 ms accuracy, ARCHITECTURE §7).
#[derive(Debug)]
#[allow(dead_code)] // Contract stage: the fields are read by `pop` / `frame_of` (WP-13).
pub struct MidiIn<'a> {
    receiver: &'a mut MidiReceiver,
    info: CallbackInfo,
}

impl<'a> MidiIn<'a> {
    /// A view of `receiver` for the callback described by `info`.
    pub fn new(receiver: &'a mut MidiReceiver, info: CallbackInfo) -> MidiIn<'a> {
        MidiIn { receiver, info }
    }

    /// RT. The next event stamped before [`CallbackInfo::now`], oldest first, from all enabled
    /// ports.
    pub fn pop(&mut self) -> Option<MidiStamped> {
        todo!("WP-13")
    }

    /// RT. Frame offset `0 .. frames` of an event stamped at `time` (see the type docs).
    pub fn frame_of(&self, time: Instant) -> u32 {
        todo!("WP-13")
    }
}
