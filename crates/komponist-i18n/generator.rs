//! The catalog generator (`docs/I18N.md`, `docs/I18N-EVALUATION.md` §4.4).
//!
//! Shared by `build.rs` and the golden tests (`tests/it/golden.rs`): it takes the text of every
//! `.ftl` file of every language and returns the Rust source of the catalog, or every error with
//! file and line. It does no file IO itself.
//!
//! Supported Fluent subset: text, `{ $var }`, string literals `{ "…" }`, terms `{ -term }` (plain
//! text, substituted at build time) and plural selects `{ $n -> [one] … *[other] … }` with number
//! (`[0]`) and CLDR category keys. Everything else is an error.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{self, Write as _};

use fluent_syntax::ast::{
    Entry, Expression, InlineExpression, Pattern, PatternElement, VariantKey,
};
use fluent_syntax::parser::{self, ErrorKind};

/// The source language: every other language must have exactly its messages and variables.
pub const SOURCE_LANG: &str = "de";

/// One `.ftl` file.
pub struct SourceFile {
    /// Path shown in errors, e.g. `locales/de/dock.ftl`.
    pub path: String,
    /// The file's text.
    pub text: String,
}

/// All files of one language folder.
pub struct Locale {
    /// Folder name, e.g. `de`, `en`, `pt-br`.
    pub code: String,
    /// The `.ftl` files, in a stable order.
    pub files: Vec<SourceFile>,
}

/// A build error: `path:line: message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// File the error is reported at.
    pub path: String,
    /// 1-based line, 0 when the whole file or folder is meant.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}: {}", self.path, self.message)
        } else {
            write!(f, "{}:{}: {}", self.path, self.line, self.message)
        }
    }
}

const CATEGORIES: [&str; 5] = ["zero", "one", "two", "few", "many"];

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// Method names that `Lang` already has.
const RESERVED_METHODS: &[&str] = &[
    "by_id",
    "code",
    "from_code",
    "plural",
    "clone",
    "clone_from",
    "eq",
    "ne",
    "hash",
    "fmt",
    "default",
    "into",
    "from",
    "try_from",
    "try_into",
    "borrow",
    "borrow_mut",
    "type_id",
    "to_owned",
];

// ------------------------------------------------------------------------------------------------
// Model
// ------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Var(String),
    Select(Select),
}

#[derive(Debug, Clone, PartialEq)]
struct Select {
    var: String,
    /// In source order; the `other` variant is separate.
    variants: Vec<(Key, Vec<Piece>)>,
    /// The default variant (`*[other]` of a plural select, any `*[key]` of a text select).
    other: Vec<Piece>,
    /// Key of the default variant.
    default_key: String,
    /// A select on a text value (`[on]`, `*[off]`) instead of a number.
    text: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Key {
    Number(i64),
    Category(&'static str),
    Text(String),
}

struct Msg {
    key: String,
    path: String,
    line: usize,
    comment: Vec<String>,
    pieces: Vec<Piece>,
    /// Variables in order of first appearance.
    vars: Vec<String>,
    /// Variables used as a plural selector.
    counts: BTreeSet<String>,
    /// Variables used as a text selector.
    strings: BTreeSet<String>,
    /// `var[key,key]*default` of every text select, sorted: must be equal in all languages.
    text_shapes: Vec<String>,
}

#[derive(Clone, Copy)]
struct TermDef<'a> {
    path: &'a str,
    line: usize,
    pattern: &'a Pattern<&'a str>,
}

#[derive(Clone)]
struct Site {
    path: String,
    line: usize,
    what: String,
}

struct Ctx<'a> {
    terms: HashMap<String, TermDef<'a>>,
    resolved: HashMap<String, Option<String>>,
    resolving: Vec<String>,
    errors: Vec<Error>,
}

impl Ctx<'_> {
    fn err(&mut self, site: &Site, message: impl Into<String>) {
        self.errors.push(Error {
            path: site.path.clone(),
            line: site.line,
            message: format!("{}: {}", site.what, message.into()),
        });
    }
}

// ------------------------------------------------------------------------------------------------
// Parsing
// ------------------------------------------------------------------------------------------------

fn line_of(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// Line of the `nth` (from 0) entry `id` (`name = …`, or `-name = …` for terms); Fluent's AST
/// has no spans.
fn entry_line(text: &str, id: &str, nth: usize) -> usize {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            l.strip_prefix(id)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .nth(nth)
        .map_or(0, |(i, _)| i + 1)
}

fn unescape(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(u @ ('u' | 'U')) => {
                let n = if u == 'u' { 4 } else { 6 };
                let hex: String = chars.by_ref().take(n).collect();
                let ch = u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("invalid escape \\{u}{hex}"))?;
                out.push(ch);
            }
            other => return Err(format!("invalid escape \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

fn push_text(pieces: &mut Vec<Piece>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Piece::Text(last)) = pieces.last_mut() {
        last.push_str(text);
    } else {
        pieces.push(Piece::Text(text.to_owned()));
    }
}

fn extend_pieces(into: &mut Vec<Piece>, more: Vec<Piece>) {
    for p in more {
        match p {
            Piece::Text(t) => push_text(into, &t),
            other => into.push(other),
        }
    }
}

fn lower_pattern<'a>(ctx: &mut Ctx<'a>, site: &Site, pattern: &Pattern<&'a str>) -> Vec<Piece> {
    let mut out = Vec::new();
    for el in &pattern.elements {
        match el {
            PatternElement::TextElement { value } => push_text(&mut out, value),
            PatternElement::Placeable { expression } => {
                let pieces = lower_expression(ctx, site, expression);
                extend_pieces(&mut out, pieces);
            }
        }
    }
    out
}

fn lower_expression<'a>(
    ctx: &mut Ctx<'a>,
    site: &Site,
    expression: &Expression<&'a str>,
) -> Vec<Piece> {
    match expression {
        Expression::Inline(inline) => lower_inline(ctx, site, inline),
        Expression::Select { selector, variants } => {
            let InlineExpression::VariableReference { id } = selector else {
                ctx.err(
                    site,
                    "a select needs a variable as selector, like `{ $n -> … }`",
                );
                return Vec::new();
            };
            let is_text = variants.iter().any(|v| {
                matches!(&v.key, VariantKey::Identifier { name }
                    if *name != "other" && !CATEGORIES.contains(name))
            });
            if is_text {
                return lower_text_select(ctx, site, id.name, variants);
            }
            let mut keyed = Vec::new();
            let mut other = None;
            let mut seen: Vec<String> = Vec::new();
            for v in variants {
                let (label, key) = match &v.key {
                    VariantKey::NumberLiteral { value } => {
                        match value.parse::<i64>() {
                            Ok(n) => (value.to_string(), Some(Key::Number(n))),
                            Err(_) => {
                                ctx.err(
                                site,
                                format!("select key [{value}]: only integer number keys are supported"),
                            );
                                continue;
                            }
                        }
                    }
                    VariantKey::Identifier { name } => {
                        if *name == "other" {
                            (name.to_string(), None)
                        } else if let Some(c) = CATEGORIES.iter().find(|c| **c == *name) {
                            (name.to_string(), Some(Key::Category(c)))
                        } else {
                            ctx.err(
                                site,
                                format!(
                                    "select key [{name}] is neither a number nor a plural category \
                                     (zero, one, two, few, many, other)"
                                ),
                            );
                            continue;
                        }
                    }
                };
                if seen.contains(&label) {
                    ctx.err(site, format!("select key [{label}] appears twice"));
                    continue;
                }
                seen.push(label.clone());
                match (key, v.default) {
                    (None, true) => other = Some(lower_pattern(ctx, site, &v.value)),
                    (None, false) => ctx.err(
                        site,
                        "the `other` variant must be the default: write `*[other]`",
                    ),
                    (Some(_), true) => ctx.err(
                        site,
                        format!("the default variant must be `*[other]`, not `*[{label}]`"),
                    ),
                    (Some(k), false) => keyed.push((k, lower_pattern(ctx, site, &v.value))),
                }
            }
            let Some(other) = other else {
                // The parser already rejects a select without a default; this covers `*[one]`.
                ctx.err(site, "select has no `*[other]` default variant");
                return Vec::new();
            };
            vec![Piece::Select(Select {
                var: id.name.to_owned(),
                variants: keyed,
                other,
                default_key: "other".to_owned(),
                text: false,
            })]
        }
    }
}

/// `{ $state -> [on] an *[off] aus }`: identifier keys that are not plural categories. Any key
/// may be the default (it is the `_` arm); number keys cannot be mixed in.
fn lower_text_select<'a>(
    ctx: &mut Ctx<'a>,
    site: &Site,
    var: &str,
    variants: &[fluent_syntax::ast::Variant<&'a str>],
) -> Vec<Piece> {
    let mut keyed = Vec::new();
    let mut default = None;
    let mut seen: Vec<String> = Vec::new();
    for v in variants {
        let VariantKey::Identifier { name } = &v.key else {
            ctx.err(
                site,
                "a select on text values cannot have number keys; use identifiers only",
            );
            continue;
        };
        if seen.iter().any(|k| k == name) {
            ctx.err(site, format!("select key [{name}] appears twice"));
            continue;
        }
        seen.push(name.to_string());
        let body = lower_pattern(ctx, site, &v.value);
        if v.default {
            default = Some((name.to_string(), body));
        } else {
            keyed.push((Key::Text(name.to_string()), body));
        }
    }
    let Some((default_key, other)) = default else {
        ctx.err(site, "select has no `*[…]` default variant");
        return Vec::new();
    };
    vec![Piece::Select(Select {
        var: var.to_owned(),
        variants: keyed,
        other,
        default_key,
        text: true,
    })]
}

fn lower_inline<'a>(
    ctx: &mut Ctx<'a>,
    site: &Site,
    inline: &InlineExpression<&'a str>,
) -> Vec<Piece> {
    match inline {
        InlineExpression::StringLiteral { value } => match unescape(value) {
            Ok(s) => vec![Piece::Text(s)],
            Err(e) => {
                ctx.err(site, e);
                Vec::new()
            }
        },
        InlineExpression::VariableReference { id } => vec![Piece::Var(id.name.to_owned())],
        InlineExpression::TermReference {
            id,
            attribute,
            arguments,
        } => {
            if attribute.is_some() || arguments.is_some() {
                ctx.err(
                    site,
                    format!(
                        "term -{}: attributes and arguments are not supported",
                        id.name
                    ),
                );
                return Vec::new();
            }
            match term_text(ctx, site, id.name) {
                Some(t) => vec![Piece::Text(t)],
                None => Vec::new(),
            }
        }
        InlineExpression::Placeable { expression } => lower_expression(ctx, site, expression),
        InlineExpression::NumberLiteral { value } => {
            ctx.err(
                site,
                format!("number literal {{ {value} }} is not supported; write the number as text"),
            );
            Vec::new()
        }
        InlineExpression::FunctionReference { id, .. } => {
            ctx.err(
                site,
                format!(
                    "function {}() is not supported; format values in Rust",
                    id.name
                ),
            );
            Vec::new()
        }
        InlineExpression::MessageReference { id, .. } => {
            ctx.err(
                site,
                format!(
                    "message reference {{ {} }} is not supported; use a term ({{ -name }}) or Rust",
                    id.name
                ),
            );
            Vec::new()
        }
    }
}

/// The text of term `-name`, substituted at build time.
fn term_text<'a>(ctx: &mut Ctx<'a>, site: &Site, name: &str) -> Option<String> {
    if let Some(done) = ctx.resolved.get(name) {
        return done.clone();
    }
    let Some(def) = ctx.terms.get(name).copied() else {
        ctx.err(
            site,
            format!("term -{name} is not defined in this language"),
        );
        return None;
    };
    if ctx.resolving.iter().any(|n| n == name) {
        ctx.err(site, format!("term -{name} refers to itself"));
        return None;
    }
    ctx.resolving.push(name.to_owned());
    let term_site = Site {
        path: def.path.to_owned(),
        line: def.line,
        what: format!("term -{name}"),
    };
    let pieces = lower_pattern(ctx, &term_site, def.pattern);
    ctx.resolving.pop();
    let text = match pieces.as_slice() {
        [] => Some(String::new()),
        [Piece::Text(t)] => Some(t.clone()),
        _ => {
            ctx.err(
                &term_site,
                "terms must be plain text (no variables or selects)",
            );
            None
        }
    };
    ctx.resolved.insert(name.to_owned(), text.clone());
    text
}

fn collect_vars(
    pieces: &[Piece],
    vars: &mut Vec<String>,
    counts: &mut BTreeSet<String>,
    strings: &mut BTreeSet<String>,
    shapes: &mut Vec<String>,
) {
    for p in pieces {
        match p {
            Piece::Text(_) => {}
            Piece::Var(v) => {
                if !vars.contains(v) {
                    vars.push(v.clone());
                }
            }
            Piece::Select(s) => {
                if !vars.contains(&s.var) {
                    vars.push(s.var.clone());
                }
                if s.text {
                    strings.insert(s.var.clone());
                    let mut keys: Vec<String> = s
                        .variants
                        .iter()
                        .map(|(k, _)| match k {
                            Key::Text(t) => t.clone(),
                            _ => String::new(),
                        })
                        .collect();
                    keys.sort();
                    shapes.push(format!("${}[{}]*{}", s.var, keys.join(","), s.default_key));
                } else {
                    counts.insert(s.var.clone());
                }
                for (_, body) in &s.variants {
                    collect_vars(body, vars, counts, strings, shapes);
                }
                collect_vars(&s.other, vars, counts, strings, shapes);
            }
        }
    }
}

/// Parses one language: its messages by key, in file order of the sources.
fn parse_locale(locale: &Locale, errors: &mut Vec<Error>) -> Vec<Msg> {
    let mut parsed = Vec::new();
    for file in &locale.files {
        let resource = match parser::parse(file.text.as_str()) {
            Ok(r) => r,
            Err((r, errs)) => {
                for e in errs {
                    let message = match e.kind {
                        ErrorKind::MissingDefaultVariant => {
                            "select has no `*[other]` default variant".to_owned()
                        }
                        ref kind => {
                            format!("syntax error: {}", kind.to_string().replace('\n', "\\n"))
                        }
                    };
                    errors.push(Error {
                        path: file.path.clone(),
                        line: line_of(
                            &file.text,
                            e.slice.as_ref().map_or(e.pos.start, |s| s.start),
                        ),
                        message,
                    });
                }
                r
            }
        };
        parsed.push((file, resource));
    }
    let mut ctx = Ctx {
        terms: HashMap::new(),
        resolved: HashMap::new(),
        resolving: Vec::new(),
        errors: Vec::new(),
    };
    let mut seen_lines: HashMap<(String, String), usize> = HashMap::new();
    for (file, resource) in &parsed {
        for entry in &resource.body {
            if let Entry::Term(t) = entry {
                let id = format!("-{}", t.id.name);
                let nth = seen_lines
                    .entry((file.path.clone(), id.clone()))
                    .or_default();
                let line = entry_line(&file.text, &id, *nth);
                *nth += 1;
                let def = TermDef {
                    path: &file.path,
                    line,
                    pattern: &t.value,
                };
                if let Some(first) = ctx.terms.insert(t.id.name.to_owned(), def) {
                    ctx.errors.push(Error {
                        path: file.path.clone(),
                        line,
                        message: format!(
                            "term -{} is defined twice (first at {}:{})",
                            t.id.name, first.path, first.line
                        ),
                    });
                }
            }
        }
    }
    let mut msgs: Vec<Msg> = Vec::new();
    for (file, resource) in &parsed {
        for entry in &resource.body {
            let Entry::Message(m) = entry else { continue };
            let key = m.id.name.to_owned();
            let nth = seen_lines
                .entry((file.path.clone(), key.clone()))
                .or_default();
            let line = entry_line(&file.text, &key, *nth);
            *nth += 1;
            let site = Site {
                path: file.path.clone(),
                line,
                what: format!("message `{key}`"),
            };
            if let Some(first) = msgs.iter().find(|x| x.key == key) {
                ctx.err(
                    &site,
                    format!("defined twice (first at {}:{})", first.path, first.line),
                );
                continue;
            }
            if !m.attributes.is_empty() {
                ctx.err(
                    &site,
                    "attributes (`.name = …`) are not supported; use a separate message",
                );
            }
            let Some(value) = &m.value else {
                ctx.err(&site, "has no value");
                continue;
            };
            let pieces = lower_pattern(&mut ctx, &site, value);
            let mut vars = Vec::new();
            let mut counts = BTreeSet::new();
            let mut strings = BTreeSet::new();
            let mut text_shapes = Vec::new();
            collect_vars(
                &pieces,
                &mut vars,
                &mut counts,
                &mut strings,
                &mut text_shapes,
            );
            text_shapes.sort();
            if let Some(v) = counts.intersection(&strings).next() {
                ctx.err(
                    &site,
                    format!("${v} is used as a number and as text selector"),
                );
            }
            lint_pieces(&mut ctx, &site, &locale.code, &pieces);
            msgs.push(Msg {
                key,
                path: file.path.clone(),
                line,
                comment: m
                    .comment
                    .as_ref()
                    .map(|c| c.content.iter().map(|l| l.to_string()).collect())
                    .unwrap_or_default(),
                pieces,
                vars,
                counts,
                strings,
                text_shapes,
            });
        }
    }
    errors.append(&mut ctx.errors);
    msgs
}

// ------------------------------------------------------------------------------------------------
// Checks
// ------------------------------------------------------------------------------------------------

/// Typography lints on the text of a message (after terms and literals are substituted).
fn lint_pieces(ctx: &mut Ctx<'_>, site: &Site, code: &str, pieces: &[Piece]) {
    for p in pieces {
        match p {
            Piece::Text(t) => {
                for problem in lint_text(code, t) {
                    ctx.err(site, problem);
                }
            }
            Piece::Var(_) => {}
            Piece::Select(sel) => {
                for (_, body) in &sel.variants {
                    lint_pieces(ctx, site, code, body);
                }
                lint_pieces(ctx, site, code, &sel.other);
            }
        }
    }
}

/// What is wrong with `text` typographically (docs/I18N.md §6):
/// - ASCII `...` (write U+2026) in every language;
/// - `„` (U+201E, German opening quote) or the word "Strg" in `en`;
/// - an ASCII hyphen-minus that works as a minus sign, directly before a digit and not after a
///   letter or digit (` -3`, `(-3`): write U+2212. Compounds and ranges like `24-Bit` or `1-4`
///   have a letter or digit before the hyphen and pass.
fn lint_text(code: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    if text.contains("...") {
        out.push("ASCII \"...\" in text: write the ellipsis U+2026 (…)".to_owned());
    }
    if code == "en" {
        if text.contains('„') {
            out.push("„ (U+201E) in English text: use “…” (U+201C, U+201D)".to_owned());
        }
        if text.contains("Strg") {
            out.push("\"Strg\" in English text: the key is called Ctrl".to_owned());
        }
    }
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '-'
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
            && !(i > 0 && chars[i - 1].is_alphanumeric())
        {
            out.push("ASCII hyphen before a digit: write the minus sign U+2212 (−)".to_owned());
            break;
        }
    }
    out
}

fn method_name(key: &str) -> String {
    key.replace('-', "_")
}

fn check_identifiers(msgs: &[Msg], errors: &mut Vec<Error>) {
    let mut seen: HashMap<String, &Msg> = HashMap::new();
    for m in msgs {
        let name = method_name(&m.key);
        let bad = if KEYWORDS.contains(&name.as_str()) {
            Some("is a Rust keyword")
        } else if RESERVED_METHODS.contains(&name.as_str()) {
            Some("collides with a method of `Lang`")
        } else {
            None
        };
        if let Some(why) = bad {
            errors.push(Error {
                path: m.path.clone(),
                line: m.line,
                message: format!(
                    "message `{}`: the method name `{name}` {why}; rename the key",
                    m.key
                ),
            });
        }
        if let Some(first) = seen.insert(name.clone(), m) {
            errors.push(Error {
                path: m.path.clone(),
                line: m.line,
                message: format!(
                    "message `{}`: the method name `{name}` is already taken by `{}` ({}:{})",
                    m.key, first.key, first.path, first.line
                ),
            });
        }
        for v in &m.vars {
            let var = v.replace('-', "_");
            if KEYWORDS.contains(&var.as_str()) {
                errors.push(Error {
                    path: m.path.clone(),
                    line: m.line,
                    message: format!(
                        "message `{}`: variable ${v} is a Rust keyword; rename it",
                        m.key
                    ),
                });
            }
        }
    }
}

fn var_set(m: &Msg) -> String {
    let mut v: Vec<&str> = m.vars.iter().map(String::as_str).collect();
    v.sort_unstable();
    if v.is_empty() {
        "none".to_owned()
    } else {
        v.iter()
            .map(|x| format!("${x}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn check_languages(locales: &[&Locale], parsed: &[(String, Vec<Msg>)], errors: &mut Vec<Error>) {
    let (_, source) = &parsed[0];
    for (i, (code, msgs)) in parsed.iter().enumerate().skip(1) {
        for s in source {
            match msgs.iter().find(|m| m.key == s.key) {
                None => {
                    // Point at the file the German message lives in, with the same name here.
                    let file = s.path.rsplit('/').next().unwrap_or(&s.path);
                    let path = locales[i]
                        .files
                        .iter()
                        .map(|f| f.path.as_str())
                        .find(|p| p.rsplit('/').next() == Some(file))
                        .map_or_else(|| format!("locales/{code}/{file}"), str::to_owned);
                    errors.push(Error {
                        path,
                        line: 0,
                        message: format!(
                            "message `{}` is missing in `{code}` (defined in `{SOURCE_LANG}` at {}:{})",
                            s.key, s.path, s.line
                        ),
                    });
                }
                Some(m) => {
                    if var_set(m) == var_set(s) && m.text_shapes != s.text_shapes {
                        errors.push(Error {
                            path: m.path.clone(),
                            line: m.line,
                            message: format!(
                                "message `{}` selects on text differently in `{code}` ([{}]) and `{SOURCE_LANG}` ([{}], {}:{}); \
                                 same variants and the same default are required",
                                m.key,
                                m.text_shapes.join("; "),
                                s.text_shapes.join("; "),
                                s.path,
                                s.line
                            ),
                        });
                    } else if var_set(m) != var_set(s) {
                        errors.push(Error {
                            path: m.path.clone(),
                            line: m.line,
                            message: format!(
                                "message `{}` uses variables [{}] in `{code}` but [{}] in `{SOURCE_LANG}` ({}:{})",
                                m.key,
                                var_set(m),
                                var_set(s),
                                s.path,
                                s.line
                            ),
                        });
                    }
                }
            }
        }
        for m in msgs {
            if !source.iter().any(|s| s.key == m.key) {
                errors.push(Error {
                    path: m.path.clone(),
                    line: m.line,
                    message: format!(
                        "message `{}` exists in `{code}` but not in `{SOURCE_LANG}` (the source language)",
                        m.key
                    ),
                });
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Emission
// ------------------------------------------------------------------------------------------------

fn lit(s: &str) -> String {
    format!("{s:?}")
}

fn variant_name(code: &str) -> String {
    code.split('-')
        .map(|part| {
            let mut c = part.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn module_name(code: &str) -> String {
    code.replace('-', "_")
}

fn ident(var: &str) -> String {
    var.replace('-', "_")
}

fn category_variant(c: &str) -> &'static str {
    match c {
        "zero" => "Zero",
        "one" => "One",
        "two" => "Two",
        "few" => "Few",
        _ => "Many",
    }
}

fn is_static(m: &Msg) -> bool {
    m.vars.is_empty()
}

fn static_text(m: &Msg) -> String {
    match m.pieces.as_slice() {
        [Piece::Text(t)] => t.clone(),
        _ => String::new(),
    }
}

/// `{ $n -> [one] … *[other] … }`-style preview for rustdoc.
fn preview(pieces: &[Piece]) -> String {
    let mut s = String::new();
    for p in pieces {
        match p {
            Piece::Text(t) => s.push_str(t),
            Piece::Var(v) => {
                let _ = write!(s, "{{ ${v} }}");
            }
            Piece::Select(sel) => {
                let _ = write!(s, "{{ ${} ->", sel.var);
                for (k, body) in &sel.variants {
                    match k {
                        Key::Number(n) => {
                            let _ = write!(s, " [{n}] {}", preview(body));
                        }
                        Key::Category(c) => {
                            let _ = write!(s, " [{c}] {}", preview(body));
                        }
                        Key::Text(c) => {
                            let _ = write!(s, " [{c}] {}", preview(body));
                        }
                    }
                }
                let _ = write!(s, " *[{}] {} }}", sel.default_key, preview(&sel.other));
            }
        }
    }
    s
}

fn code_span(s: &str) -> String {
    if s.contains('`') {
        format!("`` {s} ``")
    } else {
        format!("`{s}`")
    }
}

fn emit_pieces(out: &mut String, pieces: &[Piece], depth: usize) {
    let pad = "    ".repeat(depth);
    for p in pieces {
        match p {
            Piece::Text(t) => {
                let _ = writeln!(out, "{pad}__out.push_str({});", lit(t));
            }
            Piece::Var(v) => {
                let _ = writeln!(out, "{pad}let _ = write!(__out, \"{{}}\", {});", ident(v));
            }
            Piece::Select(s) => emit_select(out, s, depth),
        }
    }
}

fn emit_select(out: &mut String, s: &Select, depth: usize) {
    let pad = "    ".repeat(depth);
    let var = ident(&s.var);
    if s.text {
        let _ = writeln!(out, "{pad}match {var} {{");
        for (k, body) in &s.variants {
            if let Key::Text(k) = k {
                let _ = writeln!(out, "{pad}    {} => {{", lit(k));
                emit_pieces(out, body, depth + 2);
                let _ = writeln!(out, "{pad}    }}");
            }
        }
        let _ = writeln!(out, "{pad}    _ => {{");
        emit_pieces(out, &s.other, depth + 2);
        let _ = writeln!(out, "{pad}    }}");
        let _ = writeln!(out, "{pad}}}");
        return;
    }
    let numbers: Vec<_> = s
        .variants
        .iter()
        .filter_map(|(k, b)| match k {
            Key::Number(n) => Some((n, b)),
            _ => None,
        })
        .collect();
    let cats: Vec<_> = s
        .variants
        .iter()
        .filter_map(|(k, b)| match k {
            Key::Category(c) => Some((*c, b)),
            _ => None,
        })
        .collect();
    let inner_depth = if numbers.is_empty() { depth } else { depth + 2 };
    if !numbers.is_empty() {
        let _ = writeln!(out, "{pad}match {var} {{");
        for (n, body) in &numbers {
            let _ = writeln!(out, "{pad}    {n} => {{");
            emit_pieces(out, body, depth + 2);
            let _ = writeln!(out, "{pad}    }}");
        }
        let _ = writeln!(out, "{pad}    _ => {{");
    }
    let ipad = "    ".repeat(inner_depth);
    if cats.is_empty() {
        emit_pieces(out, &s.other, inner_depth);
    } else {
        let _ = writeln!(out, "{ipad}match self.plural({var}) {{");
        for (c, body) in &cats {
            let _ = writeln!(out, "{ipad}    Plural::{} => {{", category_variant(c));
            emit_pieces(out, body, inner_depth + 2);
            let _ = writeln!(out, "{ipad}    }}");
        }
        let _ = writeln!(out, "{ipad}    _ => {{");
        emit_pieces(out, &s.other, inner_depth + 2);
        let _ = writeln!(out, "{ipad}    }}");
        let _ = writeln!(out, "{ipad}}}");
    }
    if !numbers.is_empty() {
        let _ = writeln!(out, "{pad}    }}");
        let _ = writeln!(out, "{pad}}}");
    }
}

fn text_len(pieces: &[Piece]) -> usize {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) => t.len(),
            Piece::Var(_) => 8,
            Piece::Select(s) => s
                .variants
                .iter()
                .map(|(_, b)| text_len(b))
                .chain([text_len(&s.other)])
                .max()
                .unwrap_or(0),
        })
        .sum()
}

fn emit(parsed: &[(String, Vec<Msg>)]) -> String {
    let mut out = String::new();
    out.push_str("// @generated by komponist-i18n/build.rs from locales/*/*.ftl; do not edit.\n\n");
    // Lang
    let codes: Vec<&str> = parsed.iter().map(|(c, _)| c.as_str()).collect();
    out.push_str("/// A UI language. German is the source language and the default.\n");
    out.push_str("#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]\npub enum Lang {\n");
    for (i, c) in codes.iter().enumerate() {
        if i == 0 {
            out.push_str("    #[default]\n");
        }
        let _ = writeln!(
            out,
            "    /// Folder `locales/{c}`.\n    {},",
            variant_name(c)
        );
    }
    out.push_str("}\n\nimpl Lang {\n");
    let _ = writeln!(
        out,
        "    /// Every language, German first.\n    pub const ALL: [Lang; {}] = [{}];\n",
        codes.len(),
        codes
            .iter()
            .map(|c| format!("Lang::{}", variant_name(c)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    out.push_str("    /// The language code, the name of its `locales` folder (`de`, `en`).\n");
    out.push_str("    pub const fn code(self) -> &'static str {\n        match self {\n");
    for c in &codes {
        let _ = writeln!(out, "            Lang::{} => {},", variant_name(c), lit(c));
    }
    out.push_str("        }\n    }\n");

    let (_, source) = &parsed[0];
    for s in source {
        let name = method_name(&s.key);
        let per_lang: Vec<&Msg> = parsed
            .iter()
            .map(|(_, msgs)| msgs.iter().find(|m| m.key == s.key).expect("checked"))
            .collect();
        out.push('\n');
        if !s.comment.is_empty() {
            for l in &s.comment {
                let _ = writeln!(out, "    #[doc = {}]", lit(&format!(" {l}")));
            }
            out.push_str("    #[doc = \"\"]\n");
        }
        for ((code, _), m) in parsed.iter().zip(&per_lang) {
            let _ = writeln!(
                out,
                "    #[doc = {}]",
                lit(&format!(" - {code}: {}", code_span(&preview(&m.pieces))))
            );
        }
        if is_static(s) {
            let _ = writeln!(
                out,
                "    #[inline]\n    pub fn {name}(self) -> &'static str {{\n        match self {{"
            );
            for ((code, _), m) in parsed.iter().zip(&per_lang) {
                let _ = writeln!(
                    out,
                    "            Lang::{} => {},",
                    variant_name(code),
                    lit(&static_text(m))
                );
            }
            out.push_str("        }\n    }\n");
        } else {
            let params: Vec<String> = s
                .vars
                .iter()
                .map(|v| {
                    let ty = if s.counts.contains(v) {
                        "Count"
                    } else if s.strings.contains(v) {
                        "AsRef<str>"
                    } else {
                        "::core::fmt::Display"
                    };
                    format!("{}: impl {ty}", ident(v))
                })
                .collect();
            let cap = per_lang
                .iter()
                .map(|m| text_len(&m.pieces))
                .max()
                .unwrap_or(0);
            let _ = writeln!(
                out,
                "    pub fn {name}(self, {}) -> String {{\n        #[allow(unused_imports)]\n        use ::core::fmt::Write as _;",
                params.join(", ")
            );
            for v in s.counts.iter() {
                let v = ident(v);
                let _ = writeln!(out, "        let {v} = Count::count(&{v});");
            }
            for v in s.strings.iter() {
                let v = ident(v);
                let _ = writeln!(out, "        let {v}: &str = {v}.as_ref();");
            }
            let _ = writeln!(out, "        let mut __out = String::with_capacity({cap});");
            out.push_str("        match self {\n");
            for ((code, _), m) in parsed.iter().zip(&per_lang) {
                let _ = writeln!(out, "            Lang::{} => {{", variant_name(code));
                emit_pieces(&mut out, &m.pieces, 4);
                out.push_str("            }\n");
            }
            out.push_str("        }\n        __out\n    }\n");
        }
    }

    out.push_str(
        "\n    /// Resolves a stored message id (`undo-duplicate-clips`) to its text in this language;\n    /// `None` for unknown ids and for messages with variables.\n",
    );
    out.push_str(
        "    pub fn by_id(self, id: &str) -> Option<&'static str> {\n        match id {\n",
    );
    for s in source.iter().filter(|m| is_static(m)) {
        let _ = writeln!(
            out,
            "            {} => Some(self.{}()),",
            lit(&s.key),
            method_name(&s.key)
        );
    }
    out.push_str("            _ => None,\n        }\n    }\n}\n");

    for (code, msgs) in parsed {
        let _ = writeln!(
            out,
            "\n/// The texts of `{code}` as constants (for tests and `match` patterns).\npub mod {} {{",
            module_name(code)
        );
        for m in msgs.iter().filter(|m| is_static(m)) {
            let _ = writeln!(
                out,
                "    #[doc = {}]\n    pub const {}: &str = {};",
                lit(&format!(" {}", code_span(&preview(&m.pieces)))),
                method_name(&m.key).to_uppercase(),
                lit(&static_text(m))
            );
        }
        out.push_str("}\n");
    }
    out
}

// ------------------------------------------------------------------------------------------------
// Entry point
// ------------------------------------------------------------------------------------------------

/// Generates the catalog source for `locales` (the source language `de` must be among them).
///
/// # Errors
/// Every problem found, each with file and line.
pub fn generate(locales: &[Locale]) -> Result<String, Vec<Error>> {
    let mut errors = Vec::new();
    let mut locales: Vec<&Locale> = locales.iter().collect();
    locales.sort_by(|a, b| (a.code != SOURCE_LANG, &a.code).cmp(&(b.code != SOURCE_LANG, &b.code)));
    if locales.first().map(|l| l.code.as_str()) != Some(SOURCE_LANG) {
        return Err(vec![Error {
            path: "locales".to_owned(),
            line: 0,
            message: format!("the source language folder `{SOURCE_LANG}` is missing"),
        }]);
    }
    for l in &locales {
        let ok = !l.code.is_empty()
            && l.code.split('-').all(|p| {
                !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
            && l.code.starts_with(|c: char| c.is_ascii_lowercase());
        if !ok {
            errors.push(Error {
                path: format!("locales/{}", l.code),
                line: 0,
                message: "folder names are lower-case language codes like `de`, `en`, `pt-br`"
                    .to_owned(),
            });
        }
    }
    let mut parsed: Vec<(String, Vec<Msg>)> = Vec::new();
    for l in &locales {
        let msgs = parse_locale(l, &mut errors);
        parsed.push((l.code.clone(), msgs));
    }
    // Follow-up errors (a message that failed to parse is "missing") only confuse.
    if errors.is_empty() {
        check_languages(&locales, &parsed, &mut errors);
    }
    check_identifiers(&parsed[0].1, &mut errors);
    // One error per place and text, in a stable order.
    errors.sort_by(|a, b| (&a.path, a.line, &a.message).cmp(&(&b.path, b.line, &b.message)));
    errors.dedup();
    if errors.is_empty() {
        Ok(emit(&parsed))
    } else {
        Err(errors)
    }
}

/// Groups the files of `locales/<lang>/*.ftl` for tests: `(code, [(file name, text)])`.
#[allow(dead_code)]
pub fn locales_from(files: &[(&str, &str, &str)]) -> Vec<Locale> {
    let mut by: BTreeMap<&str, Vec<SourceFile>> = BTreeMap::new();
    for (code, name, text) in files {
        by.entry(code).or_default().push(SourceFile {
            path: format!("locales/{code}/{name}"),
            text: (*text).to_owned(),
        });
    }
    by.into_iter()
        .map(|(code, files)| Locale {
            code: code.to_owned(),
            files,
        })
        .collect()
}
