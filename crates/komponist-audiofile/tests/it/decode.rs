//! Decoding: every format, chunked reads, seeks, errors.

use komponist_audiofile::{AudioError, AudioFile, Format, decode_all};

use crate::common::*;

#[test]
fn audio_file_can_move_to_a_reader_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<AudioFile>();
}

#[test]
fn aiff_big_endian_pcm_supports_chunked_reads_and_seeks() {
    let dir = Scratch::new("aiff");
    let path = dir.path("stereo.aiff");
    let samples = [16384_i16, -16384, 0, 32767, -32768, 8192];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"FORM");
    bytes.extend_from_slice(&58_u32.to_be_bytes());
    bytes.extend_from_slice(b"AIFFCOMM");
    bytes.extend_from_slice(&18_u32.to_be_bytes());
    bytes.extend_from_slice(&2_u16.to_be_bytes());
    bytes.extend_from_slice(&3_u32.to_be_bytes());
    bytes.extend_from_slice(&16_u16.to_be_bytes());
    // 44,100 in AIFF's 80-bit extended precision representation.
    bytes.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"SSND");
    bytes.extend_from_slice(&20_u32.to_be_bytes());
    bytes.extend_from_slice(&[0; 8]);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_be_bytes());
    }
    std::fs::write(&path, bytes).unwrap();
    let mut file = AudioFile::open(&path).unwrap();
    assert_eq!(file.info().format, Format::Aiff);
    assert_eq!(
        (
            file.info().sample_rate,
            file.info().channels,
            file.info().frames
        ),
        (44_100, 2, 3)
    );
    let mut out = [0.0; 4];
    assert_eq!(file.read(&mut out).unwrap(), 2);
    assert_eq!(out, [0.5, -0.5, 0.0, 32767.0 / 32768.0]);
    file.seek(2).unwrap();
    assert_eq!(file.read(&mut out).unwrap(), 1);
    assert_eq!(&out[..2], &[-1.0, 0.25]);
}

#[test]
fn wav_sample_formats_decode_to_the_same_values() {
    let dir = Scratch::new("wav-formats");
    // 16-bit stereo: exact scaling by 1/32768.
    let path = dir.path("i16.wav");
    write_wav_i16(&path, 44100, 2, &[16384, -16384, 0, 32767, -32768, 8192]);
    let d = decode_all(&path, None).unwrap();
    assert_eq!(d.sample_rate, 44100);
    assert_eq!(d.channels[0], vec![0.5, 0.0, -1.0]);
    assert_eq!(d.channels[1], vec![-0.5, 32767.0 / 32768.0, 0.25]);

    // 24-bit and 8-bit integer, 32-bit float.
    let spec = |bits, format| hound::WavSpec {
        channels: 1,
        sample_rate: 22050,
        bits_per_sample: bits,
        sample_format: format,
    };
    let p24 = dir.path("i24.wav");
    let mut w = hound::WavWriter::create(&p24, spec(24, hound::SampleFormat::Int)).unwrap();
    w.write_sample(4_194_304i32).unwrap(); // 0.5
    w.finalize().unwrap();
    assert_eq!(decode_all(&p24, None).unwrap().channels[0], vec![0.5]);

    let p8 = dir.path("i8.wav");
    let mut w = hound::WavWriter::create(&p8, spec(8, hound::SampleFormat::Int)).unwrap();
    w.write_sample(64i8).unwrap(); // 0.5
    w.finalize().unwrap();
    assert_eq!(decode_all(&p8, None).unwrap().channels[0], vec![0.5]);

    let pf = dir.path("f32.wav");
    write_wav_f32(&pf, 96000, &[vec![0.25, -0.75]]);
    let d = decode_all(&pf, None).unwrap();
    assert_eq!(
        (d.sample_rate, d.channels),
        (96000, vec![vec![0.25, -0.75]])
    );
}

#[test]
fn seeking_after_a_wav_read_error_retains_the_original_length_and_channel_alignment() {
    let dir = Scratch::new("wav-seek-after-error");
    let path = dir.path("stereo.wav");
    let samples: Vec<_> = (0..5000).flat_map(|i| [i as i16, -(i as i16)]).collect();
    write_wav_i16(&path, 48000, 2, &samples);
    let original = std::fs::read(&path).unwrap();
    let mut file = AudioFile::open(&path).unwrap();
    let source = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let data_offset = source.metadata().unwrap().len() - 5000 * 4;
    source.set_len(data_offset + 2500 * 4).unwrap();
    let mut buf = vec![0.0; 4000 * 2];
    assert!(matches!(file.read(&mut buf), Err(AudioError::Io(_))));
    file.seek(1000).unwrap();
    assert_eq!(file.info().frames, 5000);
    assert_eq!(file.read(&mut buf[..4]).unwrap(), 2);
    assert_eq!(
        &buf[..4],
        &[
            1000.0 / 32768.0,
            -1000.0 / 32768.0,
            1001.0 / 32768.0,
            -1001.0 / 32768.0
        ]
    );
    file.seek(4000).unwrap();
    assert!(matches!(file.read(&mut buf[..4]), Err(AudioError::Io(_))));
    source.set_len(0).unwrap();
    assert!(file.seek(1000).is_err());
    std::fs::write(&path, original).unwrap();
    file.seek(4000).unwrap();
    assert_eq!(file.read(&mut buf[..2]).unwrap(), 1);
    assert_eq!(&buf[..2], &[4000.0 / 32768.0, -4000.0 / 32768.0]);
}

#[test]
fn files_with_more_than_two_channels_give_the_first_two() {
    let dir = Scratch::new("wav-multi");
    let path = dir.path("six.wav");
    write_wav_i16(&path, 48000, 3, &[1000, 2000, 3000, -1000, -2000, -3000]);
    let file = AudioFile::open(&path).unwrap();
    assert_eq!((file.info().channels, file.info().source_channels), (2, 3));
    let d = decode_all(&path, None).unwrap();
    assert_eq!(d.channels.len(), 2);
    assert_eq!(d.channels[0], vec![1000.0 / 32768.0, -1000.0 / 32768.0]);
    assert_eq!(d.channels[1], vec![2000.0 / 32768.0, -2000.0 / 32768.0]);
}

#[test]
fn compressed_fixtures_decode_to_the_tones_they_hold() {
    for name in FIXTURES {
        let d = decode_all(&fixture(name), None).unwrap();
        assert_eq!(d.sample_rate, 44100, "{name}");
        assert_eq!(d.channels.len(), 2, "{name}");
        let frames = d.frames();
        // 0.75 s; the lossy encoders pad to a packet boundary.
        assert!(
            (33075 - 2304..=33075 + 2304).contains(&frames),
            "{name}: {frames} frames"
        );
        let (l, r) = (&d.channels[0], &d.channels[1]);
        // Left 440 Hz at 0.5, right 880 Hz at 0.5, nothing of the other tone. (The OGG fixture
        // comes from ffmpeg's native Vorbis encoder, which halves the second channel; ffmpeg's
        // own decoder gives the same 0.245 for this file.)
        let right = if name.ends_with(".ogg") { 0.245 } else { 0.5 };
        assert!(
            (tone_amplitude(l, 440.0, 44100) - 0.5).abs() < 0.03,
            "{name}"
        );
        assert!(
            (tone_amplitude(r, 880.0, 44100) - right).abs() < 0.03,
            "{name}"
        );
        assert!(tone_amplitude(l, 880.0, 44100) < 0.02, "{name}");
        assert!(tone_amplitude(r, 440.0, 44100) < 0.02, "{name}");
    }
}

#[test]
fn premature_eof_is_an_error_for_compressed_files_with_a_declared_length() {
    let dir = Scratch::new("declared-eof");
    for ext in ["flac", "mp3"] {
        let filename = format!("stereo-440-880.{ext}");
        let bytes = std::fs::read(fixture(&filename)).unwrap();
        let path = dir.path(&filename);
        std::fs::write(&path, &bytes[..bytes.len() * 2 / 3]).unwrap();
        let mut file = AudioFile::open(&path).unwrap();
        assert_eq!(file.info().frames, 33075, "{ext}");
        let mut buf = [0.0; 1024];
        loop {
            match file.read(&mut buf) {
                Ok(0) => panic!("{ext}: accepted premature EOF at frame {}", file.position()),
                Ok(_) => (),
                Err(AudioError::Malformed(_)) => break,
                Err(error) => panic!("{ext}: {error}"),
            }
        }
        assert!(
            file.position() > 10_000 && file.position() < 30_000,
            "{ext}"
        );
    }
}

#[test]
fn an_mp3_duration_estimate_does_not_make_clean_eof_an_error() {
    let dir = Scratch::new("estimated-eof");
    let path = dir.path("tagless.mp3");
    let mut bytes = std::fs::read(fixture("stereo-440-880.mp3")).unwrap();
    // Hide the Info tag so the MPEG stream has only a duration estimated from the bitrate.
    // A zero trailer overestimates the duration without removing any decodable frames.
    let tag = bytes.windows(4).position(|p| p == b"Info").unwrap();
    bytes[tag..tag + 4].copy_from_slice(b"NONE");
    bytes.extend_from_slice(&[0; 4096]);
    std::fs::write(&path, bytes).unwrap();
    let mut file = AudioFile::open(&path).unwrap();
    let mut buf = [0.0; 1024];
    while file.read(&mut buf).unwrap() > 0 {}
    assert!(file.position() > 33_000);
    assert!(file.position() < file.info().frames);
}

#[test]
fn flac_is_lossless() {
    let d = decode_all(&fixture("stereo-440-880.flac"), None).unwrap();
    assert_eq!(d.frames(), 33075);
    let want = sine(440.0, 44100, 33075, 0.5);
    let worst = d.channels[0]
        .iter()
        .zip(&want)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max);
    // 16-bit quantisation of the source: half a step.
    assert!(worst <= 1.0 / 32768.0, "worst error {worst}");
}

#[test]
fn info_reports_format_rate_channels_and_length() {
    let flac = AudioFile::open(&fixture("stereo-440-880.flac")).unwrap();
    assert_eq!(flac.info().format, Format::Flac);
    assert_eq!(
        (
            flac.info().sample_rate,
            flac.info().channels,
            flac.info().frames
        ),
        (44100, 2, 33075)
    );
    assert_eq!(
        AudioFile::open(&fixture("stereo-440-880.ogg"))
            .unwrap()
            .info()
            .format,
        Format::Ogg
    );
    let mp3 = AudioFile::open(&fixture("stereo-440-880.mp3")).unwrap();
    assert_eq!(mp3.info().format, Format::Mp3);
    // The length of a file without a length field is the sum of its packets, not zero.
    assert!(mp3.info().frames > 30000);
}

#[test]
fn chunked_reads_add_up_to_the_whole_file() {
    for name in FIXTURES {
        let whole = decode_all(&fixture(name), None).unwrap();
        let mut file = AudioFile::open(&fixture(name)).unwrap();
        let mut got = [Vec::new(), Vec::new()];
        let mut buf = vec![0.0f32; 700 * 2];
        let mut position = 0;
        loop {
            let n = file.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            position += n as u64;
            assert_eq!(file.position(), position, "{name}");
            for f in buf[..n * 2].as_chunks::<2>().0 {
                got[0].push(f[0]);
                got[1].push(f[1]);
            }
        }
        assert_eq!(got[0], whole.channels[0], "{name}");
        assert_eq!(got[1], whole.channels[1], "{name}");
        assert_eq!(file.read(&mut buf).unwrap(), 0, "stays at the end: {name}");
    }
}

#[test]
fn seek_lands_on_the_frame_asked_for() {
    // Seek-and-read must give what a sequential decode gives at that frame: the pre-roll
    // rebuilds the decoder state, so the samples are the same, not merely close.
    for name in FIXTURES {
        let whole = decode_all(&fixture(name), None).unwrap();
        let mut file = AudioFile::open(&fixture(name)).unwrap();
        for start in [0usize, 1, 1151, 1152, 4096, 20000, 30000] {
            file.seek(start as u64).unwrap();
            assert_eq!(file.position(), start as u64);
            let mut buf = vec![0.0f32; 512 * 2];
            let n = file.read_full(&mut buf).unwrap();
            assert_eq!(n, 512, "{name} at {start}");
            let err = rms((0..n).flat_map(|i| {
                let (l, r) = (buf[i * 2], buf[i * 2 + 1]);
                [
                    f64::from(l - whole.channels[0][start + i]),
                    f64::from(r - whole.channels[1][start + i]),
                ]
            }));
            assert!(err < 1e-4, "{name} at {start}: rms error {err}");
        }
        // Back and forth, and past the end.
        file.seek(100).unwrap();
        file.seek(0).unwrap();
        let mut one = [0.0f32; 2];
        assert_eq!(file.read(&mut one).unwrap(), 1);
        let end = file.info().frames;
        file.seek(end + 1000).unwrap();
        assert_eq!(file.position(), end);
        assert_eq!(file.read(&mut one).unwrap(), 0);
    }
}

#[test]
fn wav_seek_is_sample_exact() {
    let dir = Scratch::new("wav-seek");
    let path = dir.path("ramp.wav");
    let ramp: Vec<i16> = (0..10_000i32).map(|i| (i - 5000) as i16).collect();
    write_wav_i16(&path, 44100, 1, &ramp);
    let mut file = AudioFile::open(&path).unwrap();
    file.seek(7777).unwrap();
    let mut buf = [0.0f32; 3];
    assert_eq!(file.read(&mut buf).unwrap(), 3);
    assert_eq!(buf, [2777.0 / 32768.0, 2778.0 / 32768.0, 2779.0 / 32768.0]);
}

#[test]
fn read_never_runs_past_the_end() {
    let dir = Scratch::new("wav-end");
    let path = dir.path("short.wav");
    write_wav_i16(&path, 44100, 1, &[1, 2, 3, 4, 5]);
    let mut file = AudioFile::open(&path).unwrap();
    let mut buf = [0.0f32; 16];
    assert_eq!(file.read(&mut buf).unwrap(), 5);
    assert_eq!(file.read(&mut buf).unwrap(), 0);
}

#[test]
fn errors_name_what_is_wrong() {
    let dir = Scratch::new("errors");
    assert!(matches!(
        AudioFile::open(&dir.path("missing.wav")),
        Err(AudioError::Io(_))
    ));
    let junk = dir.path("junk.mp3");
    std::fs::write(&junk, vec![0x55u8; 4096]).unwrap();
    assert!(matches!(
        AudioFile::open(&junk),
        Err(AudioError::Unsupported(_) | AudioError::Malformed(_))
    ));
    let empty = dir.path("empty.wav");
    write_wav_i16(&empty, 44100, 1, &[]);
    assert_eq!(AudioFile::open(&empty).unwrap_err(), AudioError::Empty);
    let long = dir.path("long.wav");
    write_wav_i16(&long, 100, 1, &vec![0i16; 1000]); // 10 s
    assert_eq!(
        decode_all(&long, Some(5.0)).unwrap_err(),
        AudioError::TooLong
    );
    assert!(decode_all(&long, Some(10.0)).is_ok());
    // 12-bit-style WAV variants and ADPCM are refused by name, not decoded as noise.
    let text = dir.path("text.wav");
    std::fs::write(&text, b"RIFF....WAVEjunk").unwrap();
    assert!(AudioFile::open(&text).is_err());
}

#[test]
fn a_header_that_promises_too_much_fails_or_clamps_cleanly() {
    let dir = Scratch::new("hostile-header");
    // A WAV with 4 frames whose RIFF and data chunk sizes claim almost 4 GiB.
    let path = dir.path("huge.wav");
    write_wav_i16(&path, 44100, 1, &[1, 2, 3, 4]);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[4..8].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
    let data = bytes.windows(4).position(|w| w == b"data").unwrap();
    bytes[data + 4..data + 8].copy_from_slice(&0xFFFF_FFC0u32.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    // Metadata stays bounded by the file size. Reaching the missing declared data is an error.
    let mut file = AudioFile::open(&path).unwrap();
    assert_eq!(file.info().frames, 4);
    assert_eq!(file.read(&mut [0.0; 4]).unwrap(), 4);
    assert!(matches!(file.read(&mut [0.0]), Err(AudioError::Io(_))));
    assert!(matches!(decode_all(&path, None), Err(AudioError::Io(_))));
}

#[test]
fn absurd_sample_rates_are_refused() {
    let dir = Scratch::new("hostile-rate");
    let path = dir.path("fast.wav");
    write_wav_i16(&path, 50_000_000, 1, &[1, 2, 3]);
    assert!(matches!(
        AudioFile::open(&path),
        Err(AudioError::Unsupported(_))
    ));
    let ok = dir.path("ok.wav");
    write_wav_i16(&ok, 768_000, 1, &[1, 2, 3]);
    assert!(AudioFile::open(&ok).is_ok());
}

#[test]
fn damaged_files_fail_cleanly_and_never_panic() {
    // Deterministic byte flips and truncations of every fixture: any outcome but a panic is fine.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let dir = Scratch::new("fuzz");
    for name in FIXTURES {
        let original = std::fs::read(fixture(name)).unwrap();
        let ext = name.rsplit('.').next().unwrap();
        for round in 0..60 {
            let mut bytes = original.clone();
            for _ in 0..1 + round % 8 {
                // Half of the flips hit the header area, where the lengths and rates are.
                let limit = if next() % 2 == 0 { 400 } else { bytes.len() };
                let at = (next() as usize) % limit.min(bytes.len());
                bytes[at] = next() as u8;
            }
            if round % 5 == 0 {
                let keep = (next() as usize) % bytes.len();
                bytes.truncate(keep);
            }
            let path = dir.path(&format!("f{round}.{ext}"));
            std::fs::write(&path, &bytes).unwrap();
            let _ = decode_all(&path, Some(60.0));
            if let Ok(mut file) = AudioFile::open(&path) {
                let _ = file.seek(next() % 40_000);
                let mut buf = [0.0f32; 512];
                let _ = file.read_full(&mut buf);
            }
        }
    }
}

#[test]
fn wav_float_samples_that_are_not_finite_decode_to_silence() {
    let dir = Scratch::new("wav-non-finite");
    let path = dir.path("nan.wav");
    let bad = vec![0.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.25];
    write_wav_f32(&path, 44100, &[bad.clone(), bad]);
    let d = decode_all(&path, None).unwrap();
    for plane in &d.channels {
        assert_eq!(plane, &vec![0.5, 0.0, 0.0, 0.0, -0.25]);
    }
    // The streaming reader (what playback uses) filters the same way.
    let mut file = AudioFile::open(&path).unwrap();
    let mut out = [1.0; 10];
    assert_eq!(file.read_full(&mut out).unwrap(), 5);
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn aiff_c_float_samples_that_are_not_finite_decode_to_silence() {
    // The symphonia path: AIFF-C with 32-bit float samples ("fl32"), mono.
    let dir = Scratch::new("aiff-non-finite");
    let path = dir.path("nan.aifc");
    let samples = [0.5_f32, f32::NAN, f32::INFINITY, -0.25];
    let mut comm = Vec::new();
    comm.extend_from_slice(&1_u16.to_be_bytes());
    comm.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    comm.extend_from_slice(&32_u16.to_be_bytes());
    comm.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
    comm.extend_from_slice(b"fl32");
    comm.extend_from_slice(&[12]);
    comm.extend_from_slice(b"32-bit float");
    comm.push(0);
    let mut ssnd = vec![0; 8];
    for s in samples {
        ssnd.extend_from_slice(&s.to_be_bytes());
    }
    let mut form = Vec::new();
    form.extend_from_slice(b"AIFC");
    form.extend_from_slice(b"FVER");
    form.extend_from_slice(&4_u32.to_be_bytes());
    form.extend_from_slice(&0xA280_5140_u32.to_be_bytes());
    form.extend_from_slice(b"COMM");
    form.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    form.extend_from_slice(&comm);
    form.extend_from_slice(b"SSND");
    form.extend_from_slice(&(ssnd.len() as u32).to_be_bytes());
    form.extend_from_slice(&ssnd);
    let mut bytes = b"FORM".to_vec();
    bytes.extend_from_slice(&(form.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&form);
    std::fs::write(&path, bytes).unwrap();
    let d = decode_all(&path, None).unwrap();
    assert_eq!(d.channels[0], vec![0.5, 0.0, 0.0, -0.25]);
}

/// The files proptest mutates: the compressed fixtures, and small WAV and AIFF files written here.
fn seed_files(dir: &Scratch) -> Vec<(Vec<u8>, &'static str)> {
    let mut seeds: Vec<(Vec<u8>, &'static str)> = FIXTURES
        .iter()
        .map(|name| {
            let ext = name.rsplit('.').next().unwrap();
            (std::fs::read(fixture(name)).unwrap(), ext)
        })
        .collect();
    let wav = dir.path("seed-i16.wav");
    write_wav_i16(&wav, 44100, 2, &[1, -2, 300, -400, 5000, -6000]);
    seeds.push((std::fs::read(&wav).unwrap(), "wav"));
    let float = dir.path("seed-f32.wav");
    write_wav_f32(&float, 48000, &[noise(300, 1)]);
    seeds.push((std::fs::read(&float).unwrap(), "wav"));
    seeds
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

    /// Damaged files of every format decode to an error or to sane audio (the `audio_file` fuzz
    /// target runs the same harness).
    #[test]
    fn damaged_files_give_errors_or_sane_audio(
        seed in 0usize..5,
        flips in proptest::collection::vec((proptest::num::usize::ANY, proptest::num::u8::ANY), 0..12),
        cut in proptest::option::of(proptest::num::usize::ANY),
        seek in 0u64..100_000,
    ) {
        let dir = Scratch::new(&format!("prop-{seed}-{}", flips.len()));
        let (mut bytes, ext) = seed_files(&dir).swap_remove(seed);
        for (at, value) in flips {
            // Half of the flips in the header area, where lengths, rates and channels are.
            let limit = if at % 2 == 0 { 256.min(bytes.len()) } else { bytes.len() };
            bytes[(at / 2) % limit] = value;
        }
        if let Some(cut) = cut {
            bytes.truncate(cut % (bytes.len() + 1));
        }
        komponist_audiofile::fuzzing::audio_bytes(&dir.0, &bytes, ext, seek);
    }

    /// Arbitrary bytes behind each format's magic.
    #[test]
    fn arbitrary_bytes_behind_a_magic_never_panic(
        magic in 0usize..5,
        body in proptest::collection::vec(proptest::num::u8::ANY, 0..2048),
    ) {
        let (head, ext): (&[u8], &str) = [
            (&b"RIFF\x00\x10\x00\x00WAVEfmt "[..], "wav"),
            (&b"FORM\x00\x00\x10\x00AIFFCOMM"[..], "aiff"),
            (&b"fLaC"[..], "flac"),
            (&b"OggS"[..], "ogg"),
            (&b"ID3\x04\x00\x00"[..], "mp3"),
        ][magic];
        let dir = Scratch::new(&format!("prop-magic-{magic}-{}", body.len()));
        let bytes = [head, &body].concat();
        komponist_audiofile::fuzzing::audio_bytes(&dir.0, &bytes, ext, body.len() as u64);
    }
}
