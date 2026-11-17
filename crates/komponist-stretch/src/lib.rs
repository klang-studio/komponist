//! Pitch-preserving, planar audio chunks for the read-ahead worker (#14 WP-B).
//!
//! This crate does not connect to audio hardware or the engine. Construction, reset, seek,
//! processing and destruction all belong off the audio thread. Instances may move between
//! workers, but must have one exclusive owner. The backend is vendored Signalsmith Default,
//! unity pitch, with a fixed seed and the same portable FFT on every platform. Repeatability
//! is within a build; bitwise equality across compilers/architectures is not guaranteed.
//!
//! Ratios mean **output duration / input duration** (0.5 is twice as fast). After construction
//! or a ratio change, read [`Stretcher::prime_frames`] source frames starting at the desired
//! position, zero-padding past EOF, and call [`Stretcher::seek`]. Those frames include source
//! look-ahead and discarded output delay; continue reading immediately after them. Then ask
//! [`Stretcher::input_frames_required`] for each output chunk and provide exactly that input.
//! Output chunks contain [`CHUNK_FRAMES`] frames; a shorter chunk ends the stream. This keeps
//! device callback sizes out of the algorithm. A future worker must buffer these chunks for
//! heads, rings and export. The caller controls finite output length with [`output_length`]
//! and pads source reads past EOF; there is no separate library drain or export quality mode.
//!
//! A minimal silent-source chunk (replace the source buffers with decoder reads):
//!
//! ```
//! use komponist_stretch::{Stretcher, CHUNK_FRAMES};
//! let mut stretch = Stretcher::new(48_000, 2, 0.75)?;
//! let lookahead = vec![0.0; stretch.prime_frames()];
//! stretch.seek(&[&lookahead, &lookahead])?;
//! let source = vec![0.0; stretch.input_frames_required(CHUNK_FRAMES)?];
//! let (mut left, mut right) = ([0.0; CHUNK_FRAMES], [0.0; CHUNK_FRAMES]);
//! stretch.process(&[&source, &source], &mut [&mut left, &mut right])?;
//! # Ok::<(), komponist_stretch::Error>(())
//! ```
//!
//! Seek and ratio changes deliberately require re-priming rather than promising click-free
//! live retiming. No project mapping, routing, voice cap or playback integration is added here.

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

/// Fixed worker output chunk; only a terminal chunk may be shorter.
pub const CHUNK_FRAMES: usize = 512;
/// Valid planar source headroom (60 dBFS). Larger finite values can overflow DSP intermediates.
pub const MAX_SAMPLE_MAGNITUDE: f32 = 1024.0;
/// Bounded counters keep integer/f64 frame mapping precise (over 265 days at 48 kHz).
pub const MAX_STREAM_FRAMES: u64 = 1 << 40;

/// Uncompensated backend latency at the configured sample rate. `seek` compensates both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// Source look-ahead frames before advancing the algorithm.
    pub input_frames: usize,
    /// Initial generated output frames discarded during priming.
    pub output_frames: usize,
}

/// A rejected call leaves state unchanged; a backend failure requires another reset/seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Only 8–192 kHz is supported by this bounded wrapper.
    SampleRate,
    /// Only mono and stereo are supported; this is unrelated to simultaneous voice counts.
    Channels,
    /// Duration ratio must be finite and in 0.5..=2.0.
    Ratio,
    /// Output must contain 1..=512 frames; a short output terminates the stream.
    OutputFrames,
    /// Wrong channel count/length, nonfinite audio, or excessive source magnitude.
    Input,
    /// Wrong output channel count or unequal planar lengths.
    Output,
    /// Supply source look-ahead with `seek` before processing.
    NotPrimed,
    /// A terminal short chunk has already been emitted; seek again to reuse the instance.
    Finished,
    /// A stream/output length exceeds the bounded precise mapping range.
    FrameLimit,
    /// The native backend could not allocate memory.
    Allocation,
    /// A native exception or unexpected ABI status was caught.
    Backend,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SampleRate => "stretch sample rate must be 8000..=192000 Hz",
            Self::Channels => "stretch supports one or two planar channels",
            Self::Ratio => "stretch duration ratio must be finite and 0.5..=2.0",
            Self::OutputFrames => "stretch output must contain 1..=512 frames",
            Self::Input => "stretch input has invalid dimensions, nonfinite or excessive samples",
            Self::Output => "stretch output has invalid planar dimensions",
            Self::NotPrimed => "stretch requires source priming after creation/reset/ratio changes",
            Self::Finished => "stretch emitted a terminal chunk; seek before processing again",
            Self::FrameLimit => "stretch frame mapping exceeds its precise bounded range",
            Self::Allocation => "stretch backend allocation failed",
            Self::Backend => "stretch backend failed; reset or seek before reuse",
        })
    }
}
impl std::error::Error for Error {}

fn validate_ratio(ratio: f64) -> Result<(), Error> {
    if ratio.is_finite() && (0.5..=2.0).contains(&ratio) {
        Ok(())
    } else {
        Err(Error::Ratio)
    }
}

/// Exact finite destination length policy. Source look-ahead and flush padding are excluded.
pub fn output_length(source_frames: u64, ratio: f64) -> Result<u64, Error> {
    validate_ratio(ratio)?;
    let length = (source_frames as f64 * ratio).round();
    if source_frames > MAX_STREAM_FRAMES || length > MAX_STREAM_FRAMES as f64 {
        return Err(Error::FrameLimit);
    }
    Ok(length as u64)
}

/// Unique worker-owned native state. No method is an audio-callback API.
pub struct Stretcher {
    native: NonNull<c_void>,
    channels: usize,
    sample_rate: u32,
    ratio: f64,
    latency: Latency,
    // Includes discarded output frames so every input boundary uses one cumulative mapping.
    produced: u64,
    primed: bool,
    finished: bool,
}

// SAFETY: the handle owns all native state, starts no threads and has no thread affinity/global
// mutable state. Mutating calls require &mut self. NonNull keeps this type !Sync.
unsafe impl Send for Stretcher {}

impl Stretcher {
    /// Create an unprimed mono/stereo instance at 8–192 kHz, ratio 0.5..=2.0.
    pub fn new(sample_rate: u32, channels: usize, ratio: f64) -> Result<Self, Error> {
        if !(8000..=192000).contains(&sample_rate) {
            return Err(Error::SampleRate);
        }
        if !(1..=2).contains(&channels) {
            return Err(Error::Channels);
        }
        validate_ratio(ratio)?;
        let mut native = std::ptr::null_mut();
        let mut latency = Latency {
            input_frames: 0,
            output_frames: 0,
        };
        // SAFETY: valid bounded configuration and exclusive writable out-parameters. Native
        // construction catches every exception; ownership of a successful handle passes to us.
        status(unsafe {
            kp_stretch_new(
                sample_rate,
                channels as u32,
                &mut native,
                &mut latency.input_frames,
                &mut latency.output_frames,
            )
        })?;
        let native = NonNull::new(native).ok_or(Error::Backend)?;
        Ok(Self {
            native,
            channels,
            sample_rate,
            ratio,
            latency,
            produced: 0,
            primed: false,
            finished: false,
        })
    }

    /// Number of planar channels (one or two).
    pub fn channels(&self) -> usize {
        self.channels
    }
    /// Common input/output sample rate; rate conversion belongs before this wrapper.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    /// Output/input duration ratio.
    pub fn ratio(&self) -> f64 {
        self.ratio
    }
    /// Algorithmic look-ahead/output delay before seek compensation.
    pub fn latency(&self) -> Latency {
        self.latency
    }

    /// Source frames to read from the seek position, including all algorithmic pre-roll.
    pub fn prime_frames(&self) -> usize {
        self.latency.input_frames
            + (self.latency.output_frames as f64 / self.ratio).round() as usize
    }

    /// Clear history/RNG state. Allocates and destroys native buffers; only call off-RT.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.primed = false;
        self.finished = false;
        self.produced = 0;
        // SAFETY: handle is live and exclusively borrowed; reset catches C++ exceptions.
        status(unsafe { kp_stretch_reset(self.native.as_ptr()) })
    }

    /// Change duration ratio and invalidate priming. The next seek supplies the new look-ahead.
    /// Even setting the existing ratio resets history; no live/gliding retime is promised.
    pub fn set_ratio(&mut self, ratio: f64) -> Result<(), Error> {
        validate_ratio(ratio)?;
        self.reset()?;
        self.ratio = ratio;
        Ok(())
    }

    /// Seek to the start of `input`: exactly `prime_frames()` samples on each channel.
    /// Resets history and seed, then discards algorithmic delay. Zero-pad unavailable EOF data.
    pub fn seek(&mut self, input: &[&[f32]]) -> Result<(), Error> {
        self.validate_input(input, self.prime_frames())?;
        self.reset()?;
        let ptrs = self.input_ptrs(input);
        // SAFETY: all channel slices are validated, equally long and live for this call; the
        // native prime function accesses only prime_frames() samples and catches exceptions.
        let result = status(unsafe {
            kp_stretch_prime(
                self.native.as_ptr(),
                ptrs.as_ptr(),
                self.prime_frames(),
                self.ratio,
            )
        });
        if result.is_ok() {
            self.produced = self.latency.output_frames as u64;
            self.primed = true;
        }
        result
    }

    /// Exact next source demand. `output_frames` is 512, except for a terminal 1..511 frames.
    pub fn input_frames_required(&self, output_frames: usize) -> Result<usize, Error> {
        if !self.primed {
            return Err(Error::NotPrimed);
        }
        if self.finished {
            return Err(Error::Finished);
        }
        if !(1..=CHUNK_FRAMES).contains(&output_frames) {
            return Err(Error::OutputFrames);
        }
        let next = self
            .produced
            .checked_add(output_frames as u64)
            .ok_or(Error::FrameLimit)?;
        if next > MAX_STREAM_FRAMES + self.latency.output_frames as u64 {
            return Err(Error::FrameLimit);
        }
        Ok(
            ((next as f64 / self.ratio).round() - (self.produced as f64 / self.ratio).round())
                as usize,
        )
    }

    /// Emit a full chunk or terminal partial chunk, at unity pitch. Validates before mutation.
    /// Input length must exactly match `input_frames_required(output[0].len())` on all channels.
    pub fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]]) -> Result<(), Error> {
        if output.len() != self.channels {
            return Err(Error::Output);
        }
        let frames = output[0].len();
        let required = self.input_frames_required(frames)?;
        if output.iter().any(|c| c.len() != frames) {
            return Err(Error::Output);
        }
        self.validate_input(input, required)?;
        let in_ptrs = self.input_ptrs(input);
        let mut out_ptrs = [std::ptr::null_mut(); 2];
        for (ptr, channel) in out_ptrs.iter_mut().zip(output.iter_mut()) {
            *ptr = channel.as_mut_ptr();
        }
        // SAFETY: exclusive, disjoint output slices and shared input slices have validated
        // dimensions/bounded samples. Counts are <=1024 input and <=512 output, fit C++ int.
        // The handle is live and exclusively borrowed; native code catches every exception.
        let result = status(unsafe {
            kp_stretch_process(
                self.native.as_ptr(),
                in_ptrs.as_ptr(),
                required,
                out_ptrs.as_ptr(),
                frames,
            )
        });
        if result.is_ok() {
            self.produced += frames as u64;
            self.finished = frames < CHUNK_FRAMES;
        } else {
            self.primed = false;
        }
        result
    }

    fn validate_input(&self, input: &[&[f32]], frames: usize) -> Result<(), Error> {
        if input.len() != self.channels
            || input.iter().any(|c| {
                c.len() != frames
                    || c.iter()
                        .any(|s| !s.is_finite() || s.abs() > MAX_SAMPLE_MAGNITUDE)
            })
        {
            return Err(Error::Input);
        }
        Ok(())
    }
    fn input_ptrs(&self, input: &[&[f32]]) -> [*const f32; 2] {
        let mut ptrs = [std::ptr::null(); 2];
        for (ptr, channel) in ptrs.iter_mut().zip(input) {
            *ptr = channel.as_ptr();
        }
        ptrs
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        // SAFETY: uniquely owned handle created by kp_stretch_new, destroyed exactly once;
        // native destruction is noexcept. Caller must arrange for this drop to occur off-RT.
        unsafe { kp_stretch_delete(self.native.as_ptr()) };
    }
}

fn status(code: i32) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        1 => Err(Error::Allocation),
        _ => Err(Error::Backend),
    }
}
unsafe extern "C" {
    fn kp_stretch_new(
        rate: u32,
        channels: u32,
        out: *mut *mut c_void,
        input_latency: *mut usize,
        output_latency: *mut usize,
    ) -> i32;
    fn kp_stretch_reset(handle: *mut c_void) -> i32;
    fn kp_stretch_prime(
        handle: *mut c_void,
        input: *const *const f32,
        frames: usize,
        ratio: f64,
    ) -> i32;
    fn kp_stretch_process(
        handle: *mut c_void,
        input: *const *const f32,
        input_frames: usize,
        output: *const *mut f32,
        output_frames: usize,
    ) -> i32;
    fn kp_stretch_delete(handle: *mut c_void);
}

#[cfg(test)]
mod tests;
