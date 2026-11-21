//! Tests of the program compiler (WP-24): golden event lists, incremental rebuilds, slot mapping,
//! diagnostics (#92).
//!
//! Golden fixtures live in `tests/golden/*.txt`, one readable dump of a compiled program per case.
//! After an intended change, regenerate them and review the diff:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test -p komponist-engine --test compile
//! ```

mod diagnostics;
mod golden;
mod incremental;
mod routing;
mod support;
