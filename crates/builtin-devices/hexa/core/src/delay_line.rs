//! Delay lines with a shared write position, for the chorus and the delay.
//! The samples are one heap allocation made when the effect is built (at
//! activation); nothing is allocated afterwards.

use alloc::boxed::Box;
use alloc::vec;

/// `K` lines of the same length, written together.
pub(crate) struct DelayLines<const K: usize> {
    /// Line `k` is `samples[k * len..(k + 1) * len]`.
    samples: Box<[f32]>,
    len: usize,
    /// Where the next samples go.
    pos: usize,
    /// Samples written since the last clear (up to `len`); older ones read
    /// as silence, so clearing costs nothing.
    written: usize,
}

impl<const K: usize> DelayLines<K> {
    /// Silent lines of `len` samples (at least 4) each.
    pub(crate) fn new(len: usize) -> Self {
        let len = len.max(4);
        Self {
            samples: vec![0.0; K * len].into_boxed_slice(),
            len,
            pos: 0,
            written: 0,
        }
    }

    /// Samples per line.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Silences the lines.
    pub(crate) fn clear(&mut self) {
        self.written = 0;
    }

    /// Appends one sample per line.
    #[inline]
    pub(crate) fn push(&mut self, samples: [f32; K]) {
        for (k, x) in samples.into_iter().enumerate() {
            self.samples[k * self.len + self.pos] = x;
        }
        self.pos = if self.pos + 1 == self.len {
            0
        } else {
            self.pos + 1
        };
        self.written = (self.written + 1).min(self.len);
    }

    /// Line `k` `back` samples before the newest one (`back` = 0, at most
    /// `len - 2`), linearly interpolated.
    #[inline]
    pub(crate) fn read(&self, k: usize, back: f32) -> f32 {
        let whole = back as usize;
        let frac = back - whole as f32;
        let a = self.at(k, whole);
        let b = self.at(k, whole + 1);
        a + (b - a) * frac
    }

    #[inline]
    fn at(&self, k: usize, back: usize) -> f32 {
        if back >= self.written {
            return 0.0;
        }
        let d = back + 1;
        let i = if self.pos >= d {
            self.pos - d
        } else {
            self.pos + self.len - d
        };
        self.samples[k * self.len + i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(len: usize) -> DelayLines<2> {
        DelayLines::new(len)
    }

    #[test]
    fn reads_back_interpolates_and_wraps() {
        let mut d = lines(8);
        for i in 0..20 {
            d.push([i as f32, -(i as f32)]);
        }
        assert_eq!(d.read(0, 0.0), 19.0);
        assert_eq!(d.read(1, 3.0), -16.0);
        assert_eq!(d.read(0, 2.5), 16.5);
        assert_eq!(d.read(0, 6.0), 13.0);
    }

    #[test]
    fn clearing_silences_the_old_samples() {
        let mut d = lines(8);
        for _ in 0..8 {
            d.push([1.0, 1.0]);
        }
        d.clear();
        assert_eq!(d.read(0, 0.0), 0.0);
        d.push([2.0, 2.0]);
        assert_eq!(d.read(0, 0.0), 2.0);
        assert_eq!(d.read(0, 0.5), 1.0);
        assert_eq!(d.read(0, 1.0), 0.0);
    }
}
