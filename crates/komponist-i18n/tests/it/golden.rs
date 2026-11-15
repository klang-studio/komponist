//! Golden tests of the generator: one valid input with its generated source, and one test per
//! build error (file, line, message).
//!
//! `UPDATE_GOLDEN=1 cargo test -p komponist-i18n --test it golden::` rewrites the golden file.

use crate::generator::{generate, locales_from};

fn errors(files: &[(&str, &str, &str)]) -> Vec<String> {
    match generate(&locales_from(files)) {
        Ok(_) => panic!("expected errors"),
        Err(e) => e.iter().map(ToString::to_string).collect(),
    }
}

const DE: &str = "\
## Golden

-brand = komponist

# Source: S01.
hello = Hallo { -brand }

# A count.
count =
    { $n ->
        [0] Nichts
        [one] { $n } Ding
       *[other] { $n } Dinge
    }

named = { $name } ist { $n }
";

const EN: &str = "\
-brand = komponist

hello = Hello { -brand }

count =
    { $n ->
        [0] Nothing
        [one] { $n } thing
       *[other] { $n } things
    }

named = { $name } is { $n }
";

#[test]
fn valid_input_matches_the_golden_file() {
    let code = generate(&locales_from(&[
        ("de", "golden.ftl", DE),
        ("en", "golden.ftl", EN),
    ]))
    .expect("valid");
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/it/golden/basic.rs.golden"
    );
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(path, &code).unwrap();
    }
    let want = std::fs::read_to_string(path).expect("golden file");
    assert_eq!(
        code, want,
        "generated code changed; UPDATE_GOLDEN=1 to accept"
    );
}

#[test]
fn missing_key_in_english() {
    let e = errors(&[
        ("de", "x.ftl", "a = A\nb = B\n"),
        ("en", "x.ftl", "a = A\n"),
    ]);
    assert_eq!(
        e,
        [
            "locales/en/x.ftl: message `b` is missing in `en` (defined in `de` at locales/de/x.ftl:2)"
        ]
    );
}

#[test]
fn key_only_outside_german() {
    let e = errors(&[
        ("de", "x.ftl", "a = A\n"),
        ("en", "x.ftl", "a = A\n\nextra = E\n"),
    ]);
    assert_eq!(
        e,
        [
            "locales/en/x.ftl:3: message `extra` exists in `en` but not in `de` (the source language)"
        ]
    );
}

#[test]
fn variables_differ() {
    let e = errors(&[
        ("de", "x.ftl", "a = { $n } und { $m }\n"),
        ("en", "x.ftl", "# c\na = { $n } and\n"),
    ]);
    assert_eq!(
        e,
        [
            "locales/en/x.ftl:2: message `a` uses variables [$n] in `en` but [$m, $n] in `de` (locales/de/x.ftl:1)"
        ]
    );
}

#[test]
fn static_text_in_one_language_and_variable_in_the_other() {
    let e = errors(&[
        ("de", "x.ftl", "a = Hallo\n"),
        ("en", "x.ftl", "a = Hello { $name }\n"),
    ]);
    assert_eq!(
        e,
        [
            "locales/en/x.ftl:1: message `a` uses variables [$name] in `en` but [none] in `de` (locales/de/x.ftl:1)"
        ]
    );
}

#[test]
fn select_without_other() {
    let e = errors(&[
        (
            "de",
            "x.ftl",
            "ok = Ok\nn =\n    { $n ->\n        [one] eins\n        [few] wenige\n    }\n",
        ),
        ("en", "x.ftl", "ok = Ok\nn = { $n }\n"),
    ]);
    assert!(
        e.iter().any(|l| l.starts_with("locales/de/x.ftl:")
            && l.ends_with("select has no `*[other]` default variant")),
        "{e:?}"
    );
}

#[test]
fn default_variant_must_be_other() {
    let de = "n =\n    { $n ->\n        [other] viele\n       *[one] eins\n    }\n";
    let e = errors(&[("de", "x.ftl", de), ("en", "x.ftl", "n = { $n }\n")]);
    assert_eq!(
        e,
        [
            "locales/de/x.ftl:1: message `n`: select has no `*[other]` default variant",
            "locales/de/x.ftl:1: message `n`: the `other` variant must be the default: write `*[other]`",
            "locales/de/x.ftl:1: message `n`: the default variant must be `*[other]`, not `*[one]`",
        ]
    );
}

#[test]
fn unsupported_constructs() {
    let de = "\
f = { NUMBER($n) }
r = { other }
attr = Wert
    .tooltip = Tipp
num = { 5 }
termargs = { -t(case: \"x\") }
undefined = { -nope }
stringsel =
    { $state ->
        [on] an
       *[other] aus
    }
numsel =
    { 5 ->
        [one] x
       *[other] y
    }
-t = text
";
    let e = errors(&[("de", "x.ftl", de), ("en", "x.ftl", "")]);
    let text = e.join("\n");
    for want in [
        "locales/de/x.ftl:1: message `f`: function NUMBER() is not supported; format values in Rust",
        "locales/de/x.ftl:2: message `r`: message reference { other } is not supported; use a term ({ -name }) or Rust",
        "locales/de/x.ftl:3: message `attr`: attributes (`.name = …`) are not supported; use a separate message",
        "locales/de/x.ftl:5: message `num`: number literal { 5 } is not supported; write the number as text",
        "locales/de/x.ftl:6: message `termargs`: term -t: attributes and arguments are not supported",
        "locales/de/x.ftl:7: message `undefined`: term -nope is not defined in this language",
        "locales/de/x.ftl:8: message `stringsel`: select key [on] is neither a number nor a plural category (zero, one, two, few, many, other)",
        "locales/de/x.ftl:13: message `numsel`: a select needs a variable as selector, like `{ $n -> … }`",
    ] {
        assert!(text.contains(want), "missing: {want}\nin:\n{text}");
    }
}

#[test]
fn term_with_variable_and_term_cycle() {
    let de = "a = { -t }\nb = { -u }\n-t = x { $v }\n-u = { -u }\n";
    let e = errors(&[("de", "x.ftl", de), ("en", "x.ftl", "a = a\nb = b\n")]);
    let text = e.join("\n");
    assert!(
        text.contains("locales/de/x.ftl:3: term -t: terms must be plain text"),
        "{text}"
    );
    assert!(text.contains("term -u refers to itself"), "{text}");
}

#[test]
fn syntax_error_reports_the_line() {
    let e = errors(&[
        ("de", "x.ftl", "a = A\n\nb = { $n\n\nc = C\n"),
        ("en", "x.ftl", "a = A\nb = B\nc = C\n"),
    ]);
    assert!(
        e.iter()
            .any(|l| l.starts_with("locales/de/x.ftl:3: syntax error")),
        "{e:?}"
    );
}

#[test]
fn duplicate_keys_and_clashing_method_names() {
    let e = errors(&[
        (
            "de",
            "a.ftl",
            "x-y = 1\nx_y = 2\ntype = T\nx-y = 3\nby-id = I\n",
        ),
        ("en", "a.ftl", "x-y = 1\n"),
    ]);
    let text = e.join("\n");
    assert!(
        text.contains(
            "locales/de/a.ftl:4: message `x-y`: defined twice (first at locales/de/a.ftl:1)"
        ),
        "{text}"
    );
    assert!(
        text.contains("message `type`: the method name `type` is a Rust keyword"),
        "{text}"
    );
    assert!(
        text.contains("message `by-id`: the method name `by_id` collides with a method of `Lang`"),
        "{text}"
    );
}

#[test]
fn keyword_variable() {
    let e = errors(&[
        ("de", "a.ftl", "a = { $type }\n"),
        ("en", "a.ftl", "a = { $type }\n"),
    ]);
    assert_eq!(
        e,
        ["locales/de/a.ftl:1: message `a`: variable $type is a Rust keyword; rename it"]
    );
}

#[test]
fn source_language_is_required() {
    let e = errors(&[("en", "a.ftl", "a = A\n")]);
    assert_eq!(e, ["locales: the source language folder `de` is missing"]);
}
