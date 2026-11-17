//! The file pool: sharing by identity, decode once, peaks off-thread, the disk cache.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use komponist_audiofile::peaks::cache;
use komponist_audiofile::{AudioError, AudioFile, Decoded, FilePool, PeakData, PeakState};

use crate::common::*;

fn wav(dir: &Scratch, name: &str, frames: usize) -> std::path::PathBuf {
    let path = dir.path(name);
    write_wav_f32(&path, 44100, &[noise(frames, 1), noise(frames, 2)]);
    path
}

#[test]
fn one_file_is_one_entry_however_it_is_reached() {
    let dir = Scratch::new("pool-identity");
    let path = wav(&dir, "a.wav", 5000);
    std::fs::create_dir(dir.path("sub")).unwrap();
    let pool = FilePool::new();
    let a = pool.open(&path).unwrap();
    // Another spelling of the same path.
    let b = pool.open(&dir.path("sub/../a.wav")).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&path, dir.path("link.wav")).unwrap();
        assert!(Arc::ptr_eq(&a, &pool.open(&dir.path("link.wav")).unwrap()));
    }
    assert_eq!(pool.live_files(), 1);
    // A second file is a second entry.
    let other = pool.open(&wav(&dir, "b.wav", 5000)).unwrap();
    assert!(!Arc::ptr_eq(&a, &other));
    assert_eq!(pool.live_files(), 2);
}

#[test]
fn an_entry_lives_as_long_as_somebody_holds_it() {
    let dir = Scratch::new("pool-refcount");
    let path = wav(&dir, "a.wav", 5000);
    let pool = FilePool::new();
    let a = pool.open(&path).unwrap();
    let b = pool.open(&path).unwrap();
    drop(a);
    assert_eq!(pool.live_files(), 1);
    let weak = Arc::downgrade(&b);
    drop(b);
    assert_eq!(pool.live_files(), 0);
    assert!(
        weak.upgrade().is_none(),
        "the pool does not keep entries alive"
    );
    // Opened again: a fresh entry that works.
    assert_eq!(pool.open(&path).unwrap().info().frames, 5000);
}

#[test]
fn a_file_that_changed_on_disk_is_a_new_entry() {
    let dir = Scratch::new("pool-changed");
    let path = wav(&dir, "a.wav", 5000);
    let pool = FilePool::new();
    let old = pool.open(&path).unwrap();
    wav(&dir, "a.wav", 7000);
    let new = pool.open(&path).unwrap();
    assert!(!Arc::ptr_eq(&old, &new));
    assert_eq!((old.info().frames, new.info().frames), (5000, 7000));
}

#[test]
fn entries_are_shared_between_pool_handles() {
    let dir = Scratch::new("pool-clone");
    let path = wav(&dir, "a.wav", 100);
    let pool = FilePool::new();
    let a = pool.open(&path).unwrap();
    let b = pool.clone().open(&path).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert!(Arc::ptr_eq(
        &FilePool::global().open(&path).unwrap(),
        &FilePool::global().open(&path).unwrap()
    ));
}

#[test]
fn opening_what_is_not_audio_is_an_error() {
    let dir = Scratch::new("pool-errors");
    let pool = FilePool::new();
    assert!(matches!(
        pool.open(&dir.path("nope.wav")),
        Err(AudioError::Io(_))
    ));
    let junk = dir.path("junk.flac");
    std::fs::write(&junk, b"not audio at all, just text").unwrap();
    assert!(pool.open(&junk).is_err());
    assert_eq!(pool.live_files(), 0);
}

#[test]
fn many_users_decode_a_file_once() {
    let dir = Scratch::new("pool-decode-once");
    let file = FilePool::new().open(&wav(&dir, "a.wav", 20_000)).unwrap();
    let results: Vec<Arc<Decoded>> = std::thread::scope(|s| {
        (0..6)
            .map(|_| s.spawn(|| file.decoded(None).unwrap()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });
    assert_eq!(file.decode_count(), 1);
    for r in &results {
        assert!(Arc::ptr_eq(r, &results[0]));
    }
    // Not held any more: the memory is gone, the next user decodes again.
    drop(results);
    assert_eq!(file.decoded(None).unwrap().frames(), 20_000);
    assert_eq!(file.decode_count(), 2);
}

#[test]
fn the_decode_limit_applies_to_cached_audio_too() {
    let dir = Scratch::new("pool-limit");
    let file = FilePool::new()
        .open(&wav(&dir, "a.wav", 44100 * 2))
        .unwrap(); // 2 s
    let held = file.decoded(None).unwrap();
    assert_eq!(file.decoded(Some(1.0)).unwrap_err(), AudioError::TooLong);
    drop(held);
}

#[test]
fn derived_data_is_built_once_and_shared() {
    struct Lengths(usize);
    let dir = Scratch::new("pool-derived");
    let pool = FilePool::new();
    let a = pool.open(&wav(&dir, "a.wav", 9000)).unwrap();
    let b = pool.open(&dir.path("a.wav")).unwrap();
    let builds = std::sync::atomic::AtomicU32::new(0);
    let build = |d: Arc<Decoded>| {
        builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Nobody else holds the audio: the buffers move, no copy.
        let d = Arc::try_unwrap(d).expect("unique");
        Lengths(d.channels[0].len())
    };
    let first = a.derived(None, build).unwrap();
    let second = b.derived(None, build).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.0, 9000);
    assert_eq!(builds.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(a.decode_count(), 1);
    // The decoded buffer itself was released after the build.
    assert_eq!(a.decoded(None).unwrap().frames(), 9000);
    assert_eq!(a.decode_count(), 2);
}

#[test]
fn derived_errors_are_not_cached() {
    struct Marker;
    let dir = Scratch::new("pool-derived-err");
    let file = FilePool::new()
        .open(&wav(&dir, "a.wav", 44100 * 2))
        .unwrap();
    assert!(file.derived(Some(1.0), |_| Marker).is_err());
    assert!(file.derived(None, |_| Marker).is_ok());
}

fn wait(rx: &mpsc::Receiver<()>) {
    rx.recv_timeout(Duration::from_secs(20))
        .expect("peaks arrive");
}

#[test]
fn peaks_come_from_the_worker_and_the_callback_says_when() {
    let dir = Scratch::new("pool-peaks");
    let planar = vec![noise(50_000, 5), noise(50_000, 6)];
    let path = dir.path("a.wav");
    write_wav_f32(&path, 44100, &planar);
    let file = FilePool::new().open(&path).unwrap();
    assert!(matches!(file.peaks_state(), PeakState::Idle));
    assert!(file.peaks_ready().is_none());
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    // Two requests share one computation, both get their callback.
    let first = file.request_peaks(move || tx.send(()).unwrap());
    let second = file.request_peaks(move || tx2.send(()).unwrap());
    assert!(matches!(first, PeakState::Pending | PeakState::Ready(_)));
    assert!(matches!(second, PeakState::Pending | PeakState::Ready(_)));
    wait(&rx);
    wait(&rx);
    let peaks = file.peaks_ready().expect("ready");
    assert_eq!(*peaks, PeakData::from_planar(44100, &planar));
    // Asking again is answered at once, from memory, with the same data.
    let PeakState::Ready(again) = file.request_peaks(|| panic!("no callback when already done"))
    else {
        panic!("ready")
    };
    assert!(Arc::ptr_eq(&peaks, &again));
    assert!(Arc::ptr_eq(&peaks, &file.peaks().unwrap()));
}

#[test]
fn failed_peaks_are_reported_not_retried_forever() {
    let dir = Scratch::new("pool-peaks-fail");
    let path = wav(&dir, "a.wav", 5000);
    let file = FilePool::new().open(&path).unwrap();
    // The file vanishes between open and the peak computation.
    std::fs::remove_file(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    file.request_peaks(move || tx.send(()).unwrap());
    wait(&rx);
    assert!(matches!(
        file.peaks_state(),
        PeakState::Failed(AudioError::Io(_))
    ));
    assert!(file.peaks().is_err());
}

#[test]
fn peaks_use_the_decoded_audio_when_it_is_in_memory() {
    let dir = Scratch::new("pool-peaks-mem");
    let planar = vec![noise(30_000, 8)];
    let path = dir.path("a.wav");
    write_wav_f32(&path, 44100, &planar);
    let file = FilePool::new().open(&path).unwrap();
    let held = file.decoded(None).unwrap();
    // The file is gone, but the audio is held: the peaks come from memory.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        *file.peaks().unwrap(),
        PeakData::from_planar(44100, &planar)
    );
    drop(held);
}

#[test]
fn the_disk_cache_is_written_and_read_back() {
    let dir = Scratch::new("pool-peaks-cache");
    let planar = vec![noise(60_000, 12), noise(60_000, 13)];
    let path = dir.path("a.wav");
    write_wav_f32(&path, 44100, &planar);
    let cache_dir = dir.path("cache");

    let pool = FilePool::new();
    pool.set_cache_dir(Some(cache_dir.clone()));
    assert_eq!(pool.cache_dir(), Some(cache_dir.clone()));
    let file = pool.open(&path).unwrap();
    let computed = file.peaks().unwrap();
    let on_disk = cache::path_for(&cache_dir, file.id());
    assert!(on_disk.is_file(), "{on_disk:?}");
    assert_eq!(
        cache::read(&cache_dir, file.id()).as_ref(),
        Some(&*computed)
    );

    // A fresh pool (a new run of the app) with the audio file unreadable still gets the peaks:
    // they come from the cache. Prove it by planting recognisable peaks under the file's identity.
    let planted = PeakData::from_planar(44100, &[vec![0.125; 1000]]);
    cache::write(&cache_dir, file.id(), &planted).unwrap();
    let fresh = FilePool::new();
    fresh.set_cache_dir(Some(cache_dir.clone()));
    let again = fresh.open(&path).unwrap();
    assert_eq!(*again.peaks().unwrap(), planted);

    // Without a cache folder nothing is read or written.
    let plain = FilePool::new().open(&path).unwrap();
    assert_eq!(*plain.peaks().unwrap(), *computed);
}

#[test]
fn streaming_users_open_their_own_decoder_on_the_entry() {
    let dir = Scratch::new("pool-stream");
    let file = FilePool::new().open(&wav(&dir, "a.wav", 10_000)).unwrap();
    let mut a = AudioFile::open(file.path()).unwrap();
    let mut b = AudioFile::open(file.path()).unwrap();
    a.seek(5000).unwrap();
    assert_eq!((a.position(), b.position()), (5000, 0));
    let mut buf = [0.0f32; 4];
    b.read(&mut buf).unwrap();
    assert_eq!((a.position(), b.position()), (5000, 2));
}
