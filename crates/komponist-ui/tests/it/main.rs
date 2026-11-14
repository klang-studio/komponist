//! One integration-test binary per crate (WP-66): every former `tests/<name>.rs` is a module here,
//! so egui/wgpu link once. Binaries that need their own process stay next to this folder.

mod device_widgets;
mod harness;
mod timeline;
mod value_controls;
mod widget_variants;
mod widgets_a;
