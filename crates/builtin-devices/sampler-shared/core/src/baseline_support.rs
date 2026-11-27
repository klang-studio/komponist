//! Helpers for the LFO baseline of #362, shared by sampler-core's and General-Sampler's tests
//! (General-Sampler includes this file with `#[path]`): the block splits, the FNV-1a render digest
//! and its comparison with the committed reference.

/// A small deterministic generator for test scripts and irregular splits (splitmix64), independent
/// of the RNG the voices use.
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

    /// Uniform in `0..1` (24 bits).
    pub(crate) fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u32 << 24) as f32
    }
}

/// How the host cuts the samples into calls. sampler-core renders per sample, but the
/// free-running LFO phases advance once per call (`Engine::advance_lfo_phases`).
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

    /// The next call length, each call.
    pub(crate) fn sizes(self) -> impl FnMut() -> usize {
        let mut dice = Dice(match self {
            Split::Fixed(_) => 0,
            Split::Irregular { seed, .. } => seed,
        });
        move || match self {
            Split::Fixed(n) => n,
            Split::Irregular { max, .. } => dice.size(max),
        }
    }
}

/// The block splits of #358, the irregular one capped at 256 (General-Sampler's tests activate
/// with that maximum block size).
pub(crate) const SPLITS: [Split; 7] = [
    Split::Fixed(1),
    Split::Fixed(31),
    Split::Fixed(32),
    Split::Fixed(33),
    Split::Fixed(64),
    Split::Fixed(256),
    Split::Irregular {
        seed: 362,
        max: 256,
    },
];

/// FNV-1a (64 bit) over the little-endian bytes of `u32` words.
pub(crate) struct Digest(pub(crate) u64);

impl Digest {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn word(&mut self, word: u32) {
        for byte in word.to_le_bytes() {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub(crate) fn samples(&mut self, samples: &[f32]) {
        samples.iter().for_each(|s| self.word(s.to_bits()));
    }
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
    // Every name once on both sides, so each committed entry is checked exactly once.
    let unique = |names: Vec<&str>| {
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate digest names: {names:?}"
        );
    };
    unique(actual.iter().map(|(name, _)| name.as_str()).collect());
    unique(expected.iter().map(|(name, _)| *name).collect());
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
