//! Audio and MIDI I/O of komponist.
//!
//! cpal backends (PipeWire, JACK, ALSA) and a `NullBackend` for tests, the stream lifecycle with an
//! engine handoff that survives a stream rebuild, midir MIDI inputs with timestamps, and CPU load
//! and xrun statistics.
//!
//! Scaffold only: the contracts arrive with WP-01, the implementation with WP-13.
