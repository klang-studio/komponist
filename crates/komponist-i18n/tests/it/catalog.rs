//! The generated API, tried on the fixture catalog (`tests/fixtures/locales`) and the real one.

use komponist_i18n::{Count, Lang, Plural, de, en};

/// The fixture catalog, compiled from `tests/fixtures/locales` by the build script.
mod fixture {
    #![allow(clippy::all, missing_docs, dead_code, unused)]
    use komponist_i18n::{Count, Plural};

    impl Lang {
        fn plural(self, n: i64) -> Plural {
            Plural::germanic(n)
        }
    }

    include!(concat!(env!("OUT_DIR"), "/fixture_catalog.rs"));
}

use fixture::Lang as F;

#[test]
fn static_texts_and_constants() {
    assert_eq!(F::De.plain(), "Hallo Welt");
    assert_eq!(F::En.plain(), "Hello World");
    assert_eq!(fixture::de::PLAIN, "Hallo Welt");
    assert_eq!(fixture::en::PLAIN, "Hello World");
    assert_eq!(F::De.quotes(), "„Nachtfahrt“ · −3.2 dB");
    assert_eq!(F::En.quotes(), "“Nachtfahrt” · −3.2 dB");
}

#[test]
fn terms_and_string_literals_are_substituted_at_build_time() {
    assert_eq!(F::De.term(), "komponist startet {jetzt}");
    assert_eq!(F::En.term(), "komponist starts {now}");
}

#[test]
fn variables() {
    assert_eq!(F::De.greeting("Erik"), "Hallo Erik");
    assert_eq!(F::En.greeting(String::from("Erik")), "Hello Erik");
}

#[test]
fn plurals_with_number_keys() {
    assert_eq!(F::De.clips(0usize), "Keine Clips");
    assert_eq!(F::De.clips(1u32), "Ein Clip");
    assert_eq!(F::De.clips(2usize), "2 Clips");
    assert_eq!(F::En.clips(0usize), "No clips");
    assert_eq!(F::En.clips(1usize), "One clip");
    assert_eq!(F::En.clips(12i64), "12 clips");
}

#[test]
fn variable_order_follows_german() {
    assert_eq!(F::De.takes("Spur 1", 1usize), "Spur 1: ein Take");
    assert_eq!(F::En.takes("Track 1", 3usize), "Track 1: 3 takes");
}

#[test]
fn nested_selects() {
    assert_eq!(F::De.nested(1usize, 1usize), "eins, x");
    assert_eq!(F::De.nested(1usize, 5usize), "eins, y");
    assert_eq!(F::En.nested(7usize, 1usize), "many");
}

#[test]
fn by_id_resolves_static_messages_only() {
    assert_eq!(F::De.by_id("undo-duplicate"), Some("Clips duplizieren"));
    assert_eq!(F::En.by_id("undo-duplicate"), Some("Duplicate Clips"));
    assert_eq!(F::En.by_id("greeting"), None);
    assert_eq!(F::En.by_id("nope"), None);
}

#[test]
fn lang_codes() {
    assert_eq!(Lang::default(), Lang::De);
    assert_eq!(Lang::ALL, [Lang::De, Lang::En]);
    assert_eq!(Lang::De.code(), "de");
    assert_eq!(Lang::En.code(), "en");
    for (code, lang) in [
        ("de", Some(Lang::De)),
        ("de_DE.UTF-8", Some(Lang::De)),
        ("de-AT", Some(Lang::De)),
        ("en_US", Some(Lang::En)),
        ("EN", Some(Lang::En)),
        ("fr_FR", None),
        ("C", None),
        ("", None),
    ] {
        assert_eq!(Lang::from_code(code), lang, "{code}");
    }
}

#[test]
fn plural_rule_of_german_and_english() {
    for lang in Lang::ALL {
        assert_eq!(lang.plural(1), Plural::One);
        assert_eq!(lang.plural(-1), Plural::One);
        assert_eq!(lang.plural(0), Plural::Other);
        assert_eq!(lang.plural(2), Plural::Other);
        assert_eq!(lang.plural(1000), Plural::Other);
    }
}

#[test]
fn count_saturates() {
    assert_eq!(5usize.count(), 5);
    assert_eq!(u64::MAX.count(), i64::MAX);
    assert_eq!((-3i32).count(), -3);
}

#[test]
fn real_catalog_german_and_english() {
    assert_eq!(Lang::De.dock_devices(), "Geräte");
    assert_eq!(Lang::En.dock_devices(), "Devices");
    assert_eq!(de::DOCK_COLLAPSE, "Dock einklappen");
    assert_eq!(en::DOCK_COLLAPSE, "Collapse dock");
    assert_eq!(Lang::De.by_id("dock-mixer"), Some("Mixer"));
}
