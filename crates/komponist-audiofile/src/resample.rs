//! Sample-rate conversion to the engine rate.
//!
//! **Choice: rubato's `Fft` resampler** (MIT OR Apache-2.0; its FFT dependency `realfft` is
//! already in the tree). klang-dsp has no resampler (`no_std` building blocks only, ARCHITECTURE
//! §10), and the converter runs offline or on a read-ahead thread, never in the audio thread, so
//! quality counts and CPU does not. The synchronous FFT converter is for a fixed ratio, which is
//! the case here (file rate to engine rate); it has no quality settings because it always
//! delivers its best. Measured on sines (`tests/it/resample.rs`): the error against the ideal
//! signal is ≈ −130 dB re the signal (the precision of `f32`) for 44.1/48/96/22.05 kHz pairs, the
//! passband is flat to 20 kHz, and a tone above the new Nyquist is rejected by > 140 dB. The
//! asynchronous sinc converter would only matter for a ratio that changes while playing (varispeed),
//! which audio clips do not have.
//!
//! Two interfaces:
//! - [`resample`]: a whole clip in memory (the delay is trimmed, the output has exactly
//!   `ceil(frames × ratio)` frames).
//! - [`StreamResampler`]: chunk by chunk, for the engine's read-ahead thread (#11): push decoded
//!   chunks of any size, collect output, [`StreamResampler::finish`] at the end of the file.

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Indexing, Resampler};

use crate::AudioError;

/// Input frames per converter call.
const CHUNK: usize = 1024;

/// Converts planar audio from `from` Hz to `to` Hz. Equal rates give a copy.
pub fn resample(channels: &[Vec<f32>], from: u32, to: u32) -> Result<Vec<Vec<f32>>, AudioError> {
    if from == to || channels.is_empty() {
        return Ok(channels.to_vec());
    }
    let frames = channels[0].len();
    let mut converter = StreamResampler::new(from, to, channels.len())?;
    let mut out =
        vec![Vec::with_capacity(converter.expected_frames(frames as u64) as usize); channels.len()];
    converter.push(channels, &mut out)?;
    converter.finish(&mut out)?;
    Ok(out)
}

/// A rate converter that takes its input in chunks. The output is time-aligned with the input:
/// the converter's own delay is trimmed, so output frame `n` is at time `n / to`, like input frame
/// `n` is at `n / from`. After a seek, call [`StreamResampler::reset`] and push audio starting at
/// the new position; the first few milliseconds of output then carry the filter's start-up
/// transient (as at the start of a file).
pub struct StreamResampler {
    /// `None`: both rates are equal, chunks pass through.
    inner: Option<Fft<f32>>,
    channels: usize,
    from: u64,
    to: u64,
    /// Input that does not fill a converter chunk yet, planar.
    pending: Vec<Vec<f32>>,
    chunk_in: Vec<Vec<f32>>,
    chunk_out: Vec<Vec<f32>>,
    /// Output frames of the converter's delay still to drop.
    skip: usize,
    frames_in: u64,
    frames_out: u64,
}

impl StreamResampler {
    /// A converter from `from` Hz to `to` Hz for `channels` channels.
    pub fn new(from: u32, to: u32, channels: usize) -> Result<StreamResampler, AudioError> {
        if from == 0 || to == 0 || channels == 0 {
            return Err(AudioError::Resample(format!(
                "{from} Hz to {to} Hz with {channels} channels"
            )));
        }
        let inner = if from == to {
            None
        } else {
            Some(
                Fft::<f32>::new(
                    from as usize,
                    to as usize,
                    CHUNK,
                    channels,
                    FixedSync::Input,
                )
                .map_err(|e| AudioError::Resample(e.to_string()))?,
            )
        };
        let mut s = StreamResampler {
            inner,
            channels,
            from: u64::from(from),
            to: u64::from(to),
            pending: vec![Vec::new(); channels],
            chunk_in: Vec::new(),
            chunk_out: Vec::new(),
            skip: 0,
            frames_in: 0,
            frames_out: 0,
        };
        s.reset();
        Ok(s)
    }

    /// Output frames for `input_frames` input frames: `ceil(input_frames × to / from)`.
    pub fn expected_frames(&self, input_frames: u64) -> u64 {
        (u128::from(input_frames) * u128::from(self.to)).div_ceil(u128::from(self.from)) as u64
    }

    /// Starts over (a new file, or a seek): forgets pending input and the filter state.
    pub fn reset(&mut self) {
        if let Some(inner) = &mut self.inner {
            inner.reset();
            self.skip = inner.output_delay();
        }
        for p in &mut self.pending {
            p.clear();
        }
        self.frames_in = 0;
        self.frames_out = 0;
    }

    /// Additional input frames needed for the next converter block. The initial blocks can
    /// produce no visible output while the filter delay is being discarded.
    pub fn frames_until_next_chunk(&self) -> usize {
        self.inner
            .as_ref()
            .map_or(1, |inner| inner.input_frames_next() - self.pending[0].len())
    }

    /// Converts `input` (one slice per channel, equal lengths) and appends the output to `out`.
    /// Input that does not fill a converter chunk waits for the next call.
    pub fn push<V: AsRef<[f32]>>(
        &mut self,
        input: &[V],
        out: &mut [Vec<f32>],
    ) -> Result<(), AudioError> {
        if input.len() != self.channels || out.len() != self.channels {
            return Err(AudioError::Resample("channel count".into()));
        }
        let frames = input[0].as_ref().len();
        // Unequal channels would leave the pending planes out of step (a slice past the end of
        // the shorter one in `run`).
        if input.iter().any(|channel| channel.as_ref().len() != frames) {
            return Err(AudioError::Resample("channels of unequal length".into()));
        }
        self.frames_in += frames as u64;
        if self.inner.is_none() {
            for (o, i) in out.iter_mut().zip(input) {
                o.extend_from_slice(i.as_ref());
            }
            self.frames_out += frames as u64;
            return Ok(());
        }
        for (p, i) in self.pending.iter_mut().zip(input) {
            p.extend_from_slice(i.as_ref());
        }
        while self.pending[0].len() >= self.inner.as_ref().map_or(0, |i| i.input_frames_next()) {
            self.run(None, out)?;
        }
        Ok(())
    }

    /// Flushes the converter at the end of the stream: converts the pending input and the filter's
    /// tail, so that exactly [`StreamResampler::expected_frames`] of everything pushed since the
    /// last reset have been delivered. The converter is reset afterwards.
    pub fn finish(&mut self, out: &mut [Vec<f32>]) -> Result<(), AudioError> {
        if self.inner.is_some() {
            let expected = self.expected_frames(self.frames_in);
            if !self.pending[0].is_empty() {
                self.run(Some(self.pending[0].len()), out)?;
            }
            // Pure silence pushes the filter's tail out. A rate pair with a small common divisor
            // (say 96001 Hz to 48 kHz) has an FFT block far larger than a converter chunk: nothing
            // comes out before a whole block has gone in, so the bound grows with the block.
            let limit = 64
                + 2 * self
                    .inner
                    .as_ref()
                    .map_or(0, |i| i.fft_size_in().div_ceil(CHUNK));
            let mut guard = 0;
            while self.frames_out < expected && guard < limit {
                self.run(Some(0), out)?;
                guard += 1;
            }
            let excess = self.frames_out.saturating_sub(expected) as usize;
            for o in out.iter_mut() {
                let keep = o.len().saturating_sub(excess);
                o.truncate(keep);
            }
        }
        self.reset();
        Ok(())
    }

    /// One converter call. `partial`: `Some(n)` uses the first `n` pending frames and zero-fills
    /// the chunk; `None` uses a full chunk.
    fn run(&mut self, partial: Option<usize>, out: &mut [Vec<f32>]) -> Result<(), AudioError> {
        let inner = self.inner.as_mut().expect("run is for a converter");
        let need = inner.input_frames_next();
        let max_out = inner.output_frames_max();
        let used = partial.unwrap_or(need);
        self.chunk_in.resize(self.channels, Vec::new());
        self.chunk_out.resize(self.channels, Vec::new());
        for (c, chunk) in self.chunk_in.iter_mut().enumerate() {
            chunk.clear();
            chunk.extend_from_slice(&self.pending[c][..used]);
            chunk.resize(need, 0.0);
        }
        for chunk in &mut self.chunk_out {
            chunk.resize(max_out, 0.0);
        }
        let indexing = Indexing {
            partial_len: partial,
            ..Indexing::default()
        };
        let adapter_err = |e: &dyn std::fmt::Display| AudioError::Resample(e.to_string());
        let input = SequentialSliceOfVecs::new(&self.chunk_in, self.channels, need)
            .map_err(|e| adapter_err(&e))?;
        let mut output =
            SequentialSliceOfVecs::new_mut(&mut self.chunk_out, self.channels, max_out)
                .map_err(|e| adapter_err(&e))?;
        let (consumed, produced) = inner
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| adapter_err(&e))?;
        let drain = if partial.is_some() { used } else { consumed };
        for p in &mut self.pending {
            p.drain(..drain);
        }
        let skip = self.skip.min(produced);
        self.skip -= skip;
        for (o, chunk) in out.iter_mut().zip(&self.chunk_out) {
            o.extend_from_slice(&chunk[skip..produced]);
        }
        self.frames_out += (produced - skip) as u64;
        Ok(())
    }
}
