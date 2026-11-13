//! The link between komponist and its plugin bridge processes.
//!
//! Shared memory (memfd), futex signalling with deadlines, SPSC event rings in shared memory and the
//! control protocol over a Unix socket. Both sides use this crate: the app's `ExternalProcessor` and
//! `komponist-bridge`.
//!
//! Scaffold only: the contracts arrive with WP-01, the implementation with WP-16.
