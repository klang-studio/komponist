//! Sample loading, project links and state shared by the bundled samplers.

pub mod links;
pub mod load;
pub mod settings;
pub mod testkit;

pub use links::{Collector, SampleLink};
pub use load::{ManagedLoader, PadStatus, PadView, SamplerHandle, SyncStatus};

/// Capacity of the shared loader and sample link format.
const MAX_PAD_SLOTS: usize = 8;
