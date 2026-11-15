//! Context placeholders in parameter ids.
//!
//! A device layout names a cell's parameter with a template such as `op{env_op}_attack`,
//! `b{band}_freq` or `pad{sel}_tune`. The cell binds to the parameter of the operator, band or
//! pad that is selected right now: the layout gives the template, the view gives a
//! [`ParamContext`], [`resolve`] gives the [`ParamId`]. No UI uses this yet (WP-82 and later).

use std::collections::BTreeMap;
use std::fmt::{self, Display};

use super::ParamId;

/// The operator whose envelope the envelope page shows (`op{env_op}_attack`).
pub const ENV_OP: &str = "env_op";
/// The selected EQ band (`b{band}_freq`).
pub const BAND: &str = "band";
/// The selected pad or similar selection (`pad{sel}_tune`).
pub const SEL: &str = "sel";
/// The envelope a device with several shows, e.g. `amp` or `filt` (`{env_view}_a`).
pub const ENV_VIEW: &str = "env_view";
/// The LFO a device with several shows, `1` or `2` (`lfo{lfo_view}_rate`).
pub const LFO_VIEW: &str = "lfo_view";

/// The values of the placeholders a view currently has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParamContext {
    values: BTreeMap<String, String>,
}

impl ParamContext {
    /// No placeholder set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets placeholder `key` (`ENV_OP` … `ENV_VIEW`, or any name a layout uses) to `value`.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Display) -> Self {
        self.set(key, value);
        self
    }

    /// Sets placeholder `key` to `value`.
    pub fn set(&mut self, key: &str, value: impl Display) {
        self.values.insert(key.to_owned(), value.to_string());
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

/// Why a template did not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The context has no value for this placeholder.
    Missing(String),
    /// A `{` without `}`, a `}` without `{`, nested braces or an empty `{}`.
    Malformed,
}

impl Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::Missing(key) => write!(f, "no value for placeholder {{{key}}}"),
            ResolveError::Malformed => f.write_str("malformed placeholder"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// The placeholder names of `template` in order of appearance (`op{env_op}_{band}` gives
/// `env_op`, `band`), or [`ResolveError::Malformed`]. Empty for a plain id.
pub fn placeholders(template: &str) -> Result<Vec<&str>, ResolveError> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        if rest.as_bytes()[i] == b'}' {
            return Err(ResolveError::Malformed);
        }
        let after = &rest[i + 1..];
        let end = after.find(['{', '}']).ok_or(ResolveError::Malformed)?;
        if after.as_bytes()[end] == b'{' || end == 0 {
            return Err(ResolveError::Malformed);
        }
        names.push(&after[..end]);
        rest = &after[end + 1..];
    }
    Ok(names)
}

/// Whether `id` contains placeholders (cheap check, no validation).
pub fn is_template(id: &str) -> bool {
    id.contains('{')
}

/// Replaces every `{name}` of `template` with its value in `context`.
pub fn resolve(template: &str, context: &ParamContext) -> Result<ParamId, ResolveError> {
    let names = placeholders(template)?;
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for name in names {
        let start = rest.find('{').ok_or(ResolveError::Malformed)?;
        out.push_str(&rest[..start]);
        out.push_str(
            context
                .get(name)
                .ok_or_else(|| ResolveError::Missing(name.to_owned()))?,
        );
        rest = &rest[start + name.len() + 2..];
    }
    out.push_str(rest);
    Ok(ParamId::new(out))
}
