//! Offline tempo estimates through the file pool, with known answers at four sample rates.
use std::sync::{Arc, mpsc};
use std::time::Duration;

use komponist_audiofile::{FilePool, PoolFile, TempoEstimate, TempoState};

use super::common::{Scratch, sine, write_wav_f32};

fn analysis(file: &Arc<PoolFile>) -> Option<TempoEstimate> {
    let (tx, rx) = mpsc::channel();
    let worker = file.request_tempo(move || tx.send(std::thread::current().id()).unwrap());
    let result = match worker {
        TempoState::Pending => {
            let worker_thread = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("tempo worker returned no result");
            assert_ne!(worker_thread, std::thread::current().id());
            file.tempo_state()
        }
        result => result,
    };
    match result {
        TempoState::Ready(result) => result,
        result => panic!("unexpected tempo state: {result:?}"),
    }
}

fn rhythm(rate: u32, bpm: f64, beats: usize, pattern: usize) -> Vec<f32> {
    let beat = f64::from(rate) * 60.0 / bpm;
    let mut data = vec![0.0; (beat * beats as f64).round() as usize];
    for i in 0..beats {
        let accent = match (pattern, i % 4) {
            (0, _) | (_, 0) => 1.0,
            (_, 2) => 0.76,
            _ => 0.51,
        };
        let quarter = (i as f64 * beat).round() as usize;
        let eighth = ((i as f64 + 0.5) * beat).round() as usize;
        for n in 0..(rate / 13) as usize {
            let t = n as f32 / rate as f32;
            let kick = (std::f32::consts::TAU * (83.0 - 210.0 * t) * t).sin();
            let attack = (t / 0.002).min(1.0);
            data[quarter + n] += accent * 0.64 * attack * kick * (-t / 0.028).exp();
            if pattern == 2 {
                let hat = (std::f32::consts::TAU * 2964.0 * t).sin()
                    + 0.7 * (std::f32::consts::TAU * 4458.0 * t).sin();
                data[eighth + n] += 0.045 * attack * hat * (-t / 0.014).exp();
            }
        }
    }
    data
}

#[test]
fn estimates_synthetic_loops_at_four_sample_rates() {
    let scratch = Scratch::new("tempo-loops");
    let pool = FilePool::new();
    let mut exact = 0;
    let mut octave = 0;
    for (i, reference) in [
        55.0, 63.0, 70.0, 78.5, 80.0, 86.0, 90.0, 92.5, 98.0, 102.5, 108.0, 115.0, 120.0, 127.5,
        136.0, 142.0, 151.5, 160.0, 174.0, 184.0, 197.0, 205.0,
    ]
    .into_iter()
    .enumerate()
    {
        let rate = [12000, 24000, 44100, 48000][i % 4];
        let signal = rhythm(rate, reference, [4, 8, 16][(i / 3) % 3], i % 3);
        let opposite = signal.iter().map(|&sample| -sample).collect();
        let path = scratch.path(&format!("loop-{i}.wav"));
        write_wav_f32(&path, rate, &[signal, opposite]);
        let result = analysis(&pool.open(&path).unwrap())
            .unwrap_or_else(|| panic!("no candidate for {reference} BPM at {rate} Hz"));
        let within_two = |bpm: f64| (bpm - reference).abs() <= 2.0;
        eprintln!(
            "{reference:6.2} -> {:6.2}, confidence {:.2}",
            result.bpm, result.confidence
        );
        if within_two(result.bpm) {
            exact += 1;
        } else if within_two(result.bpm / 2.0) || within_two(result.bpm * 2.0) {
            octave += 1;
        } else {
            panic!(
                "{} BPM is outside the 2 BPM or octave tolerance for {reference}",
                result.bpm
            );
        }
        assert!(result.confidence > 0.0 && result.confidence <= 1.0);
    }
    eprintln!("{exact} direct matches and {octave} matches after octave correction");
    assert!(
        exact >= 20,
        "expected at least 20 direct matches on this synthetic set"
    );
}

#[test]
fn silence_stationary_tone_and_irregular_transients_have_no_confident_period() {
    let scratch = Scratch::new("tempo-unsuitable");
    let pool = FilePool::new();
    let rate = 12000;
    let mut irregular = vec![0.0; 4 * rate as usize];
    for at in [1600, 5300, 15100, 40100] {
        irregular[at..at + 240].copy_from_slice(&sine(85.0, rate, 240, 0.6));
    }
    for (i, signal) in [
        vec![0.0; 3 * rate as usize],
        sine(440.0, rate, 3 * rate as usize, 0.5),
        irregular,
    ]
    .into_iter()
    .enumerate()
    {
        let path = scratch.path(&format!("quiet-{i}.wav"));
        write_wav_f32(&path, rate, &[signal]);
        assert_eq!(analysis(&pool.open(&path).unwrap()), None, "case {i}");
    }
}

#[test]
fn tempo_cache_is_versioned_and_does_not_reuse_a_replaced_file() {
    let scratch = Scratch::new("tempo-cache");
    let path = scratch.path("loop.wav");
    let cache = scratch.path("cache");
    let pool = FilePool::new();
    pool.set_cache_dir(Some(cache.clone()));
    write_wav_f32(&path, 12000, &[rhythm(12000, 90.0, 8, 0)]);
    let before = pool.open(&path).unwrap();
    assert!(matches!(before.tempo_state(), TempoState::Idle));
    let original = analysis(&before).unwrap();
    assert!((original.bpm - 90.0).abs() < 1.0);
    assert!(matches!(
        before.request_tempo(|| panic!("a ready estimate runs no callback")),
        TempoState::Ready(Some(estimate)) if estimate == original
    ));
    let files: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].extension().unwrap(), "tempo");

    let cached = TempoEstimate {
        bpm: 123.45,
        confidence: original.confidence,
    };
    let mut record = std::fs::read(&files[0]).unwrap();
    assert_eq!(&record[..8], b"KPTEMPO2");
    record[8..16].copy_from_slice(&cached.bpm.to_le_bytes());
    std::fs::write(&files[0], &record).unwrap();

    let fresh = FilePool::new();
    fresh.set_cache_dir(Some(cache.clone()));
    assert_eq!(analysis(&fresh.open(&path).unwrap()), Some(cached));
    assert_eq!(std::fs::read(&files[0]).unwrap(), record);

    // Keep a valid but distinguishable payload and change only its version. The second
    // fresh pool cannot see a Ready entry that is still live on the previous worker.
    record[..8].copy_from_slice(b"KPTEMPO1");
    std::fs::write(&files[0], record).unwrap();
    let after_corruption = FilePool::new();
    after_corruption.set_cache_dir(Some(cache));
    assert_eq!(
        analysis(&after_corruption.open(&path).unwrap()),
        Some(original)
    );
    assert_eq!(&std::fs::read(&files[0]).unwrap()[..8], b"KPTEMPO2");

    write_wav_f32(&path, 12000, &[rhythm(12000, 160.0, 8, 1)]);
    let after = pool.open(&path).unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    let replacement = analysis(&after).unwrap();
    assert!((replacement.bpm - 160.0).abs() < 1.0);
    assert_ne!(replacement, original);
}

// APFS accepts only valid UTF-8 filenames. Portable native-path hashing is tested
// without filesystem access in pool::tests.
#[cfg(target_os = "linux")]
#[test]
fn cache_keeps_non_utf8_paths_and_pre_epoch_versions_distinct() {
    use std::ffi::OsString;
    use std::fs::{FileTimes, OpenOptions};
    use std::os::unix::ffi::OsStringExt;
    use std::time::UNIX_EPOCH;

    let scratch = Scratch::new("tempo-native-identity");
    let cache = scratch.path("cache");
    let first = scratch
        .0
        .join(OsString::from_vec(b"clip-\xff.wav".to_vec()));
    let second = scratch
        .0
        .join(OsString::from_vec(b"clip-\xfe.wav".to_vec()));
    let past = UNIX_EPOCH - Duration::from_secs(60);
    let write = |path: &std::path::Path, bpm: f64, beats: usize, mtime| {
        write_wav_f32(path, 12000, &[rhythm(12000, bpm, beats, 0)]);
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(mtime))
            .unwrap();
    };
    write(&first, 90.0, 4, past);
    write(&second, 180.0, 8, past);
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());
    let pool = FilePool::new();
    pool.set_cache_dir(Some(cache.clone()));
    let first_file = pool.open(&first).unwrap();
    let second_file = pool.open(&second).unwrap();
    assert_eq!(first_file.id().len, second_file.id().len);
    assert_eq!(first_file.id().mtime_ns, Some(-60_000_000_000));
    assert_eq!(first_file.id().mtime_ns, second_file.id().mtime_ns);
    let a = analysis(&first_file).unwrap();
    let b = analysis(&second_file).unwrap();
    assert!((a.bpm - 90.0).abs() < 1.0, "{a:?}");
    assert!((b.bpm - 180.0).abs() < 1.0, "{b:?}");

    // Rewrite a known source with a different rhythm at the same length, keeping its mtime
    // before the Unix epoch. Holding the old handle also verifies in-memory invalidation.
    write(&first, 180.0, 8, past - Duration::from_secs(60));
    let updated = pool.open(&first).unwrap();
    assert_eq!(first_file.id().len, updated.id().len);
    assert!(!Arc::ptr_eq(&first_file, &updated));
    let result = analysis(&updated).unwrap();
    assert!((result.bpm - 180.0).abs() < 1.0, "{result:?}");

    let fresh = FilePool::new();
    fresh.set_cache_dir(Some(cache));
    assert_eq!(analysis(&fresh.open(&first).unwrap()), Some(result));
    assert_eq!(analysis(&fresh.open(&second).unwrap()), Some(b));
}
