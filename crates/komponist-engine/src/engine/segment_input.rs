//! Prepare the audio capture window and MIDI input for one engine segment.

use komponist_model::TrackId;

use crate::api::TransportState;
use crate::program::{InputFilter, TrackProgram};
use crate::recording::CaptureWindow;
use crate::sequencer::LiveMsg;

use super::{Core, MidiInEvent, TrackState};

/// Audio frames that a track can submit to the recorder during this segment.
#[derive(Clone, Copy)]
pub(super) struct SegmentCapture {
    pub(super) window: CaptureWindow,
    frames: usize,
    /// Drain progress in frames, if the transport has ended the take.
    pub(super) draining: Option<u32>,
}

impl SegmentCapture {
    /// RT. With a tail, stop each input at its own latency correction.
    pub(super) fn frames_remaining(self, correction: u32) -> usize {
        self.draining.map_or(self.frames, |progress| {
            self.frames
                .min(correction.saturating_sub(progress) as usize)
        })
    }
}

impl Core {
    /// RT. Capture follows the live transport or the tail of the just-ended take.
    pub(super) fn segment_capture(
        &self,
        frames: usize,
        loop_samples: Option<(i64, i64)>,
    ) -> Option<SegmentCapture> {
        let window = |at, lap, lp| CaptureWindow {
            at,
            lap,
            lp,
            map: self.song.map,
            loop_range: self.song.loop_range,
        };
        if let Some(tail) = self.capture_tail {
            return Some(SegmentCapture {
                window: window(tail.pos, tail.lap, tail.cycle),
                frames: (tail.left as usize).min(frames),
                draining: Some(tail.total - tail.left),
            });
        }
        (self.transport.state == TransportState::Recording).then(|| SegmentCapture {
            window: window(
                self.transport.pos,
                self.capture_lap,
                self.recorder
                    .as_ref()
                    .and_then(|recorder| recorder.cycle_window(loop_samples)),
            ),
            frames,
            draining: None,
        })
    }
}

/// Live MIDI that can enter a track during this segment.
pub(super) struct SegmentMidi<'a> {
    pub(super) hardware: &'a [MidiInEvent],
    pub(super) direct_notes: &'a [(TrackId, u8, u8)],
    pub(super) first_frame: usize,
    pub(super) frames: usize,
    pub(super) recording: bool,
}

impl SegmentMidi<'_> {
    /// RT. Reuses the scratch list for pending releases, direct notes, then filtered hardware
    /// messages. Returns the overflow count used by the engine's dropped-event counter.
    pub(super) fn prepare(
        &self,
        track: &TrackProgram,
        state: &mut TrackState,
        live: &mut Vec<LiveMsg>,
    ) -> u32 {
        live.clear();
        let capture = track.arm && self.recording;
        for (channel, pending) in state.release_hardware.iter_mut().enumerate() {
            while *pending != 0 && live.len() < live.capacity() {
                let key = pending.trailing_zeros() as u8;
                *pending &= !(1u128 << key);
                live.push(LiveMsg {
                    offset: 0,
                    data: [0x80 | channel as u8, key, 0],
                    play: true,
                    capture: true,
                });
            }
        }
        for &(id, key, velocity) in self.direct_notes {
            if id == track.id && live.len() < live.capacity() {
                let status = if velocity > 0 { 0x90 } else { 0x80 };
                live.push(LiveMsg {
                    offset: 0,
                    data: [status, key, velocity],
                    play: true,
                    capture,
                });
            }
        }
        // An armed track monitors on its own ("Auto", owner decision 2026-11-15);
        // I monitors without arming.
        let play = track.monitor || track.arm;
        if play || capture {
            for message in self
                .hardware
                .iter()
                .filter(|message| accepts(&track.input, message))
            {
                if live.len() == live.capacity() {
                    return 1;
                }
                let frame = (message.frame as usize)
                    .clamp(self.first_frame, self.first_frame + self.frames - 1);
                if play {
                    let held = &mut state.hardware[usize::from(message.data[0] & 0x0f)];
                    let key_bit = 1u128 << (message.data[1] & 0x7f);
                    match message.data[0] & 0xf0 {
                        0x90 if message.data[2] & 0x7f != 0 => *held |= key_bit,
                        0x80 | 0x90 => *held &= !key_bit,
                        _ => {}
                    }
                }
                live.push(LiveMsg {
                    offset: (frame - self.first_frame) as u32,
                    data: message.data,
                    play,
                    capture,
                });
            }
        }
        0
    }
}

/// Whether a track listening with `filter` receives `message`.
fn accepts(filter: &InputFilter, message: &MidiInEvent) -> bool {
    let status = message.data[0];
    !filter.port_missing
        && (0x80..0xf0).contains(&status)
        && filter.port.is_none_or(|port| port == message.port)
        && filter
            .channel
            .is_none_or(|channel| channel == status & 0x0f)
}
