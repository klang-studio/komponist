//! The on-disk peak cache: one small file per audio file in a cache folder the caller owns
//! (the user's cache directory, never the repository or the project folder).
//!
//! The file name is a stable hash of the audio file's identity ([`crate::FileId`]: canonical
//! path, size, modification time); the header repeats size and modification time, so a file
//! that changed under the same name never reads stale peaks, and a damaged or foreign file
//! reads as a miss. All of it is best effort: every error is a miss on read and ignored on write.
//!
//! Layout (little-endian): `"KPK1"`, sample rate `u32`, frames `u64`, file size `u64`, mtime
//! nanoseconds `u64`, channels `u32`, level count `u32`, then per level the block size `u64`,
//! the bucket count `u64`, and `channels × buckets` min/max `f32` pairs.

use std::path::{Path, PathBuf};

use super::{Level, Peak, PeakData};
use crate::FileId;

const MAGIC: &[u8; 4] = b"KPK1";
const HEADER: usize = 4 + 4 + 8 + 8 + 8 + 4 + 4;

/// The cache file of `id` inside `dir`.
pub fn path_for(dir: &Path, id: &FileId) -> PathBuf {
    dir.join(format!("{:016x}.kpeaks", id.stable_hash()))
}

/// Serialises `data` for the file `id`.
pub fn to_bytes(data: &PeakData, id: &FileId) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + data.byte_size() + 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&data.sample_rate.to_le_bytes());
    out.extend_from_slice(&data.frames.to_le_bytes());
    out.extend_from_slice(&id.len.to_le_bytes());
    out.extend_from_slice(&id.mtime_ns.to_le_bytes());
    out.extend_from_slice(&(data.channels() as u32).to_le_bytes());
    out.extend_from_slice(&(data.levels.len() as u32).to_le_bytes());
    for level in &data.levels {
        out.extend_from_slice(&level.block.to_le_bytes());
        out.extend_from_slice(&(level.data[0].len() as u64).to_le_bytes());
        for channel in &level.data {
            for p in channel {
                out.extend_from_slice(&p.min.to_le_bytes());
                out.extend_from_slice(&p.max.to_le_bytes());
            }
        }
    }
    out
}

/// Parses what [`to_bytes`] wrote; `None` for anything else or for peaks of another version of the file.
pub fn from_bytes(bytes: &[u8], id: &FileId) -> Option<PeakData> {
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let sample_rate = r.u32()?;
    let frames = r.u64()?;
    if r.u64()? != id.len || r.u64()? != id.mtime_ns {
        return None;
    }
    let channels = r.u32()? as usize;
    let level_count = r.u32()? as usize;
    if channels == 0 || channels > crate::MAX_CHANNELS || level_count == 0 || level_count > 64 {
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
        if buckets.checked_mul(channels)?.checked_mul(8)? > r.0.len() {
            return None;
        }
        let mut data = Vec::with_capacity(channels);
        for _ in 0..channels {
            let mut c = Vec::with_capacity(buckets);
            for _ in 0..buckets {
                c.push(Peak {
                    min: r.f32()?,
                    max: r.f32()?,
                });
            }
            data.push(c);
        }
        levels.push(Level { block, data });
    }
    r.0.is_empty().then_some(PeakData {
        sample_rate,
        frames,
        levels,
    })
}

/// Reads the cached peaks of `id` from `dir`.
pub fn read(dir: &Path, id: &FileId) -> Option<PeakData> {
    from_bytes(&std::fs::read(path_for(dir, id)).ok()?, id)
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
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}
