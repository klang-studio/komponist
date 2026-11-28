//! The on-disk peak cache: one small file per audio file in a cache folder the caller owns
//! (the user's cache directory, never the repository or the project folder).
//!
//! The file name is a stable hash of the audio file's identity ([`crate::FileId`]: canonical
//! path, size, modification time); the header repeats size and modification time, so a file
//! that changed under the same name never reads stale peaks, and a damaged or foreign file
//! reads as a miss. All of it is best effort: every error is a miss on read and ignored on write.
//!
//! Layout (little-endian): `"KPK3"`, sample rate `u32`, frames `u64`, file size `u64`, signed mtime
//! nanoseconds `i128` (minimum for missing), channels `u32`, level count `u32`, then per level the block size `u64`,
//! the bucket count `u64`, and `channels × buckets` min/max `f32` pairs and sum-of-squares `f64` values.
//! Legacy KPK2 records used an unsigned mtime and still work for unambiguous legacy identities.

use std::path::{Path, PathBuf};

use super::{Level, Peak, PeakData};
use crate::FileId;

const MAGIC: &[u8; 4] = b"KPK3";
const LEGACY_MAGIC: &[u8; 4] = b"KPK2";
const HEADER: usize = 4 + 4 + 8 + 8 + 16 + 4 + 4;

/// The cache file of `id` inside `dir`.
pub fn path_for(dir: &Path, id: &FileId) -> PathBuf {
    dir.join(format!("v3-{:016x}.kpeaks", id.stable_hash()))
}

/// Serialises `data` for the file `id`.
pub fn to_bytes(data: &PeakData, id: &FileId) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + data.byte_size() + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&data.sample_rate.to_le_bytes());
    out.extend_from_slice(&data.frames.to_le_bytes());
    out.extend_from_slice(&id.len.to_le_bytes());
    out.extend_from_slice(&id.mtime_bytes());
    out.extend_from_slice(&(data.channels() as u32).to_le_bytes());
    out.extend_from_slice(&(data.levels.len() as u32).to_le_bytes());
    for level in &data.levels {
        out.extend_from_slice(&level.block.to_le_bytes());
        out.extend_from_slice(&(level.data[0].len() as u64).to_le_bytes());
        for (channel, energy) in level.data.iter().zip(&level.energy) {
            for (p, energy) in channel.iter().zip(energy) {
                out.extend_from_slice(&p.min.to_le_bytes());
                out.extend_from_slice(&p.max.to_le_bytes());
                out.extend_from_slice(&energy.to_le_bytes());
            }
        }
    }
    out
}

/// Parses what [`to_bytes`] wrote; `None` for anything else or for peaks of another version of the file.
pub fn from_bytes(bytes: &[u8], id: &FileId) -> Option<PeakData> {
    let mut r = Reader(bytes);
    let magic = r.take(4)?;
    let sample_rate = r.u32()?;
    let frames = r.u64()?;
    if r.u64()? != id.len {
        return None;
    }
    let matches = match magic {
        current if current == MAGIC => r.take(16)? == id.mtime_bytes(),
        legacy if legacy == LEGACY_MAGIC => r.u64()? == id.legacy_peak_key()?.1,
        _ => false,
    };
    if !matches {
        return None;
    }
    let channels = r.u32()? as usize;
    let level_count = r.u32()? as usize;
    // The decoders refuse other rates, and the level count follows from the frame count: a level
    // per factor of `FACTOR` down to a single bucket, as `PeakBuilder::finish` builds them.
    if !(crate::decode::MIN_SAMPLE_RATE..=crate::decode::MAX_SAMPLE_RATE).contains(&sample_rate)
        || channels == 0
        || channels > crate::MAX_CHANNELS
        || level_count == 0
        || level_count > 64
        || Some(level_count) != levels_for(frames)
    {
        return None;
    }
    let mut levels = Vec::with_capacity(level_count);
    for i in 0..level_count {
        let block = r.u64()?;
        let buckets = usize::try_from(r.u64()?).ok()?;
        // The shape is fixed by the frame count: anything else is not what `to_bytes` wrote.
        let expected = PeakData::FACTOR
            .checked_pow(i as u32)
            .and_then(|f| f.checked_mul(PeakData::BASE_BLOCK))?;
        if block != expected || buckets as u64 != frames.div_ceil(block) {
            return None;
        }
        // The remaining bytes must hold the buckets: no huge allocation from a corrupt count.
        if buckets.checked_mul(channels)?.checked_mul(16)? > r.0.len() {
            return None;
        }
        let mut data = Vec::with_capacity(channels);
        let mut energy = Vec::with_capacity(channels);
        for _ in 0..channels {
            let mut c = Vec::with_capacity(buckets);
            let mut e = Vec::with_capacity(buckets);
            for bucket in 0..buckets {
                let peak = Peak {
                    min: r.f32()?,
                    max: r.f32()?,
                };
                // The decoders deliver finite samples, so every bucket holds a finite range.
                if !(peak.min.is_finite() && peak.max.is_finite() && peak.min <= peak.max) {
                    return None;
                }
                c.push(peak);
                let value = r.f64()?;
                let count = frames.saturating_sub(bucket as u64 * block).min(block);
                let limit = f64::from(f32::MAX).powi(2) * count as f64;
                if !value.is_finite() || value < 0.0 || value > limit {
                    return None;
                }
                e.push(value);
            }
            data.push(c);
            energy.push(e);
        }
        levels.push(Level {
            block,
            data,
            energy,
        });
    }
    r.0.is_empty().then_some(PeakData {
        sample_rate,
        frames,
        levels,
    })
}

/// The number of levels [`PeakBuilder`](super::PeakBuilder) builds for `frames` frames; `None`
/// for none (an empty file has no cache record).
fn levels_for(frames: u64) -> Option<usize> {
    let mut buckets = frames.div_ceil(PeakData::BASE_BLOCK);
    if buckets == 0 {
        return None;
    }
    let mut levels = 1;
    while buckets > 1 {
        buckets = buckets.div_ceil(PeakData::FACTOR);
        levels += 1;
    }
    Some(levels)
}

/// Reads the cached peaks of `id` from `dir`.
pub fn read(dir: &Path, id: &FileId) -> Option<PeakData> {
    if let Some(data) = std::fs::read(path_for(dir, id))
        .ok()
        .and_then(|bytes| from_bytes(&bytes, id))
    {
        return Some(data);
    }
    let (key, _) = id.legacy_peak_key()?;
    let bytes = std::fs::read(dir.join(format!("{key:016x}.kpeaks"))).ok()?;
    from_bytes(&bytes, id)
}

/// Writes the peaks of `id` into `dir` (created if needed), through a temporary file and a rename.
/// Errors are returned for the caller to ignore.
pub fn write(dir: &Path, id: &FileId, data: &PeakData) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = path_for(dir, id);
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, to_bytes(data, id))?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_legacy_records_only_for_unambiguous_keys() {
        let id = FileId {
            path: PathBuf::from("/audio.wav"),
            len: 818,
            mtime_ns: Some(300),
        };
        let data = PeakData::from_planar(44100, &[vec![0.125; 500]]);
        let mut legacy = to_bytes(&data, &id);
        legacy[..4].copy_from_slice(LEGACY_MAGIC);
        legacy.splice(24..40, 300u64.to_le_bytes());
        assert_eq!(from_bytes(&legacy, &id), Some(data));
        assert!(id.legacy_peak_key().is_some());
        for suspect in [
            FileId {
                path: PathBuf::from("/audio-�.wav"),
                ..id.clone()
            },
            FileId {
                mtime_ns: None,
                ..id.clone()
            },
            FileId {
                mtime_ns: Some(0),
                ..id.clone()
            },
            FileId {
                mtime_ns: Some(-300),
                ..id.clone()
            },
            FileId {
                mtime_ns: Some(i128::from(u64::MAX) + 1),
                ..id.clone()
            },
        ] {
            assert!(suspect.legacy_peak_key().is_none());
            assert!(from_bytes(&legacy, &suspect).is_none());
        }
    }
}
