//! Fixed-size delay lines with a shared write position, for the chorus and
//! the delay. Nothing is allocated.

/// `K` lines of `LEN` samples, written together.
pub(crate) struct DelayLines<const LEN: usize, const K: usize> {
    lines: [[f32; LEN]; K],
    /// Where the next samples go.
    pos: usize,
    /// Samples written since the last clear (up to `LEN`); older ones read
    /// as silence, so clearing costs nothing.
    written: usize,
}

impl<const LEN: usize, const K: usize> DelayLines<LEN, K> {
    /// Writes silent lines to `this` without building them on the stack.
    ///
    /// # Safety
    /// `this` must be valid for writes and suitably aligned.
    pub(crate) unsafe fn write_new(this: *mut Self) {
        // SAFETY: guaranteed by the caller; all-zero bytes are silent lines
        // at position 0.
        unsafe { this.write_bytes(0, 1) };
    }

    /// Silences the lines.
    pub(crate) fn clear(&mut self) {
        self.written = 0;
    }

    /// Appends one sample per line.
    #[inline]
    pub(crate) fn push(&mut self, samples: [f32; K]) {
        for (line, x) in self.lines.iter_mut().zip(samples) {
            line[self.pos] = x;
        }
        self.pos = if self.pos + 1 == LEN { 0 } else { self.pos + 1 };
        self.written = (self.written + 1).min(LEN);
    }

    /// Line `k` `back` samples before the newest one (`back` = 0, at most
    /// `LEN - 2`), linearly interpolated.
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
            self.pos + LEN - d
        };
        self.lines[k][i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines<const LEN: usize>() -> Box<DelayLines<LEN, 2>> {
        let mut lines = Box::<DelayLines<LEN, 2>>::new_uninit();
        // SAFETY: a valid place, fully initialized by `write_new`.
        unsafe {
            DelayLines::write_new(lines.as_mut_ptr());
            lines.assume_init()
        }
    }

    #[test]
    fn reads_back_interpolates_and_wraps() {
        let mut d = lines::<8>();
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
        let mut d = lines::<8>();
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
