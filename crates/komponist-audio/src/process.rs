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

    /// RT. Duplex callback, with captured audio converted to the output clock. The default
    /// preserves output-only processors; engines that record or monitor override this method.
    fn process_input(
        &mut self,
        out: &mut OutputBuffer<'_>,
        info: &CallbackInfo,
        midi: &mut MidiIn<'_>,
        _input: &crate::InputBuffer<'_>,
    ) {
        self.process(out, info, midi);
    }
}

/// Parameters of the stream an [`AudioProcess`] is prepared for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamParams {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Largest number of frames one callback may ask for (the requested buffer size, or the
    /// backend's maximum when it varies). Callbacks may be shorter.
    pub max_frames: u32,
    /// Output channels of the device (≥ 1). Channel 0/1 carry left/right.
    pub channels: u16,
    /// The device's audio IO workgroup (macOS, retained; #45): threads that work to the
    /// callback's deadline besides the callback thread join it (the callback thread is a member
    /// already). `None` on Linux, for the null backend and for devices without one.
    pub workgroup: Option<komponist_rt::Workgroup>,
}

impl StreamParams {
    /// The period of a full callback (`max_frames` at `sample_rate`).
    pub fn period(&self) -> Duration {
        crate::frames_to_duration(u64::from(self.max_frames), self.sample_rate)
    }
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
    ///
    /// Shorter inputs leave the remaining frames silent.
    pub fn write_stereo(&mut self, left: &[f32], right: &[f32]) {
        debug_assert_eq!(left.len(), right.len());
        let channels = self.channels;
        let frames = self.frames().min(left.len()).min(right.len());
        let (head, tail) = self.data.split_at_mut(frames * channels);
        if channels == 1 {
            for ((out, l), r) in head.iter_mut().zip(left).zip(right) {
                *out = (l + r) * 0.5;
            }
        } else {
            for ((frame, l), r) in head.chunks_exact_mut(channels).zip(left).zip(right) {
                frame[0] = *l;
                frame[1] = *r;
                frame[2..].fill(0.0);
            }
        }
        tail.fill(0.0);
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
pub struct MidiIn<'a> {
    /// `None` when the stream has no MIDI receiver (yet).
    receiver: Option<&'a mut MidiReceiver>,
    info: CallbackInfo,
}

impl<'a> MidiIn<'a> {
    /// A view of `receiver` for the callback described by `info`.
    pub fn new(receiver: &'a mut MidiReceiver, info: CallbackInfo) -> MidiIn<'a> {
        MidiIn {
            receiver: Some(receiver),
            info,
        }
    }

    /// A view without a receiver: yields nothing.
    pub(crate) fn empty(info: CallbackInfo) -> MidiIn<'a> {
        MidiIn {
            receiver: None,
            info,
        }
    }

    /// RT. The next event stamped before [`CallbackInfo::now`], oldest first, from all enabled
    /// ports.
    pub fn pop(&mut self) -> Option<MidiStamped> {
        self.receiver.as_deref_mut()?.pop_before(self.info.now)
    }

    /// RT. Frame offset `0 .. frames` of an event stamped at `time` (see the type docs).
    pub fn frame_of(&self, time: Instant) -> u32 {
        let CallbackInfo {
            sample_rate,
            frames,
            now,
            ..
        } = self.info;
        if frames == 0 || sample_rate == 0 {
            return 0;
        }
        let period = u128::from(crate::frames_to_nanos(u64::from(frames), sample_rate));
        let age = now.saturating_duration_since(time).as_nanos();
        if age >= period {
            return 0;
        }
        // Linear: `now - period` is frame 0, `now` would be frame `frames`.
        let frame = (period - age) * u128::from(sample_rate) / 1_000_000_000;
        (frame as u32).min(frames - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::{MidiPortId, queue};
    use assert_no_alloc::assert_no_alloc;

    fn info(now: Instant, frames: u32) -> CallbackInfo {
        CallbackInfo {
            sample_rate: 48_000,
            frames,
            now,
            output_latency: Duration::ZERO,
        }
    }

    #[test]
    fn write_stereo_interleaves() {
        let mut data = [9.0f32; 6];
        assert_no_alloc(|| {
            OutputBuffer::new(&mut data, 2).write_stereo(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0]);
        });
        assert_eq!(data, [1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn write_stereo_downmixes_mono() {
        let mut data = [9.0f32; 3];
        assert_no_alloc(|| {
            OutputBuffer::new(&mut data, 1).write_stereo(&[1.0, 0.5, 0.0], &[0.0, 0.5, -1.0]);
        });
        assert_eq!(data, [0.5, 0.5, -0.5]);
    }

    #[test]
    fn write_stereo_silences_extra_channels_and_frames() {
        let mut data = [9.0f32; 12];
        assert_no_alloc(|| {
            let mut out = OutputBuffer::new(&mut data, 4);
            assert_eq!(out.frames(), 3);
            assert_eq!(out.channels(), 4);
            // One frame short: the last frame is silenced.
            out.write_stereo(&[1.0, 2.0], &[3.0, 4.0]);
        });
        assert_eq!(
            data,
            [1.0, 3.0, 0.0, 0.0, 2.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        let mut out = OutputBuffer::new(&mut data, 4);
        assert_no_alloc(|| out.silence());
        assert!(out.data_mut().iter().all(|&s| s == 0.0));
    }

    #[test]
    fn frame_of_maps_the_previous_period() {
        let (_sender, mut receiver) = queue(4);
        let now = Instant::now() + Duration::from_secs(1);
        // 480 frames at 48 kHz: a 10 ms period.
        let midi = MidiIn::new(&mut receiver, info(now, 480));
        let ms = Duration::from_millis;
        let frames = assert_no_alloc(|| {
            [
                midi.frame_of(now - ms(20)),
                midi.frame_of(now - ms(10)),
                midi.frame_of(now - ms(5)),
                midi.frame_of(now - Duration::from_micros(10)),
                midi.frame_of(now),
                midi.frame_of(now + ms(1)),
            ]
        });
        // Earlier than the period → 0; the middle → 240; just before now → the last frames.
        assert_eq!(frames, [0, 0, 240, 479, 479, 479]);
        let empty = MidiIn::new(&mut receiver, info(now, 0));
        assert_eq!(empty.frame_of(now - ms(1)), 0);
    }

    #[test]
    fn pop_yields_events_before_now_oldest_first() {
        let (sender, mut receiver) = queue(8);
        assert!(sender.push(MidiPortId(1), [0x90, 60, 100]));
        assert!(sender.push(MidiPortId(2), [0x80, 60, 0]));
        let now = Instant::now() + Duration::from_millis(1);
        std::thread::sleep(Duration::from_millis(2));
        // Arrives after the callback started: stays for the next one.
        assert!(sender.push(MidiPortId(1), [0xB0, 7, 64]));

        let mut midi = MidiIn::new(&mut receiver, info(now, 64));
        let popped = assert_no_alloc(|| [midi.pop(), midi.pop(), midi.pop()]);
        let [first, second, third] = popped;
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!((first.port, first.data), (MidiPortId(1), [0x90, 60, 100]));
        assert_eq!((second.port, second.data), (MidiPortId(2), [0x80, 60, 0]));
        assert!(first.time <= second.time);
        assert_eq!(third, None);

        let mut next = MidiIn::new(&mut receiver, info(Instant::now(), 64));
        let late = assert_no_alloc(|| next.pop()).unwrap();
        assert_eq!(late.data, [0xB0, 7, 64]);
        assert_eq!(MidiIn::empty(info(Instant::now(), 64)).pop(), None);
    }
}
