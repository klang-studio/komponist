//! Harnesses for hostile input (#230): the fuzz targets in `fuzz/` and the proptests of this crate
//! run the same checks. Not an API: each function panics when it finds a broken invariant, and
//! any other outcome (an error included) is a pass.

use std::path::{Path, PathBuf};

use crate::decode::MAX_SAMPLE_RATE;
use crate::peaks::cache;
use crate::{AudioFile, FileId, MAX_CHANNELS, PeakData, StreamResampler, decode_all};

/// Longest file a harness decodes as a whole, in seconds: hostile headers can make few bytes
/// expand to a lot of audio (`decode_all` stops there with `TooLong`).
const MAX_SECONDS: f64 = 2.0;

/// Most frames a harness reads through [`AudioFile::read`] before it stops.
const MAX_READ_FRAMES: usize = 1 << 18;

/// Opens the audio file at `path` the ways the app does (streaming reads, a seek, a whole-file
/// decode, peaks) and checks what comes back. `seek` is where the streaming read seeks to.
pub fn audio_file(path: &Path, seek: u64) {
    if let Ok(mut file) = AudioFile::open(path) {
        let info = *file.info();
        assert!(
            (1..=MAX_SAMPLE_RATE).contains(&info.sample_rate),
            "{info:?}"
        );
        assert!((1..=MAX_CHANNELS).contains(&info.channels), "{info:?}");
        assert!(info.source_channels >= info.channels, "{info:?}");
        assert!(info.frames > 0, "{info:?}");
        let mut chunk = vec![0.0f32; 4096 * info.channels + 1];
        let mut read = 0;
        while read < MAX_READ_FRAMES {
            match file.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    assert!(n * info.channels < chunk.len(), "{n} frames do not fit");
                    assert_finite(&chunk[..n * info.channels]);
                    read += n;
                }
            }
        }
        if file.seek(seek).is_ok() {
            let mut frame = vec![0.0f32; info.channels];
            if let Ok(n) = file.read_full(&mut frame) {
                assert!(n <= 1);
                assert_finite(&frame);
            }
        }
    }
    if let Ok(decoded) = decode_all(path, Some(MAX_SECONDS)) {
        assert!((1..=MAX_SAMPLE_RATE).contains(&decoded.sample_rate));
        assert!((1..=MAX_CHANNELS).contains(&decoded.channels.len()));
        let frames = decoded.frames();
        assert!(frames > 0);
        for channel in &decoded.channels {
            assert_eq!(channel.len(), frames, "channels of one length");
            assert_finite(channel);
        }
        let peaks = PeakData::from_decoded(&decoded);
        assert_eq!(peaks.channels(), decoded.channels.len());
        assert_eq!(peaks.frames(), frames as u64);
    }
    if let Ok(mut file) = AudioFile::open(path)
        && let Ok(peaks) = PeakData::from_file(&mut file)
    {
        assert_eq!(peaks.channels(), file.info().channels);
    }
}

/// Writes `bytes` to a fresh file in `dir` with the extension `ext` (the decoders sniff the
/// content; symphonia takes the extension as a hint) and runs [`audio_file`] on it.
pub fn audio_bytes(dir: &Path, bytes: &[u8], ext: &str, seek: u64) {
    let path: PathBuf = dir.join(format!("input.{ext}"));
    std::fs::write(&path, bytes).expect("scratch file");
    audio_file(&path, seek);
}

/// Parses `bytes` as a peak cache record. The file identity comes from the record itself, so the
/// identity check passes and the parser behind it is reached. A record that parses must hold a
/// valid peak pyramid, answer queries with finite ranges, and serialise to the same bytes.
pub fn peak_cache(bytes: &[u8]) {
    let field = |at: usize, len: usize| bytes.get(at..at + len);
    let len = field(16, 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()));
    // A current record carries the mtime as i128, a legacy one as u64.
    let mtime_ns = if bytes.starts_with(b"KPK2") {
        field(24, 8).map(|b| i128::from(u64::from_le_bytes(b.try_into().unwrap())))
    } else {
        field(24, 16).map(|b| i128::from_le_bytes(b.try_into().unwrap()))
    };
    let id = FileId {
        path: PathBuf::from("/fuzz/input.wav"),
        len,
        mtime_ns: mtime_ns.filter(|&t| t != i128::MIN),
    };
    let Some(data) = cache::from_bytes(bytes, &id) else {
        return;
    };
    assert!((1..=MAX_SAMPLE_RATE).contains(&data.sample_rate()));
    assert!((1..=MAX_CHANNELS).contains(&data.channels()));
    assert!(data.frames() > 0);
    if bytes.starts_with(b"KPK3") {
        assert_eq!(cache::to_bytes(&data, &id), bytes, "a record round-trips");
    }
    let frames = data.frames();
    for channel in 0..=data.channels() {
        for (start, end, columns) in [
            (0, frames, 7),
            (frames / 3, frames.saturating_mul(2), 64),
            (0, u64::MAX, 3),
            (frames, frames + 1, 1),
        ] {
            let peaks = data.query(channel, start, end, columns);
            assert_eq!(peaks.columns.len(), columns);
            for p in peaks.columns {
                assert!(
                    p.min.is_finite() && p.max.is_finite() && p.min <= p.max,
                    "{p:?}"
                );
            }
            let rms = data.query_rms(channel, start, end, columns);
            assert_eq!(rms.columns.len(), columns);
            assert_finite(&rms.columns);
        }
    }
}

/// Engine rates a stream converts to.
const ENGINE_RATES: [u32; 5] = [44_100, 48_000, 88_200, 96_000, 192_000];

/// Drives a [`StreamResampler`] with the pushes encoded in `bytes`: a file rate, an engine rate,
/// a channel count, then chunks whose channels may differ in length (refused) and resets. The
/// output must stay aligned across channels and add up to `expected_frames`.
pub fn resampler(bytes: &[u8]) {
    let mut input = Cursor(bytes);
    let from =
        u32::from_le_bytes([input.next(), input.next(), input.next(), 0]) % (MAX_SAMPLE_RATE + 1);
    let to = ENGINE_RATES[usize::from(input.next()) % ENGINE_RATES.len()];
    let channels = usize::from(input.next() % 4);
    let Ok(mut converter) = StreamResampler::new(from, to, channels) else {
        assert!(
            from == 0 || channels == 0,
            "{from} Hz to {to} Hz, {channels} channels"
        );
        return;
    };
    let mut out = vec![Vec::new(); channels];
    let mut pushed = 0u64;
    let mut sample = 0u32;
    for _ in 0..16 {
        let op = input.next();
        if op == 0xff {
            converter.reset();
            out.iter_mut().for_each(Vec::clear);
            pushed = 0;
            continue;
        }
        let frames = usize::from(op) * 37;
        // The last channel one frame longer than the others.
        let uneven = channels > 1 && input.next() % 8 == 0;
        let chunk: Vec<Vec<f32>> = (0..channels)
            .map(|c| {
                let len = frames + usize::from(uneven && c == channels - 1);
                (0..len)
                    .map(|_| {
                        sample = sample.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (sample >> 8) as f32 / (1 << 24) as f32 - 0.5
                    })
                    .collect()
            })
            .collect();
        match converter.push(&chunk, &mut out) {
            Ok(()) => {
                assert!(!uneven, "unequal channels must be refused");
                pushed += frames as u64;
            }
            Err(e) => assert!(uneven, "a valid chunk was refused: {e}"),
        }
        if input.0.is_empty() {
            break;
        }
    }
    let expected = converter.expected_frames(pushed);
    converter.finish(&mut out).expect("finish");
    for channel in &out {
        assert_eq!(channel.len() as u64, expected, "{from} Hz to {to} Hz");
        assert_finite(channel);
    }
}

/// The bytes of a harness input, one at a time; zeros after the end.
struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn next(&mut self) -> u8 {
        let (&first, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        first
    }
}

fn assert_finite(samples: &[f32]) {
    assert!(samples.iter().all(|s| s.is_finite()), "non-finite output");
}
