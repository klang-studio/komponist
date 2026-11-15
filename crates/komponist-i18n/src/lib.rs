//! The typed text catalog of komponist (`docs/I18N.md`).
//!
//! `build.rs` generates [`Lang`] from `locales/<lang>/*.ftl`: one method per message
//! (`t.dock_devices()` returns `&'static str`, a message with variables returns `String`), the
//! constants `de::DOCK_DEVICES` and `en::DOCK_DEVICES`, and [`Lang::by_id`]. German is the source
//! language and the default; a key missing in another language, different variables or syntax
//! outside the supported subset stop the build with file and line.
//!
//! This file holds what the generator does not write: [`Count`], [`Plural`] and the plural rule
//! of each language ([`Lang::plural`]), the one place a new language needs code.

#![forbid(unsafe_code)]

#[allow(clippy::all, missing_docs, unused_variables)]
mod catalog {
    #[allow(unused_imports)]
    use super::{Count, Plural};
    include!(concat!(env!("OUT_DIR"), "/catalog.rs"));
}

pub use catalog::{Lang, de, en};

/// The CLDR plural categories a Fluent select can name (`[one]`, `[few]`, …). `other` is the
/// default variant and has no entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plural {
    /// `zero`.
    Zero,
    /// `one`.
    One,
    /// `two`.
    Two,
    /// `few`.
    Few,
    /// `many`.
    Many,
    /// `other`.
    Other,
}

impl Plural {
    /// The rule of German and English (CLDR: `one` for 1, `other` for the rest) for integers.
    pub fn germanic(n: i64) -> Plural {
        if n.unsigned_abs() == 1 {
            Plural::One
        } else {
            Plural::Other
        }
    }
}

/// A number a plural select can switch on (`{ $n -> … }`).
pub trait Count {
    /// The value as the integer the plural rules and number keys (`[0]`) see; values beyond
    /// `i64` saturate.
    fn count(&self) -> i64;
}

macro_rules! count_via_try_from {
    ($($t:ty),*) => {$(
        impl Count for $t {
            fn count(&self) -> i64 {
                i64::try_from(*self).unwrap_or(if *self > 0 { i64::MAX } else { i64::MIN })
            }
        }
    )*};
}
count_via_try_from!(usize, u8, u16, u32, u64, isize, i8, i16, i32, i64);

impl Lang {
    /// The plural category of `n` in this language.
    ///
    /// **A new language adds its CLDR rule here** (the `match` stops compiling until it does):
    /// either by hand for a simple rule or through `icu_plurals`
    /// (<https://unicode.org/cldr/charts/latest/supplemental/language_plural_rules.html>).
    /// The generator already emits `[zero]`, `[one]`, `[two]`, `[few]` and `[many]` arms.
    pub fn plural(self, n: i64) -> Plural {
        match self {
            Lang::De | Lang::En => Plural::germanic(n),
        }
    }

    /// The language for a code or a system locale (`de`, `de-DE`, `de_DE.UTF-8`, `en_US`);
    /// `None` for a language komponist has no catalog for (`fr`, `C`).
    pub fn from_code(code: &str) -> Option<Lang> {
        let normal = code
            .split(['.', '@'])
            .next()
            .unwrap_or("")
            .replace('_', "-")
            .to_ascii_lowercase();
        let primary = normal.split('-').next().unwrap_or("");
        Lang::ALL
            .into_iter()
            .find(|l| l.code() == normal)
            .or_else(|| Lang::ALL.into_iter().find(|l| l.code() == primary))
    }
}
