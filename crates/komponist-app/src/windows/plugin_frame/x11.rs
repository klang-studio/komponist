//! The X11 side of the plugin frames (ARCHITECTURE §8.4, spike WP-15 §1.3): one `x11rb`
//! connection for the whole app, the lookup of a frame viewport's window by its title token, and
//! the child windows the plugins attach to.
//!
//! The connection lives as long as the app: the child windows belong to it and would vanish with
//! it. Its events (key grabs, errors) are read by the key thread ([`super::keys`]), so every
//! request here either waits for its reply or ignores its error.
//!
//! A child window is never destroyed while a plugin GUI may still live inside it: closing a frame
//! first **parks** the child (unmapped, reparented to the root window), so the frame window can
//! go at once; the child is destroyed when the bridge reports `GuiClosed` (or after a timeout).
//! Destroying it earlier would destroy the plugin's window under the plugin, which Xlib plugins
//! answer with `BadWindow` and an exit.

use std::sync::Arc;

use komponist_model::DeviceId;
use komponist_tokens::Srgba;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::keys::KeyGrabber;
use super::sizing::Px;

/// The app's X11 connection for the plugin frames.
pub struct X11 {
    conn: Arc<RustConnection>,
    root: Window,
    keys: KeyGrabber,
}

impl std::fmt::Debug for X11 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11").field("root", &self.root).finish()
    }
}

impl X11 {
    /// Connects to `$DISPLAY` (the display eframe uses, X11 is forced) and starts the key thread,
    /// which wakes `ctx`'s root viewport.
    pub fn connect(ctx: &egui::Context) -> Result<X11, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = conn.setup().roots[screen].root;
        let conn = Arc::new(conn);
        let keys = KeyGrabber::start(Arc::clone(&conn), ctx.clone());
        Ok(X11 { conn, root, keys })
    }

    /// The key grabs.
    pub fn keys(&self) -> &KeyGrabber {
        &self.keys
    }

    /// The window whose `WM_NAME` contains `token` (a recursive walk from the root, so it also
    /// finds a frame a window manager has reparented).
    pub fn find_by_title(&self, token: &str) -> Option<Window> {
        let needle = token.as_bytes();
        let mut stack = vec![self.root];
        while let Some(w) = stack.pop() {
            let name = self
                .conn
                .get_property(
                    false,
                    w,
                    x11rb::protocol::xproto::AtomEnum::WM_NAME,
                    x11rb::protocol::xproto::AtomEnum::ANY,
                    0,
                    256,
                )
                .ok()
                .and_then(|c| c.reply().ok());
            if name.is_some_and(|r| r.value.windows(needle.len()).any(|x| x == needle)) {
                return Some(w);
            }
            if let Some(tree) = self.conn.query_tree(w).ok().and_then(|c| c.reply().ok()) {
                stack.extend(tree.children);
            }
        }
        None
    }

    /// Creates the plugin area inside frame window `frame` at `pos` with `size`, filled with
    /// `background` (the body colour, so resizing shows no other colour); mapped if `mapped`.
    pub fn create_child(
        &self,
        frame: Window,
        pos: (i16, i16),
        size: Px,
        background: Srgba,
        mapped: bool,
    ) -> Result<Window, String> {
        let depth = self
            .conn
            .get_geometry(frame)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .depth;
        let [r, g, b, _] = background;
        let mut pixel = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
        if depth == 32 {
            pixel |= 0xff00_0000;
        }
        let child = self.conn.generate_id().map_err(|e| e.to_string())?;
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                child,
                frame,
                pos.0,
                pos.1,
                size.w.max(1) as u16,
                size.h.max(1) as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .background_pixel(pixel)
                    .event_mask(EventMask::NO_EVENT),
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())?;
        if mapped {
            self.set_mapped(child, true);
        }
        Ok(child)
    }

    /// Moves and resizes child `child`.
    pub fn configure(&self, child: Window, pos: (i16, i16), size: Px) {
        let aux = ConfigureWindowAux::new()
            .x(i32::from(pos.0))
            .y(i32::from(pos.1))
            .width(size.w.max(1))
            .height(size.h.max(1));
        if let Ok(c) = self.conn.configure_window(child, &aux) {
            c.ignore_error();
        }
        let _ = self.conn.flush();
    }

    /// Shows or hides child `child` (view switch).
    pub fn set_mapped(&self, child: Window, mapped: bool) {
        let done = if mapped {
            self.conn.map_window(child).map(|c| c.ignore_error())
        } else {
            self.conn.unmap_window(child).map(|c| c.ignore_error())
        };
        let _ = done;
        let _ = self.conn.flush();
    }

    /// Unmaps child `child` and moves it to the root window, out of the frame that is closing
    /// (module docs).
    pub fn park(&self, child: Window) {
        if let Ok(c) = self.conn.unmap_window(child) {
            c.ignore_error();
        }
        if let Ok(c) = self.conn.reparent_window(child, self.root, 0, 0) {
            c.ignore_error();
        }
        let _ = self.conn.flush();
    }

    /// Destroys child `child` (and the dead plugin's window inside, if any).
    pub fn destroy(&self, child: Window) {
        if let Ok(c) = self.conn.destroy_window(child) {
            c.ignore_error();
        }
        let _ = self.conn.flush();
    }

    /// Grabs the keys of frame window `frame` ([`super::keys`]).
    pub fn grab_keys(&self, frame: Window, device: DeviceId) {
        self.keys.add(frame, device);
    }

    /// Releases the key grabs of frame window `frame`.
    pub fn release_keys(&self, frame: Window) {
        self.keys.remove(frame);
    }

    /// The window with the X input focus (0 = none, 1 = pointer root).
    pub fn input_focus(&self) -> Option<Window> {
        self.conn
            .get_input_focus()
            .ok()?
            .reply()
            .ok()
            .map(|r| r.focus)
    }
}
