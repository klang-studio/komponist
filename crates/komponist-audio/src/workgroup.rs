//! Hook for joining the device's audio workgroup (macOS, `os_workgroup`).
//!
//! On Apple silicon the scheduler places the audio thread on the right cores only when the thread
//! belongs to the device's I/O workgroup (`kAudioDevicePropertyIOThreadOSWorkgroup`). Whether and
//! how komponist joins it is decided by the realtime spike (#43); until then this module does
//! nothing, so [`lookup`] returns `None` and the callback never calls
//! [`Workgroup::join_current_thread`].
//!
//! The split follows the thread rules: [`lookup`] runs when the stream is built (non-RT, UI
//! thread), [`Workgroup::join_current_thread`] runs once on the audio thread at its first
//! callback (the thread has to join itself). Leaving the workgroup when the stream stops is part
//! of the spike's work too.

/// A device's audio workgroup. Placeholder until #43 decides the implementation.
#[derive(Debug)]
pub(crate) struct Workgroup;

/// The audio workgroup of `device`, `None` where there is none or the join is not implemented.
/// Non-RT.
pub(crate) fn lookup(_device: &cpal::Device) -> Option<Workgroup> {
    None
}

impl Workgroup {
    /// Joins the calling thread to the workgroup. Called on the audio thread, RT-safe.
    pub(crate) fn join_current_thread(self) {}
}
