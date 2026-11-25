//! Host-themed canvas drawing for nice-plug devices.

use std::ffi::{CStr, c_void};
use std::ptr;

use clap_sys::plugin::clap_plugin;
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;

pub use komponist_ipc::device_canvas::{
    Alignment, DisplayList, EXTENSION_ID, FmFrame, Frame, MAX_FM_OPERATORS, Point, Primitive, Tone,
};

/// Collects a canvas's display primitives in painter order.
pub struct Painter(Vec<Primitive>);

impl Painter {
    /// Add a complete display primitive.
    pub fn push(&mut self, primitive: Primitive) {
        self.0.push(primitive);
    }

    /// Draw a token-colored line.
    pub fn line(&mut self, from: Point, to: Point, tone: Tone) {
        self.push(Primitive::Line { from, to, tone });
    }

    /// Draw a token-colored open path.
    pub fn polyline(&mut self, points: Vec<Point>, tone: Tone) {
        self.push(Primitive::Polyline { points, tone });
    }
}

/// Collects the display lists of an instance's canvases.
#[derive(Default)]
pub struct Display {
    frames: Vec<Frame>,
    fm_topologies: Vec<FmFrame>,
}

impl Display {
    /// Record one canvas declared in the version 2 layout.
    pub fn canvas(&mut self, key: impl Into<String>, paint: impl FnOnce(&mut Painter)) {
        let mut painter = Painter(Vec::new());
        paint(&mut painter);
        self.frames.push(Frame {
            key: key.into(),
            primitives: painter.0,
        });
    }

    /// Publish the current topology of a host-rendered FM diagram.
    pub fn fm_topology(&mut self, topology: FmFrame) {
        self.fm_topologies.push(topology);
    }

    /// Finish the frame for the public transfer format.
    pub fn finish(self) -> DisplayList {
        DisplayList {
            frames: self.frames,
            fm_topologies: self.fm_topologies,
        }
    }
}

/// The instance data returned by the plugin's `clap_custom_extension_data`.
pub trait DisplaySource: Send + Sync + 'static {
    /// Read a snapshot and draw it on the control thread.
    fn paint(&self, display: &mut Display);
}

#[repr(C)]
struct Extension {
    draw: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32) -> u32,
}

/// Register the display callback for a plugin with custom data of type `D`.
pub fn extension<P: ClapPlugin, D: DisplaySource>(id: &CStr) -> *const c_void {
    if id != EXTENSION_ID {
        return ptr::null();
    }
    let table: &'static Extension = &Extension { draw: draw::<P, D> };
    ptr::from_ref(table).cast()
}

unsafe extern "C" fn draw<P: ClapPlugin, D: DisplaySource>(
    plugin: *const clap_plugin,
    destination: *mut u8,
    capacity: u32,
) -> u32 {
    if plugin.is_null() || destination.is_null() {
        return 0;
    }
    // SAFETY: This table is registered for the initialized instance of Wrapper<P>.
    let encoded = unsafe {
        Wrapper::<P>::with_custom_extension_data::<D, _>(plugin, |data| {
            let mut display = Display::default();
            data.paint(&mut display);
            display.finish().encode()
        })
    }
    .flatten();
    let Some(bytes) = encoded.filter(|bytes| bytes.len() <= capacity as usize) else {
        return 0;
    };
    // SAFETY: The destination is writable for capacity bytes and the source is host-independent.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len()) };
    bytes.len() as u32
}
