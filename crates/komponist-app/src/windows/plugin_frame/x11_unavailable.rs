//! The X11 side of the plugin frames where there is no X11 (macOS, Windows): [`X11::connect`]
//! always fails. There native frames never ask for it: the bridge owns the plugin's frame window
//! (#52, port W6, `plugin_frame` module docs); the X11 path only has to compile.

use komponist_model::DeviceId;
use komponist_tokens::Srgba;

use super::keys::Grabbed;
use super::sizing::Px;

/// Window id, as in the X11 build (`xproto::Window`).
type Window = u32;

/// Never constructed: [`X11::connect`] fails.
#[derive(Debug)]
pub struct X11(std::convert::Infallible);

/// The key grabs of [`X11`]; none here.
#[derive(Debug)]
pub struct KeyGrabber(std::convert::Infallible);

impl KeyGrabber {
    /// The keys taken since the last call.
    pub fn poll(&self) -> Vec<Grabbed> {
        match self.0 {}
    }
}

impl X11 {
    /// Always fails: there is no X11 server on this platform.
    pub fn connect(_ctx: &egui::Context) -> Result<X11, String> {
        Err("no X11 on this platform".to_owned())
    }

    /// The key grabs.
    pub fn keys(&self) -> &KeyGrabber {
        match self.0 {}
    }

    /// The window whose title contains `token`.
    pub fn find_by_title(&self, _token: &str) -> Option<Window> {
        match self.0 {}
    }

    /// Creates the plugin area inside frame window `frame`.
    pub fn create_child(
        &self,
        _frame: Window,
        _pos: (i16, i16),
        _size: Px,
        _background: Srgba,
        _mapped: bool,
    ) -> Result<Window, String> {
        match self.0 {}
    }

    /// Moves and resizes child `child`.
    pub fn configure(&self, _child: Window, _pos: (i16, i16), _size: Px) {
        match self.0 {}
    }

    /// Shows or hides child `child`.
    pub fn set_mapped(&self, _child: Window, _mapped: bool) {
        match self.0 {}
    }

    /// Unmaps child `child` and moves it out of the closing frame.
    pub fn park(&self, _child: Window) {
        match self.0 {}
    }

    /// Destroys child `child`.
    pub fn destroy(&self, _child: Window) {
        match self.0 {}
    }

    /// Grabs the keys of frame window `frame`.
    pub fn grab_keys(&self, _frame: Window, _device: DeviceId) {
        match self.0 {}
    }

    /// Releases the key grabs of frame window `frame`.
    pub fn release_keys(&self, _frame: Window) {
        match self.0 {}
    }

    /// The window with the input focus.
    pub fn input_focus(&self) -> Option<Window> {
        match self.0 {}
    }
}
