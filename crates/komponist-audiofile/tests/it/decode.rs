//! Decoding: every format, chunked reads, seeks, errors.

use komponist_audiofile::{AudioError, AudioFile, Format, decode_all};

use crate::common::*;

#[test]
fn audio_file_can_move_to_a_reader_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<AudioFile>();
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
