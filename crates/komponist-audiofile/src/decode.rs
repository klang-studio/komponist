//! Seekable, chunked decoding of audio files, and whole-file decoding on top of it.
//!
//! WAV goes through `hound` (8/16/24/32-bit integer and 32-bit float PCM; sample-exact frame
//! seeks). FLAC, OGG Vorbis and MP3 go through `symphonia`: a seek lands [`PREROLL`] frames
//! before the target so the decoder's overlap and bit-reservoir state is rebuilt, and the frames
//! before the target are dropped. Every decoded buffer is placed by its packet's end timestamp,
//! so seeks are frame-exact (the tests compare them with a sequential decode, sample by sample).
//!
//! Which decoder opens a file is decided by its first bytes, never by the extension.
//!
//! Output is always `f32` and interleaved; files with more than [`MAX_CHANNELS`] channels
//! deliver their first two.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::Path;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Duration, Time, TimeBase, Timestamp};

use crate::{AudioError, MAX_CHANNELS};

/// Highest sample rate accepted; a header claiming more is damaged or hostile.
pub const MAX_SAMPLE_RATE: u32 = 768_000;

/// Most frames [`decode_all`] holds in memory (2^27: 46 minutes at 48 kHz, 1 GiB in stereo).
pub const MAX_DECODE_FRAMES: u64 = 1 << 27;

/// Largest timestamp magnitude (in container ticks) that is believed.
const MAX_TICKS: u64 = 1 << 50;

/// Frames a whole-file decode reserves up front; the vectors grow with what is really decoded.
const INITIAL_FRAMES: usize = 1 << 20;

/// Frames a whole-file decode reads per call.
const CHUNK_FRAMES: usize = 16 * 1024;

/// Frames a seek lands before its target, so the decoder's overlap and bit-reservoir state is rebuilt before the first delivered frame.
const PREROLL: u64 = 8192;

/// The container/codec a file was opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// RIFF WAVE, PCM.
    Wav,
    /// AIFF / AIFF-C PCM.
    Aiff,
    /// Native FLAC.
    Flac,
    /// OGG Vorbis.
    Ogg,
    /// MPEG layer III.
    Mp3,
}

/// What a file is, as far as playback needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInfo {
    /// Sample rate of the file in Hz.
    pub sample_rate: u32,
    /// Channels the decoder delivers: 1 or 2.
    pub channels: usize,
    /// Channels the file has (more than [`AudioInfo::channels`] are dropped).
    pub source_channels: usize,
    /// Length in frames at `sample_rate`. For WAV this is bounded by the file size. Reads can
    /// still report missing declared samples. For FLAC, OGG and MP3 it is the container's count
    /// or the sum of packet durations when absent.
    pub frames: u64,
    /// Container/codec.
    pub format: Format,
}

impl AudioInfo {
    /// Length in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate)
    }
}

/// A file opened for decoding. Seek to any frame and read interleaved chunks from there; this
/// is the interface for the engine's read-ahead thread (#11). `Send`, not `Sync`: one reader
/// thread owns it.
pub struct AudioFile {
    info: AudioInfo,
    /// Frame the next [`AudioFile::read`] starts at.
    position: u64,
    backend: Backend,
}

enum Backend {
    Wav(Wav),
    Sym(Box<Sym>),
}

impl AudioFile {
    /// Opens `path` and reads its header. The position is frame 0.
    pub fn open(path: &Path) -> Result<AudioFile, AudioError> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 12];
        let n = read_up_to(&mut file, &mut magic)?;
        let format = sniff(&magic[..n]);
        file.rewind()?;
        let (info, backend) = match format {
            Format::Wav => {
                let (info, wav) = Wav::open(file)?;
                (info, Backend::Wav(wav))
            }
            _ => {
                let (info, sym) = Sym::open(path, file, format)?;
                (info, Backend::Sym(Box::new(sym)))
            }
        };
        if info.frames == 0 {
            return Err(AudioError::Empty);
        }
        Ok(AudioFile {
            info,
            position: 0,
            backend,
        })
    }

    /// The file's properties.
    pub fn info(&self) -> &AudioInfo {
        &self.info
    }

    /// The frame the next [`AudioFile::read`] starts at.
    pub fn position(&self) -> u64 {
        self.position
    }

    fn declared_end(&self) -> u64 {
        match &self.backend {
            Backend::Wav(wav) => wav.declared_frames,
            Backend::Sym(_) => self.info.frames,
        }
    }

    /// Moves to `frame`; a frame at or past the declared end puts the position there (reads give 0).
    pub fn seek(&mut self, frame: u64) -> Result<(), AudioError> {
        let frame = frame.min(self.declared_end());
        match &mut self.backend {
            Backend::Wav(w) => w.seek(frame)?,
            Backend::Sym(s) => s.seek(frame, self.info.frames)?,
        }
        self.position = frame;
        Ok(())
    }

    /// Reads up to `out.len() / channels` frames into `out` (interleaved, `info().channels`
    /// channels) and returns the number of frames written; 0 at the end of the file. A short
    /// count before the end is possible for the compressed formats (a packet boundary).
    pub fn read(&mut self, out: &mut [f32]) -> Result<usize, AudioError> {
        let channels = self.info.channels;
        let usable = out.len() / channels * channels;
        let out = &mut out[..usable];
        let remaining = self.declared_end().saturating_sub(self.position);
        if out.is_empty() || remaining == 0 {
            return Ok(0);
        }
        let n = match &mut self.backend {
            Backend::Wav(w) => w.read(out, remaining)?,
            Backend::Sym(s) => {
                let n = s.read(out)?;
                if n == 0
                    && let Some(end) = s.declared_frames.filter(|&end| self.position < end)
                {
                    return Err(AudioError::Malformed(format!(
                        "stream ended at frame {} before the declared end at frame {end}",
                        self.position,
                    )));
                }
                n
            }
        };
        self.position += n as u64;
        Ok(n)
    }

    /// Like [`AudioFile::read`], but keeps reading until `out` is full or the file ends.
    pub fn read_full(&mut self, out: &mut [f32]) -> Result<usize, AudioError> {
        let channels = self.info.channels;
        let mut frames = 0;
        while (frames + 1) * channels <= out.len() {
            let n = self.read(&mut out[frames * channels..])?;
            if n == 0 {
                break;
            }
            frames += n;
        }
        Ok(frames)
    }
}

impl std::fmt::Debug for AudioFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioFile")
            .field("info", &self.info)
            .field("position", &self.position)
            .finish()
    }
}

fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize, AudioError> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

fn sniff(magic: &[u8]) -> Format {
    if magic.len() >= 12 && &magic[..4] == b"RIFF" && &magic[8..12] == b"WAVE" {
        Format::Wav
    } else if magic.len() >= 12
        && &magic[..4] == b"FORM"
        && matches!(&magic[8..12], b"AIFF" | b"AIFC")
    {
        Format::Aiff
    } else if magic.starts_with(b"fLaC") {
        Format::Flac
    } else if magic.starts_with(b"OggS") {
        Format::Ogg
    } else {
        // ID3 tag or a bare frame sync; symphonia refuses what is not MP3.
        Format::Mp3
    }
}

// ---------------------------------------------------------------------------------------- WAV

struct Wav {
    reader: Option<hound::WavReader<BufReader<File>>>,
    spec: hound::WavSpec,
    declared_frames: u64,
    // The active reader's cursor is discarded on error. Keep the underlying handle for a retry.
    failed_file: Option<File>,
}

fn wav_error(error: hound::Error) -> AudioError {
    match error {
        hound::Error::IoError(e) => AudioError::Io(e.to_string()),
        hound::Error::Unsupported => AudioError::Unsupported("WAV variant".into()),
        e => AudioError::Malformed(e.to_string()),
    }
}

impl Wav {
    fn open(file: File) -> Result<(AudioInfo, Wav), AudioError> {
        let file_len = file.metadata()?.len();
        let reader = hound::WavReader::new(BufReader::new(file)).map_err(wav_error)?;
        let spec = reader.spec();
        match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Float, 32) | (hound::SampleFormat::Int, 8 | 16 | 24 | 32) => {}
            (_, bits) => {
                return Err(AudioError::Unsupported(format!("{bits}-bit WAV")));
            }
        }
        if spec.sample_rate == 0 || spec.channels == 0 {
            return Err(AudioError::Malformed("no sample rate or channels".into()));
        }
        if spec.sample_rate > MAX_SAMPLE_RATE {
            return Err(AudioError::Unsupported(format!("{} Hz", spec.sample_rate)));
        }
        let declared_frames = u64::from(reader.duration());
        let bytes_per_frame = u64::from(spec.channels) * u64::from(spec.bits_per_sample / 8);
        // Keep metadata/allocation bounds conservative, including for implausible headers. The
        // reader separately checks the header's declared length when it encounters missing data.
        let holds = file_len.saturating_sub(44) / bytes_per_frame;
        let source_channels = usize::from(spec.channels);
        let info = AudioInfo {
            sample_rate: spec.sample_rate,
            channels: source_channels.min(MAX_CHANNELS),
            source_channels,
            frames: declared_frames.min(holds),
            format: Format::Wav,
        };
        Ok((
            info,
            Wav {
                reader: Some(reader),
                spec,
                declared_frames,
                failed_file: None,
            },
        ))
    }

    fn seek(&mut self, frame: u64) -> Result<(), AudioError> {
        let frame = u32::try_from(frame).map_err(|_| AudioError::TooLong)?;
        if let Some(file) = &self.failed_file {
            // Hound advances its sample count even if a read fails. Rebuild with a temporary
            // duplicate of the same file handle, keeping the original if the header is unreadable.
            let mut candidate = file.try_clone()?;
            candidate.rewind()?;
            let reader = hound::WavReader::new(BufReader::new(candidate)).map_err(wav_error)?;
            if reader.spec() != self.spec {
                return Err(AudioError::Malformed(
                    "WAV format changed while reading".into(),
                ));
            }
            self.reader = Some(reader);
            self.failed_file = None;
        }
        let reader = self.reader.as_mut().ok_or_else(wav_decoder_unavailable)?;
        let result = reader.seek(frame).map_err(AudioError::from);
        if result.is_err() {
            self.discard_cursor();
        }
        result
    }

    /// Reads at most `remaining` frames.
    fn read(&mut self, out: &mut [f32], remaining: u64) -> Result<usize, AudioError> {
        let reader = self.reader.as_mut().ok_or_else(wav_decoder_unavailable)?;
        let source = usize::from(self.spec.channels);
        let channels = source.min(MAX_CHANNELS);
        let frames = (out.len() / channels).min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let mut fill = |next: &mut dyn FnMut() -> Result<f32, hound::Error>| {
            for frame in out.chunks_exact_mut(channels).take(frames) {
                for c in 0..source {
                    let v = next()?;
                    if let Some(slot) = frame.get_mut(c) {
                        *slot = v;
                    }
                }
            }
            Ok::<_, hound::Error>(())
        };
        let unexpected_end = || hound::Error::IoError(io::ErrorKind::UnexpectedEof.into());
        let done = match (self.spec.sample_format, self.spec.bits_per_sample) {
            (hound::SampleFormat::Float, _) => {
                let mut it = reader.samples::<f32>();
                fill(&mut || it.next().unwrap_or_else(|| Err(unexpected_end())))
            }
            (_, bits) => {
                let scale = 1.0 / (1i64 << (bits - 1)) as f32;
                let mut it = reader.samples::<i32>();
                fill(&mut || {
                    it.next()
                        .unwrap_or_else(|| Err(unexpected_end()))
                        .map(|v| v as f32 * scale)
                })
            }
        };
        if let Err(error) = done {
            self.discard_cursor();
            return Err(wav_error(error));
        }
        Ok(frames)
    }

    fn discard_cursor(&mut self) {
        self.failed_file = self
            .reader
            .take()
            .map(|reader| reader.into_inner().into_inner());
    }
}

fn wav_decoder_unavailable() -> AudioError {
    AudioError::Malformed("seek required after a failed WAV read".into())
}

// ----------------------------------------------------------------------------------- symphonia

struct Sym {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    sample_rate: u32,
    channels: usize,
    /// Scratch for one decoded packet, planar.
    planar: Vec<Vec<f32>>,
    /// Decoded, interleaved, not yet delivered.
    pending: Vec<f32>,
    pending_pos: usize,
    /// The frame delivery starts at: decoded frames before it are dropped (the distance between
    /// a seek's landing point and its target, and the encoder delay at the start of a file).
    target: u64,
    ended: bool,
    /// A duration declared by the container, excluding inferred packet counts or estimates.
    declared_frames: Option<u64>,
}

fn map_sym(e: SymError) -> AudioError {
    match e {
        SymError::IoError(e) => AudioError::Io(e.to_string()),
        SymError::Unsupported(s) => AudioError::Unsupported(s.into()),
        SymError::DecodeError(s) | SymError::LimitError(s) => AudioError::Malformed(s.into()),
        SymError::SeekError(k) => AudioError::Malformed(format!("seek: {k:?}")),
        SymError::ResetRequired => AudioError::Unsupported("chained stream".into()),
        e => AudioError::Malformed(e.to_string()),
    }
}

impl Sym {
    fn open(path: &Path, mut file: File, expected: Format) -> Result<(AudioInfo, Sym), AudioError> {
        let mp3_declared = if expected == Format::Mp3 {
            // Symphonia estimates the duration of seekable MP3s without a duration tag. Inspect
            // the same file as a stream to find out which length is declared in its metadata.
            let declared = Self::probe_format(path, Box::new(ReadOnlySource::new(&mut file)))
                .ok()
                .and_then(|format| {
                    format
                        .default_track(TrackType::Audio)
                        .and_then(|track| track.num_frames)
                });
            file.rewind()?;
            declared
        } else {
            None
        };
        let (mut format, mut decoder, track_id) = Self::probe(path, file)?;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.id == track_id)
            .ok_or_else(|| AudioError::Unsupported("no audio track".into()))?;
        let time_base = track.time_base;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| AudioError::Unsupported("no audio codec parameters".into()))?;
        let mut sample_rate = params.sample_rate;
        let mut source_channels = params.channels.as_ref().map(|c| c.count());
        let frames = track.num_frames;
        let declared_frames = if expected == Format::Mp3 {
            frames.filter(|&n| mp3_declared == Some(n))
        } else {
            frames
        };
        // Rate or channels missing from the header: they are in the first decoded packet.
        let mut planar = Vec::new();
        if sample_rate.is_none() || source_channels.is_none() {
            while let Some(packet) = format.next_packet().map_err(map_sym)? {
                if packet.track_id != track_id {
                    continue;
                }
                if let Ok(buf) = decoder.decode(&packet) {
                    sample_rate = Some(buf.spec().rate());
                    source_channels = Some(buf.spec().channels().count());
                    break;
                }
            }
        }
        let (Some(sample_rate), Some(source_channels)) = (sample_rate, source_channels) else {
            return Err(AudioError::Malformed("no sample rate or channels".into()));
        };
        if sample_rate == 0 || source_channels == 0 {
            return Err(AudioError::Malformed("no sample rate or channels".into()));
        }
        if sample_rate > MAX_SAMPLE_RATE {
            return Err(AudioError::Unsupported(format!("{sample_rate} Hz")));
        }
        let mut sym = Sym {
            format,
            decoder,
            track_id,
            time_base,
            sample_rate,
            channels: source_channels.min(MAX_CHANNELS),
            planar: std::mem::take(&mut planar),
            pending: Vec::new(),
            pending_pos: 0,
            target: 0,
            ended: false,
            declared_frames,
        };
        // Probing consumed packets. Start at the top, including before any duration scan.
        sym.rewind()?;
        let frames = match frames {
            Some(frames) => frames,
            None => {
                let frames = sym.sum_packets()?;
                sym.rewind()?;
                frames
            }
        };
        let info = AudioInfo {
            sample_rate,
            channels: sym.channels,
            source_channels,
            frames,
            format: expected,
        };
        Ok((info, sym))
    }

    #[allow(clippy::type_complexity)]
    fn probe(
        path: &Path,
        file: File,
    ) -> Result<(Box<dyn FormatReader>, Box<dyn AudioDecoder>, u32), AudioError> {
        let format = Self::probe_format(path, Box::new(file))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| AudioError::Unsupported("no audio track".into()))?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| AudioError::Unsupported("no audio codec parameters".into()))?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| match e {
                SymError::Unsupported(s) => AudioError::Unsupported(s.into()),
                e => map_sym(e),
            })?;
        Ok((format, decoder, track_id))
    }

    fn probe_format<'a>(
        path: &Path,
        media: Box<dyn MediaSource + 'a>,
    ) -> Result<Box<dyn FormatReader + 'a>, AudioError> {
        let mss = MediaSourceStream::new(media, Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| match e {
                SymError::Unsupported(_) => AudioError::Unsupported("unknown audio format".into()),
                e => map_sym(e),
            })
    }

    /// Length in frames from the packet durations, reading the file once without decoding.
    fn sum_packets(&mut self) -> Result<u64, AudioError> {
        let mut ticks = 0u64;
        while let Some(packet) = self.format.next_packet().map_err(map_sym)? {
            if packet.track_id == self.track_id {
                let dur = packet
                    .dur
                    .saturating_sub(packet.trim_start)
                    .saturating_sub(packet.trim_end);
                ticks = ticks
                    .checked_add(dur.get())
                    .filter(|t| *t <= MAX_TICKS)
                    .ok_or_else(|| AudioError::Malformed("implausible length".into()))?;
            }
        }
        Ok(ticks_to_frames(self.time_base, ticks, self.sample_rate))
    }

    fn rewind(&mut self) -> Result<(), AudioError> {
        self.seek(0, u64::MAX)
    }

    fn seek(&mut self, frame: u64, total_frames: u64) -> Result<(), AudioError> {
        self.pending.clear();
        self.pending_pos = 0;
        self.target = frame;
        self.ended = frame >= total_frames;
        if self.ended {
            return Ok(());
        }
        let ts = frames_to_ts(
            self.time_base,
            frame.saturating_sub(PREROLL),
            self.sample_rate,
        );
        self.format
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts,
                    track_id: self.track_id,
                },
            )
            .map_err(map_sym)?;
        // Where the seek landed does not matter: every packet carries its own position.
        self.decoder.reset();
        Ok(())
    }

    fn read(&mut self, out: &mut [f32]) -> Result<usize, AudioError> {
        while self.pending_pos >= self.pending.len() {
            if self.ended || !self.fill()? {
                self.ended = true;
                return Ok(0);
            }
        }
        let n = (self.pending.len() - self.pending_pos).min(out.len());
        out[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
        self.pending_pos += n;
        Ok(n / self.channels)
    }

    /// Decodes packets until one gives frames past `skip`; `false` at the end of the stream.
    fn fill(&mut self) -> Result<bool, AudioError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) | Err(SymError::ResetRequired) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(e) => return Err(map_sym(e)),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            if packet.pts.get().unsigned_abs() > MAX_TICKS || packet.dur.get() > MAX_TICKS {
                return Err(AudioError::Malformed("implausible timestamp".into()));
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                // A damaged packet is skipped (the stream resyncs), like every player does.
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(map_sym(e)),
            };
            buf.copy_to_vecs_planar::<f32>(&mut self.planar);
            let frames = buf.frames();
            // Where this buffer's frames are in the file: they end where the packet ends. A
            // decoder that holds output back (Vorbis' first packet after a reset gives nothing,
            // its successor gives two windows' worth) is placed right by this, which a count of
            // decoded frames since the seek would not.
            let end = ts_to_frames(
                self.time_base,
                packet.pts.saturating_add(packet.dur),
                self.sample_rate,
            );
            let start = end.saturating_sub(frames as i64);
            let target = i64::try_from(self.target).unwrap_or(i64::MAX);
            let first = usize::try_from(target.saturating_sub(start).max(0))
                .unwrap_or(usize::MAX)
                .min(frames);
            if first == frames {
                continue;
            }
            self.pending.clear();
            self.pending_pos = 0;
            for i in first..frames {
                for plane in self.planar.iter().take(self.channels) {
                    self.pending.push(plane[i]);
                }
            }
            return Ok(true);
        }
    }
}

fn ticks_to_frames(tb: Option<TimeBase>, ticks: u64, rate: u32) -> u64 {
    match tb.and_then(|tb| tb.calc_duration(Duration::new(ticks))) {
        Some(t) => (t.as_secs_f64() * f64::from(rate)).round() as u64,
        None => ticks,
    }
}

/// A timestamp as a frame number (negative before the start of the file).
fn ts_to_frames(tb: Option<TimeBase>, ts: Timestamp, rate: u32) -> i64 {
    match tb {
        Some(tb) => {
            let magnitude = ts.get().unsigned_abs().min(MAX_TICKS);
            let (negative, ticks) = (ts.get() < 0, Timestamp::new(magnitude as i64));
            let frames =
                (tb.calc_time_saturating(ticks).as_secs_f64() * f64::from(rate)).round() as i64;
            if negative { -frames } else { frames }
        }
        None => ts.get(),
    }
}

fn frames_to_ts(tb: Option<TimeBase>, frame: u64, rate: u32) -> Timestamp {
    let secs = frame as f64 / f64::from(rate);
    match tb.and_then(|tb| Time::try_from_secs_f64(secs).and_then(|t| tb.calc_timestamp(t))) {
        Some(ts) => ts,
        None => Timestamp::new(i64::try_from(frame).unwrap_or(i64::MAX)),
    }
}

// ------------------------------------------------------------------------------------ whole file

/// A whole file in memory: planar `f32`, at the file's own sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// Sample rate of the data in Hz.
    pub sample_rate: u32,
    /// One vector per channel (1 or 2), all of the same length.
    pub channels: Vec<Vec<f32>>,
}

impl Decoded {
    /// Frames per channel.
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Length in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate)
    }

    /// The same audio at `rate` (see [`crate::resample`]); a clone when the rate is the same.
    pub fn resampled(&self, rate: u32) -> Result<Decoded, AudioError> {
        Ok(Decoded {
            sample_rate: rate,
            channels: crate::resample(&self.channels, self.sample_rate, rate)?,
        })
    }
}

/// Decodes the whole file at `path`. `max_seconds` refuses longer files before decoding them
/// ([`AudioError::TooLong`]).
pub fn decode_all(path: &Path, max_seconds: Option<f64>) -> Result<Decoded, AudioError> {
    let mut file = AudioFile::open(path)?;
    let info = *file.info();
    if max_seconds.is_some_and(|max| info.duration_secs() > max) {
        return Err(AudioError::TooLong);
    }
    if info.frames > MAX_DECODE_FRAMES {
        return Err(AudioError::TooLong);
    }
    // The header's length is a hint, not a promise: reserve a bounded amount and grow.
    let cap = usize::try_from(info.frames)
        .unwrap_or(0)
        .min(INITIAL_FRAMES);
    let mut channels: Vec<Vec<f32>> = (0..info.channels)
        .map(|_| Vec::with_capacity(cap))
        .collect();
    let mut chunk = vec![0.0f32; CHUNK_FRAMES * info.channels];
    loop {
        let n = file.read_full(&mut chunk)?;
        if n == 0 {
            break;
        }
        for frame in chunk[..n * info.channels].chunks_exact(info.channels) {
            for (plane, v) in channels.iter_mut().zip(frame) {
                plane.push(*v);
            }
        }
        // A container that understates its length must not grow without bound.
        if channels[0].len() as u64 > MAX_DECODE_FRAMES {
            return Err(AudioError::TooLong);
        }
        if max_seconds
            .is_some_and(|max| channels[0].len() as f64 > max * f64::from(info.sample_rate))
        {
            return Err(AudioError::TooLong);
        }
    }
    if channels[0].is_empty() {
        return Err(AudioError::Empty);
    }
    Ok(Decoded {
        sample_rate: info.sample_rate,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_timestamps_do_not_overflow() {
        let tb = TimeBase::try_new(1, 44100);
        for ts in [i64::MIN, i64::MIN + 1, -1, 0, i64::MAX] {
            let _ = ts_to_frames(tb, Timestamp::new(ts), 44100);
            let _ = ts_to_frames(None, Timestamp::new(ts), 44100);
        }
        for ticks in [0, 1, u64::MAX] {
            let _ = ticks_to_frames(tb, ticks, 44100);
            let _ = ticks_to_frames(None, ticks, 44100);
        }
        for frame in [0, u64::MAX] {
            let _ = frames_to_ts(tb, frame, 44100);
            let _ = frames_to_ts(None, frame, 44100);
        }
    }
}
