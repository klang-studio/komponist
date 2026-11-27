//! Helpers for the LFO baseline of #358, shared by volt's and surf's tests (surf includes this
//! file with `#[path]`): a deterministic script generator, the block splits, the render digest
//! and its comparison with the committed reference.

use komponist_engine::EventKind;

/// A small deterministic generator for the test scripts (splitmix64), independent of the RNG the
/// devices use.
pub(crate) struct Dice(pub(crate) u64);

impl Dice {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `1..=max`.
    pub(crate) fn size(&mut self, max: usize) -> usize {
        1 + (self.next() % max as u64) as usize
    }

    /// Uniform in 0..1.
    pub(crate) fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// How the host cuts the samples into calls.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Split {
    Fixed(usize),
    /// Sizes 1..=`max` from a fixed seed.
    Irregular {
        seed: u64,
        max: usize,
    },
}

impl Split {
    pub(crate) fn name(self) -> String {
        match self {
            Split::Fixed(n) => format!("split-{n}"),
            Split::Irregular { seed, max } => format!("irregular-{seed}-{max}"),
        }
    }

    /// The calls for `frames` samples: their lengths and the `events` that fall into each,
    /// relative to its start.
    pub(crate) fn calls(
        self,
        frames: usize,
        events: &[(u32, EventKind)],
    ) -> Vec<(usize, Vec<(u32, EventKind)>)> {
        let mut dice = Dice(match self {
            Split::Fixed(_) => 0,
            Split::Irregular { seed, .. } => seed,
        });
        let mut calls = Vec::new();
        let mut done = 0;
        while done < frames {
            let n = match self {
                Split::Fixed(n) => n,
                Split::Irregular { max, .. } => dice.size(max),
            }
            .min(frames - done);
            let here = events
                .iter()
                .filter(|(o, _)| (done..done + n).contains(&(*o as usize)))
                .map(|(o, k)| (*o - done as u32, *k))
                .collect();
            calls.push((n, here));
            done += n;
        }
        calls
    }
}

/// The block splits of #358.
pub(crate) const SPLITS: [Split; 7] = [
    Split::Fixed(1),
    Split::Fixed(31),
    Split::Fixed(32),
    Split::Fixed(33),
    Split::Fixed(64),
    Split::Fixed(256),
    Split::Irregular {
        seed: 358,
        max: 300,
    },
];

/// FNV-1a (64 bit) over the little-endian bytes of the samples' bits.
pub(crate) fn digest<'a>(channels: impl IntoIterator<Item = &'a [f32]>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for channel in channels {
        for sample in channel {
            for byte in sample.to_bits().to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

/// Renders `frames` with `events` in every split of [`SPLITS`] (`render` plays the calls on a
/// fresh instance and returns left and right), asserts that all splits give the same bits (volt's
/// engine steps its control rate across calls) and returns their digest.
pub(crate) fn split_independent_digest(
    frames: usize,
    events: &[(u32, EventKind)],
    mut render: impl FnMut(Vec<(usize, Vec<(u32, EventKind)>)>) -> [Vec<f32>; 2],
) -> u64 {
    let mut first: Option<(u64, Split)> = None;
    for split in SPLITS {
        let [l, r] = render(split.calls(frames, events));
        assert_eq!(l.len(), frames);
        assert!(
            l.iter().any(|s| s.abs() > 0.01),
            "silent with {}",
            split.name()
        );
        let d = digest([&l[..], &r[..]]);
        match first {
            None => first = Some((d, split)),
            Some((d0, s0)) => assert_eq!(
                d,
                d0,
                "{} and {} differ: the output must not depend on the block split",
                split.name(),
                s0.name()
            ),
        }
    }
    first.expect("splits").0
}

/// Compares `actual` with the committed `expected` digests on the reference platform (Linux
/// x86_64); elsewhere prints them. On a mismatch the message holds the table to commit if the
/// change is intended.
pub(crate) fn check_digests(device: &str, actual: &[(String, u64)], expected: &[(&str, u64)]) {
    let table: String = actual
        .iter()
        .map(|(name, digest)| format!("    (\"{name}\", {digest:#018x}),\n"))
        .collect();
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        eprintln!(
            "{device} LFO render digests, not compared (the reference platform is Linux \
             x86_64):\n{table}"
        );
        return;
    }
    let differ: Vec<&str> = actual
        .iter()
        .filter(|(name, digest)| !expected.iter().any(|(n, d)| n == name && d == digest))
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(
        differ.is_empty() && actual.len() == expected.len(),
        "{device} render digests differ from the committed reference: {differ:?}.\n\
         Only if the change is intended, commit this table:\n{table}"
    );
}
