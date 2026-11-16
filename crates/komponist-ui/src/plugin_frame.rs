//! The plugin frame window's shared parts (S05, ARCHITECTURE §8.4): the header and its measures.
//!
//! Linux: the app draws the frame (`komponist-app` `windows::plugin_frame`). macOS (#52): the
//! bridge owns the frame window and draws the same header with this code, from the model the app
//! sends (`komponist_ipc::frame`).

pub mod header;
pub mod metrics;

use komponist_ipc::frame::HeaderModel;

/// The frame window's title, "Surge XT · Pad · komponist": the plugin name, the track (the
/// header's sub line is "track · format") and the wordmark.
pub fn window_title(header: &HeaderModel) -> String {
    let track = header
        .sub
        .rsplit_once(" · ")
        .map_or(header.sub.as_str(), |(track, _)| track);
    format!("{} · {track} · {}", header.name, crate::WORDMARK)
}
