# komponist-audio

Manages audio devices, streams and timestamped MIDI input for the DAW. The app adapts its engine to
the crate's `AudioProcess` trait.

## Structure

- [src/system.rs](src/system.rs) — Enumerates hosts and devices, negotiates stream settings and
  manages stream state.
- [src/backend.rs](src/backend.rs) — Implements stream backends, including the null backend.
- [src/jack_backend.rs](src/jack_backend.rs) — Handles explicit JACK routes and same-period capture
  and playback on Linux.
- [src/input.rs](src/input.rs) — Transfers and aligns captured audio.
- [src/midi.rs](src/midi.rs) — Enumerates MIDI ports and timestamps incoming events.
- [src/slot.rs](src/slot.rs) — Transfers ownership of the callback processor between streams.

## How it works

cpal drives CoreAudio, WASAPI, optional ASIO, PipeWire, JACK and ALSA. The native JACK path handles
explicit routing and capture. Separate input callbacks transfer samples through a bounded queue and
align them to the output clock. MIDI timestamps become offsets within audio blocks. On stream
teardown the slot returns the processor so the next stream can reuse it.
