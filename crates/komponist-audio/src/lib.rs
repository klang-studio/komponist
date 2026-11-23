//! Audio and MIDI I/O of komponist.
//!
//! cpal backends (CoreAudio on macOS; WASAPI, and ASIO with the feature `asio`, on Windows; PipeWire,
//! JACK, ALSA on Linux) and a null
//! backend for tests and "Kein Audio-Gerät", the
//! stream lifecycle with an engine handoff that survives a stream rebuild, midir MIDI inputs with
//! timestamps, and CPU load and xrun statistics.
//!
//! This crate does not depend on `komponist-engine`. It drives any [`AudioProcess`]; the app
//! implements that trait for a wrapper around the engine.
//!
//! Module map (ARCHITECTURE §7):
//! - [`process`]: the [`AudioProcess`] callback trait, [`OutputBuffer`], [`CallbackInfo`],
//!   [`MidiIn`].
//! - [`system`]: [`AudioSystem`], [`HostKind`], [`DeviceInfo`], [`DeviceId`], [`StreamConfig`],
//!   [`AudioState`], [`StreamStats`].
//! - [`slot`]: [`EngineSlot`], the engine handoff between streams.
//! - [`midi`]: [`MidiInputHub`], [`MidiStamped`], [`MidiPortId`].
//! - `asio_registry` (Windows): whether an ASIO driver is registered (`HKLM\SOFTWARE\ASIO`, read-only); ASIO is
//!   an available host only then.
//!
//! Threads: `AudioSystem` and `MidiInputHub` live on the UI thread; `EngineSlot::process` and
//! `AudioProcess::process` run on the audio thread; midir callbacks on MIDI threads.

// Missing SAFETY comments: about 10 (hand count: the Linux-only JACK backend, not compiled on macOS). Remove this allow when the crate is clean (#241 follow-up).
#![allow(clippy::undocumented_unsafe_blocks)]

#[cfg(windows)]
mod asio_registry;
mod backend;
pub mod input;
#[cfg(target_os = "linux")]
mod jack_backend;
mod jack_ports;
pub mod midi;
pub mod process;
pub mod slot;
pub mod system;
#[cfg(test)]
mod testing;
mod workgroup;

// Unit tests run the RT paths inside `assert_no_alloc`, which needs its allocator (debug builds
// only; in release builds `assert_no_alloc` is a no-op).
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub use input::{
    InputBuffer, InputConfig, InputDeviceInfo, InputDeviceRole, InputReader, RoutedInput,
    TestAudioSource, input_devices, test_input,
};
pub use jack_ports::{JackCaptureChannel, JackDirection, JackPort, JackRoutes, jack_ports};
pub use midi::{
    MidiError, MidiInputHub, MidiPortId, MidiPortInfo, MidiReceiver, MidiStamped, TestMidiSource,
};
pub use process::{AudioProcess, CallbackInfo, MidiIn, OutputBuffer, StreamParams};
pub use slot::{EngineSlot, SlotHandle};
pub use system::{
    AudioError, AudioState, AudioSystem, DeviceId, DeviceInfo, HostKind, StreamConfig, StreamStats,
};

/// RT-safe. Sets flush-to-zero and denormals-are-zero for the calling thread (MXCSR on x86_64,
/// FPCR on aarch64, no-op elsewhere). The audio callback calls it before running the engine
/// (ARCHITECTURE §6.1).
pub fn enable_flush_to_zero() {
    #[cfg(target_arch = "x86_64")]
    {
        // MXCSR bit 15 = FTZ (flush results to zero), bit 6 = DAZ (treat inputs as zero).
        const FTZ_DAZ: u32 = (1 << 15) | (1 << 6);
        let mut csr: u32 = 0;
        // SAFETY: `stmxcsr` stores the calling thread's SSE control register into `csr`, a valid
        // local; `ldmxcsr` loads it back with the two mode bits set. Nothing else is touched.
        unsafe {
            core::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags));
            if csr & FTZ_DAZ != FTZ_DAZ {
                csr |= FTZ_DAZ;
                core::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, preserves_flags));
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // FPCR bit 24 = FZ. AArch64 has no separate DAZ: FZ flushes denormal inputs as well.
        const FZ: u64 = 1 << 24;
        let fpcr: u64;
        // SAFETY: reads and writes only the calling thread's floating-point control register.
        unsafe {
            core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
            if fpcr & FZ == 0 {
                core::arch::asm!("msr fpcr, {}", in(reg) fpcr | FZ, options(nomem, nostack, preserves_flags));
            }
        }
    }
}

/// Duration of `frames` frames at `sample_rate` Hz, in nanoseconds (0 for a zero rate).
pub(crate) fn frames_to_nanos(frames: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    (u128::from(frames) * 1_000_000_000 / u128::from(sample_rate)) as u64
}

/// Duration of `frames` frames at `sample_rate` Hz.
pub(crate) fn frames_to_duration(frames: u64, sample_rate: u32) -> core::time::Duration {
    core::time::Duration::from_nanos(frames_to_nanos(frames, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_to_zero_flushes_denormals() {
        // The test thread is fresh, so the mode change stays local to it.
        let tiny = std::hint::black_box(f32::MIN_POSITIVE);
        let half = std::hint::black_box(0.5f32);
        assert_ne!(tiny * half, 0.0, "denormals are kept by default");
        enable_flush_to_zero();
        assert_eq!(std::hint::black_box(tiny) * half, 0.0);
        // Idempotent.
        enable_flush_to_zero();
        assert_eq!(std::hint::black_box(tiny) * half, 0.0);
    }

    #[test]
    fn frame_durations() {
        assert_eq!(frames_to_nanos(48_000, 48_000), 1_000_000_000);
        assert_eq!(frames_to_nanos(256, 48_000), 5_333_333);
        assert_eq!(
            frames_to_nanos(u64::from(u32::MAX), 1),
            4_294_967_295_000_000_000
        );
        assert_eq!(frames_to_nanos(10, 0), 0);
    }
}
