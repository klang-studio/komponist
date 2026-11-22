//! Audio fixtures shared by the sampler tests and demo.

use std::io;
use std::path::Path;

/// Writes mono `samples` as a 16-bit WAV at `rate`.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let to_io = |e: hound::Error| io::Error::other(e.to_string());
    let mut writer = hound::WavWriter::create(path, spec).map_err(to_io)?;
    for s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
            .map_err(to_io)?;
    }
    writer.finalize().map_err(to_io)
}
