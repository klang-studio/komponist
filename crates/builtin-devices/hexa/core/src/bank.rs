//! A list of up to `N` entries in fixed storage, for the modulators of a
//! patch (LFOs, envelopes). Serialized as a plain sequence of its entries.

#[derive(Clone, Copy, Debug)]
pub struct Bank<T, const N: usize> {
    items: [T; N],
    len: usize,
}

impl<T: Copy + Default, const N: usize> Bank<T, N> {
    pub fn new() -> Self {
        Self {
            items: [T::default(); N],
            len: 0,
        }
    }

    /// Up to `N` entries of `items`; the rest is dropped.
    pub fn from_slice(items: &[T]) -> Self {
        let mut bank = Self::new();
        for &item in items.iter().take(N) {
            bank.push(item);
        }
        bank
    }

    /// Appends an entry and returns its index, or `None` if the bank is full.
    pub fn push(&mut self, item: T) -> Option<usize> {
        let index = self.len;
        *self.items.get_mut(index)? = item;
        self.len += 1;
        Some(index)
    }

    /// Applies `f` to every entry.
    pub fn map(mut self, f: impl Fn(T) -> T) -> Self {
        for item in self.as_mut_slice() {
            *item = f(*item);
        }
        self
    }
}

impl<T, const N: usize> Bank<T, N> {
    pub fn as_slice(&self) -> &[T] {
        &self.items[..self.len]
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.items[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<T: Copy + Default, const N: usize> Default for Bank<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Entries beyond the length do not matter.
impl<T: PartialEq, const N: usize> PartialEq for Bank<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

#[cfg(feature = "serde")]
impl<T: serde::Serialize, const N: usize> serde::Serialize for Bank<T, N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.as_slice())
    }
}

#[cfg(feature = "serde")]
impl<'de, T: serde::Deserialize<'de> + Copy + Default, const N: usize> serde::Deserialize<'de>
    for Bank<T, N>
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T, const N: usize>(core::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de> + Copy + Default, const N: usize>
            serde::de::Visitor<'de> for Visitor<T, N>
        {
            type Value = Bank<T, N>;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(f, "a list of at most {N} entries")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut bank = Bank::new();
                while let Some(item) = seq.next_element()? {
                    if bank.push(item).is_none() {
                        return Err(serde::de::Error::invalid_length(N + 1, &self));
                    }
                }
                Ok(bank)
            }
        }
        deserializer.deserialize_seq(Visitor::<T, N>(core::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_list() {
        let mut bank = Bank::<u8, 3>::new();
        assert!(bank.is_empty());
        assert_eq!(bank.push(7), Some(0));
        assert_eq!(bank.push(8), Some(1));
        assert_eq!(bank.push(9), Some(2));
        assert_eq!(bank.push(10), None);
        assert_eq!(bank.as_slice(), &[7, 8, 9]);
        assert_eq!(
            Bank::<u8, 3>::from_slice(&[1, 2, 3, 4]).as_slice(),
            &[1, 2, 3]
        );
        assert_eq!(bank.map(|x| x + 1).as_slice(), &[8, 9, 10]);
        // Equality ignores the unused storage.
        let mut a = Bank::<u8, 3>::from_slice(&[1]);
        let b = Bank::<u8, 3>::from_slice(&[1]);
        a.items[2] = 99;
        assert_eq!(a, b);
    }
}
