//! The error of every operation in this crate.

use std::fmt;

/// Why an audio file could not be read. The texts are English and technical; the UI maps the
/// variants to its own catalog texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// The file could not be opened or read (the OS message).
    Io(String),
    /// Not a format this crate decodes (WAV, FLAC, OGG Vorbis, MP3), or a variant of one it does not.
    Unsupported(String),
    /// The file is damaged or ends in the middle of the data.
    Malformed(String),
    /// The file has no audio frames.
    Empty,
    /// The file is longer than the caller's limit.
    TooLong,
    /// Sample-rate conversion failed (a rate of 0, or the converter refused the ratio).
    Resample(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::Io(e) => write!(f, "cannot read the file: {e}"),
            AudioError::Unsupported(e) => write!(f, "unsupported audio format: {e}"),
            AudioError::Malformed(e) => write!(f, "damaged audio file: {e}"),
            AudioError::Empty => f.write_str("the file has no audio"),
            AudioError::TooLong => f.write_str("the file is longer than the limit"),
            AudioError::Resample(e) => write!(f, "sample-rate conversion failed: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        AudioError::Io(e.to_string())
    }
}
