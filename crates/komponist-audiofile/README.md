# komponist-audiofile

Provides audio decoding, sample-rate conversion, waveform peaks and source-tempo analysis for the
engine, Sampler and editors.

## Structure

- [src/decode.rs](src/decode.rs) — Reads metadata and decodes seekable audio chunks or whole files.
- [src/resample.rs](src/resample.rs) — Converts sample rates for in-memory and streaming sources.
- [src/pool.rs](src/pool.rs) — Shares file entries, derived data and background work.
- [src/peaks/](src/peaks/) — Builds and caches multiresolution waveform data.
- [src/tempo.rs](src/tempo.rs) — Estimates original tempo and confidence from source audio.

## How it works

WAV decoding uses hound. AIFF, FLAC, MP3 and Ogg Vorbis use Symphonia. The file pool keys entries
by canonical path and file metadata, so consumers can share results and file changes produce new
entries. Weak pool references allow entries to be freed when their last consumer releases them.
Decoding, resampling and analysis run outside the audio callback.
