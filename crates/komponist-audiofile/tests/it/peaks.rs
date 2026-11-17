//! Peak data: levels, queries, the builder and the disk format.

use komponist_audiofile::peaks::cache;
use komponist_audiofile::{AudioFile, FileId, Peak, PeakBuilder, PeakData};

use crate::common::*;

fn brute(x: &[f32], from: usize, to: usize) -> Peak {
    let part = &x[from.min(x.len())..to.min(x.len())];
    Peak {
        min: part.iter().copied().fold(f32::INFINITY, f32::min),
        max: part.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    }
}

#[test]
fn level_zero_is_min_max_per_base_block() {
    let x = noise(1000, 11);
    let p = PeakData::from_planar(44100, std::slice::from_ref(&x));
    assert_eq!(
        (p.frames(), p.channels(), p.sample_rate()),
        (1000, 1, 44100)
    );
    let l0 = p.level(0, 0).unwrap();
    assert_eq!(l0.len(), 1000usize.div_ceil(128));
    for (b, peak) in l0.iter().enumerate() {
        assert_eq!(*peak, brute(&x, b * 128, (b + 1) * 128), "bucket {b}");
    }
}

#[test]
fn levels_shrink_by_four_down_to_one_bucket() {
    let x = noise(100_000, 5);
    let p = PeakData::from_planar(48000, std::slice::from_ref(&x));
    let mut buckets = 100_000usize.div_ceil(128);
    for level in 0..p.level_count() {
        let block = p.block_frames(level).unwrap();
        assert_eq!(block, 128 * 4u64.pow(level as u32));
        assert_eq!(p.level(level, 0).unwrap().len(), buckets, "level {level}");
        // Every bucket is exactly the min/max of its frames.
        for (b, peak) in p.level(level, 0).unwrap().iter().enumerate() {
            let from = b * block as usize;
            assert_eq!(
                *peak,
                brute(&x, from, from + block as usize),
                "level {level} bucket {b}"
            );
        }
        buckets = buckets.div_ceil(4);
    }
    assert_eq!(p.level(p.level_count() - 1, 0).unwrap().len(), 1);
    assert!(p.level(p.level_count(), 0).is_none());
}

#[test]
fn memory_stays_small() {
    // 10 minutes of stereo at 48 kHz: a few megabytes, not a copy of the audio.
    let frames = 10 * 60 * 48000usize;
    let mut b = PeakBuilder::new(48000, 2);
    let chunk = noise(48000 * 2, 2);
    for _ in 0..(frames / 48000) {
        b.push(&chunk);
    }
    let p = b.finish();
    assert_eq!(p.frames(), frames as u64);
    assert!(p.byte_size() < 12 * 1024 * 1024, "{} bytes", p.byte_size());
}

#[test]
fn aligned_queries_are_exact_at_every_level() {
    let x = noise(200_000, 9);
    let p = PeakData::from_planar(44100, std::slice::from_ref(&x));
    for (block, columns) in [
        (128usize, 100usize),
        (512, 100),
        (2048, 50),
        (8192, 20),
        (32768, 5),
    ] {
        let q = p.query(0, 0, (block * columns) as u64, columns);
        assert!(q.frames_per_bucket <= block as u64);
        for (c, peak) in q.columns.iter().enumerate() {
            assert_eq!(
                *peak,
                brute(&x, c * block, (c + 1) * block),
                "block {block} column {c}"
            );
        }
    }
}

#[test]
fn any_query_contains_the_true_peaks() {
    // Unaligned ranges come from the level below the column width, so a column may show a
    // little more than its frames, never less.
    let x = noise(150_000, 21);
    let p = PeakData::from_planar(44100, std::slice::from_ref(&x));
    for (start, end, columns) in [
        (0u64, 150_000u64, 300usize),
        (1234, 99_999, 777),
        (70_000, 70_500, 50),
        (10, 20_000, 3),
        (140_000, 150_000, 1000),
    ] {
        let q = p.query(0, start, end, columns);
        assert_eq!(q.columns.len(), columns);
        for (c, peak) in q.columns.iter().enumerate() {
            let from = start + (end - start) * c as u64 / columns as u64;
            let to = start + (end - start) * (c as u64 + 1) / columns as u64;
            let truth = brute(&x, from as usize, (to.max(from + 1)) as usize);
            assert!(
                peak.min <= truth.min && peak.max >= truth.max,
                "{start}..{end}/{columns} column {c}"
            );
            // And not absurdly more: within one bucket of the level used on each side.
            let slack = (end - start) / columns as u64 * 2 + 2 * q.frames_per_bucket;
            let wide = brute(
                &x,
                from.saturating_sub(slack) as usize,
                (to + slack) as usize,
            );
            assert!(peak.min >= wide.min && peak.max <= wide.max);
        }
    }
}

#[test]
fn the_query_picks_the_level_that_fits_the_width() {
    let p = PeakData::from_planar(44100, &[noise(1_000_000, 4)]);
    // 1 000 000 frames over 1000 columns: 1000 frames per column, so buckets of at most 250
    // frames: block 128 (level 0).
    assert_eq!(p.query(0, 0, 1_000_000, 1000).frames_per_bucket, 128);
    // 4 000 000 would be 4000 per column: buckets of at most 1000 frames: block 512.
    let long = PeakData::from_planar(44100, &[noise(4_000_000, 4)]);
    assert_eq!(long.query(0, 0, 4_000_000, 1000).frames_per_bucket, 512);
    // Zoomed in beyond the base block: level 0, and the answer says it is 128 frames per bucket.
    assert_eq!(p.query(0, 0, 10_000, 1000).frames_per_bucket, 128);
    // A whole-file overview of 8 columns uses a far coarser level.
    assert_eq!(p.query(0, 0, 1_000_000, 8).frames_per_bucket, 8192);
}

#[test]
fn queries_outside_the_file_read_silence() {
    let p = PeakData::from_planar(44100, &[vec![0.5; 1000]]);
    let q = p.query(0, 2000, 3000, 4);
    assert_eq!(q.columns, vec![Peak::SILENT; 4]);
    // A range that hangs over the end: the inside shows the signal, the outside silence.
    let q = p.query(0, 500, 1500, 2);
    assert_eq!(q.columns[0], Peak { min: 0.5, max: 0.5 });
    assert_eq!(q.columns[1], Peak::SILENT);
    // Empty range, no columns, wrong channel.
    assert!(p.query(0, 10, 10, 5).columns.is_empty());
    assert!(p.query(0, 0, 100, 0).columns.is_empty());
    assert_eq!(p.query(1, 0, 100, 3).columns, vec![Peak::SILENT; 3]);
    // Nothing at all.
    let empty = PeakData::from_planar(44100, &[Vec::new()]);
    assert_eq!(empty.level_count(), 0);
    assert_eq!(empty.query(0, 0, 100, 2).columns, vec![Peak::SILENT; 2]);
}

#[test]
fn seconds_map_to_source_frames() {
    let mut x = vec![0.0f32; 44100 * 2];
    x[44100 + 10] = 0.9; // at 1.0002 s
    let p = PeakData::from_planar(44100, &[x]);
    let q = p.query_secs(0, 1.0, 2.0, 1);
    assert_eq!(q.columns[0].max, 0.9);
    let q = p.query_secs(0, 0.0, 0.9, 1);
    assert_eq!(q.columns[0].max, 0.0);
}

#[test]
fn stereo_channels_have_their_own_peaks() {
    let l = vec![0.25f32; 500];
    let r = vec![-0.75f32; 500];
    let p = PeakData::from_planar(44100, &[l, r]);
    assert_eq!(p.channels(), 2);
    assert_eq!(
        p.query(0, 0, 500, 1).columns[0],
        Peak {
            min: 0.25,
            max: 0.25
        }
    );
    assert_eq!(
        p.query(1, 0, 500, 1).columns[0],
        Peak {
            min: -0.75,
            max: -0.75
        }
    );
    assert_eq!(p.query(1, 0, 500, 1).columns[0].abs_max(), 0.75);
}

#[test]
fn the_builder_does_not_care_about_chunk_sizes() {
    let l = noise(5000, 1);
    let r = noise(5000, 2);
    let planar = PeakData::from_planar(44100, &[l.clone(), r.clone()]);
    let interleaved: Vec<f32> = (0..5000).flat_map(|i| [l[i], r[i]]).collect();
    let mut b = PeakBuilder::new(44100, 2);
    for chunk in interleaved.chunks(2 * 333) {
        b.push(chunk);
    }
    assert_eq!(b.finish(), planar);
}

#[test]
fn peaks_of_a_file_equal_peaks_of_its_samples() {
    let dir = Scratch::new("peaks-file");
    let planar = vec![noise(40_000, 31), noise(40_000, 32)];
    let path = dir.path("n.wav");
    write_wav_f32(&path, 44100, &planar);
    let from_file = PeakData::from_file(&mut AudioFile::open(&path).unwrap()).unwrap();
    assert_eq!(from_file, PeakData::from_planar(44100, &planar));
}

fn id(len: u64, mtime_ns: u64) -> FileId {
    FileId {
        path: "/some/file.wav".into(),
        len,
        mtime_ns,
    }
}

#[test]
fn the_disk_format_round_trips() {
    let p = PeakData::from_planar(44100, &[noise(20_000, 3), noise(20_000, 4)]);
    let bytes = cache::to_bytes(&p, &id(77, 88));
    assert_eq!(cache::from_bytes(&bytes, &id(77, 88)), Some(p));
}

#[test]
fn stale_or_damaged_cache_files_are_misses() {
    let p = PeakData::from_planar(44100, &[noise(20_000, 3)]);
    let bytes = cache::to_bytes(&p, &id(77, 88));
    // The file changed.
    assert_eq!(cache::from_bytes(&bytes, &id(78, 88)), None);
    assert_eq!(cache::from_bytes(&bytes, &id(77, 89)), None);
    let mut shape = bytes.clone();
    shape[40..48].copy_from_slice(&256u64.to_le_bytes()); // a block size no level has
    assert_eq!(cache::from_bytes(&shape, &id(77, 88)), None);
    // Truncated at every length, wrong magic, trailing junk, a count that promises too much.
    for cut in [0, 3, 10, 40, bytes.len() / 2, bytes.len() - 1] {
        assert_eq!(
            cache::from_bytes(&bytes[..cut], &id(77, 88)),
            None,
            "cut at {cut}"
        );
    }
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert_eq!(cache::from_bytes(&bad, &id(77, 88)), None);
    let mut long = bytes.clone();
    long.push(0);
    assert_eq!(cache::from_bytes(&long, &id(77, 88)), None);
    let mut huge = bytes.clone();
    huge[48..56].copy_from_slice(&u64::MAX.to_le_bytes()); // bucket count of level 0
    assert_eq!(cache::from_bytes(&huge, &id(77, 88)), None);
}

#[test]
fn the_cache_folder_is_created_and_files_are_named_by_identity() {
    let dir = Scratch::new("peaks-cache");
    let folder = dir.path("deep/er/cache");
    let p = PeakData::from_planar(44100, &[noise(5000, 3)]);
    let a = id(1, 2);
    cache::write(&folder, &a, &p).unwrap();
    assert_eq!(cache::read(&folder, &a), Some(p));
    assert_eq!(cache::read(&folder, &id(1, 3)), None);
    assert_eq!(
        cache::path_for(&folder, &a),
        cache::path_for(&folder, &a.clone())
    );
    assert_ne!(
        cache::path_for(&folder, &a),
        cache::path_for(&folder, &id(1, 3))
    );
    assert_eq!(
        cache::path_for(&folder, &a)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .len(),
        16 + ".kpeaks".len()
    );
}

fn assert_rms_close(actual: f32, expected: f64) {
    assert!(
        (f64::from(actual) - expected).abs() < 1e-6,
        "RMS {actual}, expected {expected}"
    );
}

#[test]
fn rms_distinguishes_an_impulse_from_a_constant_with_the_same_peak() {
    let constant = PeakData::from_planar(48000, &[vec![1.0; 128]]);
    let mut impulse = vec![0.0; 128];
    impulse[0] = 1.0;
    let impulse = PeakData::from_planar(48000, &[impulse]);
    assert_eq!(constant.query(0, 0, 128, 1).columns[0].max, 1.0);
    assert_eq!(impulse.query(0, 0, 128, 1).columns[0].max, 1.0);
    assert_rms_close(constant.query_rms(0, 0, 128, 1).columns[0], 1.0);
    assert_rms_close(
        impulse.query_rms(0, 0, 128, 1).columns[0],
        (1.0f64 / 128.0).sqrt(),
    );
}

#[test]
fn rms_weights_partial_buckets_by_their_actual_sample_count() {
    // One full silent bucket and one sample at 1. An average of bucket RMS would be wrong.
    let mut samples = vec![0.0; 128];
    samples.push(1.0);
    let p = PeakData::from_planar(48000, &[samples]);
    assert_rms_close(
        p.query_rms(0, 0, 129, 1).columns[0],
        (1.0f64 / 129.0).sqrt(),
    );
    assert_rms_close(p.query_rms(0, 128, 129, 1).columns[0], 1.0);
    // Select a higher pyramid level containing the same partial final bucket.
    let q = p.query_rms(0, 0, 4096, 1);
    assert_eq!(q.frames_per_bucket, 512);
    assert_rms_close(q.columns[0], (1.0f64 / 129.0).sqrt());
}

#[test]
fn rms_queries_match_measured_energy_at_every_pyramid_resolution() {
    let samples = noise(200_003, 91);
    let p = PeakData::from_planar(44100, std::slice::from_ref(&samples));
    for (block, columns) in [
        (128usize, 100usize),
        (512, 100),
        (2048, 50),
        (8192, 20),
        (32768, 5),
    ] {
        let q = p.query_rms(0, 0, (block * columns) as u64, columns);
        assert_eq!(
            q.frames_per_bucket,
            p.query(0, 0, (block * columns) as u64, columns)
                .frames_per_bucket
        );
        for (c, &rms) in q.columns.iter().enumerate() {
            let part = &samples[c * block..(c + 1) * block];
            let energy: f64 = part.iter().map(|&x| f64::from(x).powi(2)).sum();
            assert_rms_close(rms, (energy / part.len() as f64).sqrt());
        }
    }
    // A whole-file query also includes the final partial bucket with its true weight.
    let q = p.query_rms(0, 0, samples.len() as u64, 1);
    let energy: f64 = samples.iter().map(|&x| f64::from(x).powi(2)).sum();
    assert_rms_close(q.columns[0], (energy / samples.len() as f64).sqrt());
}

#[test]
fn rms_channels_empty_ranges_and_outside_file_match_peak_queries() {
    let p = PeakData::from_planar(48000, &[vec![0.25; 256], vec![-0.75; 256]]);
    assert_eq!(p.query_rms(0, 0, 256, 2).columns, [0.25, 0.25]);
    assert_eq!(p.query_rms(1, 0, 256, 2).columns, [0.75, 0.75]);
    assert_eq!(p.query_rms(0, 128, 384, 2).columns, [0.25, 0.0]);
    assert_eq!(p.query_rms(2, 0, 256, 2).columns, [0.0, 0.0]);
    assert!(p.query_rms(0, 10, 10, 2).columns.is_empty());
    assert!(p.query_rms(0, 0, 256, 0).columns.is_empty());
    assert_eq!(
        PeakData::from_planar(48000, &[Vec::new()])
            .query_rms(0, 0, 1, 2)
            .columns,
        [0.0, 0.0]
    );
    // Large public query ranges must not overflow while multiplying by the column number.
    assert_eq!(
        p.query_rms(0, u64::MAX - 100, u64::MAX, 4).columns,
        [0.0; 4]
    );
    assert_eq!(
        p.query(0, u64::MAX - 100, u64::MAX, 4).columns,
        [Peak::SILENT; 4]
    );
}

#[test]
fn old_peak_caches_and_invalid_rms_energy_are_misses() {
    let p = PeakData::from_planar(48000, &[vec![0.5; 128]]);
    let bytes = cache::to_bytes(&p, &id(11, 22));
    assert_eq!(&bytes[..4], b"KPK2");
    let mut old = bytes.clone();
    old[..4].copy_from_slice(b"KPK1");
    assert_eq!(cache::from_bytes(&old, &id(11, 22)), None);
    for invalid in [
        -1.0f64,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
    ] {
        let mut corrupt = bytes.clone();
        // 40-byte header + 16-byte level header + min/max pair.
        corrupt[64..72].copy_from_slice(&invalid.to_le_bytes());
        assert_eq!(cache::from_bytes(&corrupt, &id(11, 22)), None);
    }
}
