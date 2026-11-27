//! The "zu spät" indicator shared by the inspector's device rows, the mixer's insert slots and the
//! device cards: a device whose blocks missed their deadline within the last [`HOLD_S`] seconds
//! (not in the design). `ProcessorHost::late_blocks` counts since the bridge started, so the
//! indicator compares counts frame by frame.

use std::collections::HashMap;

use komponist_i18n::Lang;
use komponist_model::DeviceId;

use crate::device_host::ProcessorHost;

/// The indicator's text: a device whose blocks recently missed their deadline (not in the design).
pub fn label(lang: Lang) -> &'static str {
    lang.late_label()
}

/// „3 Blöcke zu spät“: the tooltip of the late indicator.
pub fn late_blocks(lang: Lang, n: u64) -> String {
    lang.late_blocks(n)
}

/// How long a device shows "zu spät" after a block missed its deadline.
pub const HOLD_S: f64 = 2.0;

/// Per device: the late-block count last seen and when it last grew (egui time).
#[derive(Debug, Default)]
pub struct LateTracker {
    seen: HashMap<DeviceId, (u64, f64)>,
}

impl LateTracker {
    /// The late-block count of `device` when it grew within [`HOLD_S`] and `running`.
    pub fn state(
        &mut self,
        host: &dyn ProcessorHost,
        device: DeviceId,
        running: bool,
        now: f64,
    ) -> Option<u64> {
        let n = host.late_blocks(device);
        let entry = self.seen.entry(device).or_insert((n, f64::NEG_INFINITY));
        if n > entry.0 {
            *entry = (n, now);
        } else if n < entry.0 {
            // The bridge restarted ("Neu laden"): its count starts again.
            *entry = (n, f64::NEG_INFINITY);
        }
        let fresh = now - entry.1 < HOLD_S;
        (running && n > 0 && fresh).then_some(n)
    }

    /// Forgets devices that are not in `live` any more.
    pub fn retain(&mut self, live: &[DeviceId]) {
        self.seen.retain(|d, _| live.contains(d));
    }
}

/// Asks for a repaint when the indicator has to go out again.
pub fn repaint_when_expired(ctx: &egui::Context) {
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(HOLD_S));
}
