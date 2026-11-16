//! Which keys the frame takes back from a plugin that has the keyboard focus (WP-50).
//!
//! On X11 a plugin's GUI is a window of another client (the bridge). A click into it, and for
//! JUCE plugins already the opening, moves the X input focus there, and from then on the X server
//! sends the keys to the plugin, not to komponist (spike WP-15 §1.5, owner test 2026-11-13).
//!
//! To still get its shortcuts, komponist puts a **synchronous passive grab** on every key of each
//! frame window ([`KeyGrabber`]). The X server then freezes the keyboard at each key press inside
//! the frame and hands the press to komponist first; a dedicated thread decides at once and either
//! keeps it (`AllowEvents AsyncKeyboard`) or gives it back unchanged (`ReplayKeyboard`), so the
//! plugin (or the frame's own egui) gets it as if there were no grab. The thread never waits for
//! the UI, so the keyboard is frozen only for one focus query.
//!
//! The policy ([`decide`]), while the plugin has the focus:
//!
//! | Key | komponist takes it |
//! |---|---|
//! | Strg S | always (saving the project includes the plugin's state) |
//! | Leertaste (Play / Stop), Esc (closes the frame), Strg Z, Strg Shift Z | unless the user is typing in the plugin |
//! | everything else (letters, digits, arrows, Enter, Tab, …) | never: the plugin gets it |
//!
//! "Typing" is a heuristic, because X11 does not tell which part of a foreign window has the
//! focus: a text key (printable character, Backspace, Delete; no Strg, Alt or Super) passed to
//! the plugin starts or extends a typing session, Enter, Tab or Esc passed to the plugin end it,
//! and it also ends [`TYPING_GRACE`] after the last text key. So "Warm Strings" can be typed into
//! a preset name, Esc then leaves the field, and the next Esc closes the frame. Letters are never
//! taken: they would also be the first letter of a name.
//!
//! While the frame's own window has the focus (a click on the header, Alt-Tab back), every key
//! is given back to egui; the frame then applies all global shortcuts of the main window, the
//! computer-keyboard MIDI layer, and Esc closes the frame (`plugin_frame` module docs).
//!
//! `KOMPONIST_LOG_KEYS=1` logs every key event of the main window and the frames, the grab
//! decisions and X focus changes to stderr.

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::sync::mpsc::{Receiver, Sender, channel};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use komponist_i18n::Lang;
use komponist_model::DeviceId;
#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::protocol::Event;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{
    Allow, ConnectionExt as _, GrabMode, KeyPressEvent, Keycode, Keysym, ModMask, Window,
};
#[cfg(target_os = "linux")]
use x11rb::rust_connection::RustConnection;

use crate::keymap::{Key, KeyChord};

/// How long a typing session lasts after the last text key.
pub const TYPING_GRACE: Duration = Duration::from_secs(5);

/// Whether `KOMPONIST_LOG_KEYS=1` is set.
pub fn log_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("KOMPONIST_LOG_KEYS").is_ok_and(|v| v == "1"))
}

/// Writes a key log line (see the module docs).
pub fn log(line: impl AsRef<str>) {
    if log_enabled() {
        eprintln!("komponist keys: {}", line.as_ref());
    }
}

// ------------------------------------------------------------------------------------------------
// Policy
// ------------------------------------------------------------------------------------------------

/// X keysyms the policy knows (`X11/keysymdef.h`).
pub mod keysym {
    /// `XK_space`.
    pub const SPACE: u32 = 0x0020;
    /// `XK_Escape`.
    pub const ESCAPE: u32 = 0xff1b;
    /// `XK_Return`.
    pub const RETURN: u32 = 0xff0d;
    /// `XK_KP_Enter`.
    pub const KP_ENTER: u32 = 0xff8d;
    /// `XK_Tab`.
    pub const TAB: u32 = 0xff09;
    /// `XK_ISO_Left_Tab` (Shift Tab).
    pub const ISO_LEFT_TAB: u32 = 0xfe20;
    /// `XK_BackSpace`.
    pub const BACKSPACE: u32 = 0xff08;
    /// `XK_Delete`.
    pub const DELETE: u32 = 0xffff;
    /// `XK_s`.
    pub const S: u32 = 0x0073;
    /// `XK_z`.
    pub const Z: u32 = 0x007a;
}

/// Modifier bits of a key event (`KeyButMask`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    /// Shift.
    pub shift: bool,
    /// Control ("Strg").
    pub ctrl: bool,
    /// Alt (Mod1).
    pub alt: bool,
    /// Super (Mod4).
    pub logo: bool,
}

impl Mods {
    /// From the X `state` field; Caps Lock, Num Lock and AltGr (Mod5) do not count.
    #[cfg(any(target_os = "linux", test))]
    pub fn from_state(state: u16) -> Mods {
        Mods {
            shift: state & 0x0001 != 0,
            ctrl: state & 0x0004 != 0,
            alt: state & 0x0008 != 0,
            logo: state & 0x0040 != 0,
        }
    }

    fn plain(self) -> bool {
        !self.shift && !self.ctrl && !self.alt && !self.logo
    }
}

/// What a key is, for the typing heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// Prints a character or edits text (Backspace, Delete).
    Text,
    /// Ends a text entry (Enter, Tab, Esc).
    Confirm,
    /// Anything else (arrows, function keys, modifiers).
    Other,
}

/// Classifies `sym` with `mods`.
pub fn kind(sym: u32, mods: Mods) -> KeyKind {
    if mods.ctrl || mods.alt || mods.logo {
        return KeyKind::Other;
    }
    match sym {
        keysym::RETURN | keysym::KP_ENTER | keysym::TAB | keysym::ISO_LEFT_TAB | keysym::ESCAPE => {
            KeyKind::Confirm
        }
        keysym::BACKSPACE | keysym::DELETE => KeyKind::Text,
        // Latin-1, legacy character sets, dead keys, keypad characters, Unicode keysyms.
        0x0020..=0x007e
        | 0x00a0..=0x13ff
        | 0x20a0..=0x20ac
        | 0xfe50..=0xfe8f
        | 0xff80..=0xffb9
        | 0x0100_0100..=0x0110_ffff => KeyKind::Text,
        _ => KeyKind::Other,
    }
}

/// What happens to a key press inside a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// komponist keeps it and applies this chord.
    Take(KeyChord),
    /// The focused window (plugin or frame) gets it.
    Pass,
}

/// The policy (module docs) for a key `sym` with `mods` while the plugin has the focus.
pub fn decide(sym: u32, mods: Mods, typing: bool) -> Verdict {
    let ctrl_only = mods.ctrl && !mods.alt && !mods.logo;
    match sym {
        keysym::S if ctrl_only && !mods.shift => Verdict::Take(KeyChord::ctrl(Key::S)),
        _ if typing => Verdict::Pass,
        keysym::SPACE if mods.plain() => Verdict::Take(KeyChord::plain(Key::Space)),
        keysym::ESCAPE if mods.plain() => Verdict::Take(KeyChord::plain(Key::Escape)),
        keysym::Z if ctrl_only => Verdict::Take(if mods.shift {
            KeyChord::ctrl_shift(Key::Z)
        } else {
            KeyChord::ctrl(Key::Z)
        }),
        _ => Verdict::Pass,
    }
}

/// The typing heuristic of one frame (module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct Typing {
    last_text: Option<Instant>,
}

impl Typing {
    /// Whether the user is typing in the plugin at `now`.
    pub fn active(&self, now: Instant) -> bool {
        self.last_text
            .is_some_and(|t| now.duration_since(t) < TYPING_GRACE)
    }

    /// A key of `kind` went to the plugin at `now`.
    pub fn passed(&mut self, kind: KeyKind, now: Instant) {
        match kind {
            KeyKind::Text => self.last_text = Some(now),
            KeyKind::Confirm => self.last_text = None,
            KeyKind::Other => {}
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The grab thread
// ------------------------------------------------------------------------------------------------

/// A key komponist took back from a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grabbed {
    /// The frame's device.
    pub device: DeviceId,
    /// The chord to apply.
    pub chord: KeyChord,
}

/// Grabs the keys of the frame windows and runs the policy on its own thread (module docs).
#[cfg(target_os = "linux")]
pub struct KeyGrabber {
    conn: Arc<RustConnection>,
    frames: Arc<Mutex<HashMap<Window, DeviceId>>>,
    rx: Receiver<Grabbed>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for KeyGrabber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyGrabber").finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl KeyGrabber {
    /// Starts the thread on `conn`; it wakes the root viewport of `ctx` for every taken key.
    /// The thread reads all events of `conn` (it is the only reader) and ends with the
    /// connection.
    pub fn start(conn: Arc<RustConnection>, ctx: egui::Context) -> KeyGrabber {
        let frames: Arc<Mutex<HashMap<Window, DeviceId>>> = Arc::default();
        let (tx, rx) = channel();
        let thread_conn = Arc::clone(&conn);
        let thread_frames = Arc::clone(&frames);
        let spawned = std::thread::Builder::new()
            .name("komponist-keys".into())
            .spawn(move || run(&thread_conn, &thread_frames, &tx, &ctx));
        if let Err(e) = spawned {
            eprintln!("komponist: key thread not started ({e}); plugins keep their keys");
        }
        KeyGrabber { conn, frames, rx }
    }

    /// Grabs every key of frame window `frame` (of `device`).
    pub fn add(&self, frame: Window, device: DeviceId) {
        if let Ok(mut f) = self.frames.lock() {
            f.insert(frame, device);
        }
        let grabbed = self.conn.grab_key(
            false,
            frame,
            ModMask::ANY,
            0u8, // AnyKey
            GrabMode::ASYNC,
            GrabMode::SYNC,
        );
        if let Ok(cookie) = grabbed {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
        log(format!(
            "grab keys of frame {frame:#x} (device {})",
            device.0
        ));
    }

    /// Releases frame window `frame` (it is going away).
    pub fn remove(&self, frame: Window) {
        if let Ok(mut f) = self.frames.lock() {
            f.remove(&frame);
        }
        if let Ok(cookie) = self.conn.ungrab_key(0u8, frame, ModMask::ANY) {
            cookie.ignore_error();
        }
        let _ = self.conn.flush();
    }

    /// The keys taken since the last call.
    pub fn poll(&self) -> Vec<Grabbed> {
        self.rx.try_iter().collect()
    }
}

/// Keycode → keysym (level 0) of the current keyboard mapping.
#[cfg(target_os = "linux")]
struct KeyMapping {
    min: u8,
    per: usize,
    syms: Vec<Keysym>,
}

#[cfg(target_os = "linux")]
impl KeyMapping {
    fn load(conn: &RustConnection) -> Option<KeyMapping> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn
            .get_keyboard_mapping(min, max - min + 1)
            .ok()?
            .reply()
            .ok()?;
        Some(KeyMapping {
            min,
            per: usize::from(reply.keysyms_per_keycode),
            syms: reply.keysyms,
        })
    }

    fn keysym(&self, code: Keycode) -> u32 {
        if code < self.min || self.per == 0 {
            return 0;
        }
        self.syms
            .get(usize::from(code - self.min) * self.per)
            .copied()
            .unwrap_or(0)
    }
}

/// Per-thread state.
#[derive(Default)]
#[cfg(target_os = "linux")]
struct Session {
    typing: HashMap<Window, Typing>,
    /// Last release of a taken key (keycode, server time): an auto-repeat press has the same
    /// time.
    released: Option<(Keycode, u32)>,
    focus: Option<Window>,
}

#[cfg(target_os = "linux")]
fn run(
    conn: &RustConnection,
    frames: &Mutex<HashMap<Window, DeviceId>>,
    tx: &Sender<Grabbed>,
    ctx: &egui::Context,
) {
    let mut mapping = KeyMapping::load(conn);
    let mut session = Session::default();
    loop {
        let event = match conn.wait_for_event() {
            Ok(e) => e,
            Err(e) => {
                log(format!("X connection closed ({e}); key thread ends"));
                return;
            }
        };
        match event {
            Event::KeyPress(e) => {
                let device = frames.lock().ok().and_then(|f| f.get(&e.event).copied());
                let verdict = press(conn, &e, device, mapping.as_ref(), &mut session);
                let mode = match verdict {
                    Some(_) => Allow::ASYNC_KEYBOARD,
                    None => Allow::REPLAY_KEYBOARD,
                };
                if let Ok(cookie) = conn.allow_events(mode, x11rb::CURRENT_TIME) {
                    cookie.ignore_error();
                }
                let _ = conn.flush();
                if let (Some(grabbed), false) = (verdict, is_repeat(&session, &e)) {
                    if tx.send(grabbed).is_err() {
                        return;
                    }
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            }
            Event::KeyRelease(e) => session.released = Some((e.detail, e.time)),
            Event::MappingNotify(_) => mapping = KeyMapping::load(conn),
            Event::Error(e) => log(format!(
                "X error {:?} (request {})",
                e.error_kind, e.major_opcode
            )),
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
fn is_repeat(session: &Session, e: &KeyPressEvent) -> bool {
    session.released == Some((e.detail, e.time))
}

/// Decides a grabbed press; `Some` = komponist takes it.
#[cfg(target_os = "linux")]
fn press(
    conn: &RustConnection,
    e: &KeyPressEvent,
    device: Option<DeviceId>,
    mapping: Option<&KeyMapping>,
    session: &mut Session,
) -> Option<Grabbed> {
    let device = device?;
    let sym = mapping.map_or(0, |m| m.keysym(e.detail));
    let mods = Mods::from_state(u16::from(e.state));
    // 0 = None, 1 = PointerRoot: the keys go where the pointer is; treat as the frame's own.
    let focus = conn
        .get_input_focus()
        .ok()
        .and_then(|c| c.reply().ok())
        .map_or(0, |r| r.focus);
    if session.focus != Some(focus) {
        session.focus = Some(focus);
        log(format!("X input focus {focus:#x} (frame {:#x})", e.event));
    }
    let plugin_focused = focus > 1 && focus != e.event;
    let now = Instant::now();
    let typing = session.typing.entry(e.event).or_default();
    let verdict = if plugin_focused {
        decide(sym, mods, typing.active(now))
    } else {
        Verdict::Pass
    };
    if plugin_focused && verdict == Verdict::Pass {
        typing.passed(kind(sym, mods), now);
    }
    log(format!(
        "frame {:#x} (device {}): keysym {sym:#x} {mods:?} focus {} typing {} → {}",
        e.event,
        device.0,
        if plugin_focused { "plugin" } else { "frame" },
        typing.active(now),
        match verdict {
            Verdict::Take(c) => format!("komponist ({})", c.label(Lang::En)),
            Verdict::Pass => "pass".to_owned(),
        }
    ));
    match verdict {
        Verdict::Take(chord) => Some(Grabbed { device, chord }),
        Verdict::Pass => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        logo: false,
    };
    const CTRL: Mods = Mods {
        ctrl: true,
        ..PLAIN
    };
    const CTRL_SHIFT: Mods = Mods {
        ctrl: true,
        shift: true,
        ..PLAIN
    };
    const SHIFT: Mods = Mods {
        shift: true,
        ..PLAIN
    };

    #[test]
    fn space_and_esc_come_back_unless_the_user_types() {
        use Verdict::*;
        assert_eq!(
            decide(keysym::SPACE, PLAIN, false),
            Take(KeyChord::plain(Key::Space))
        );
        assert_eq!(
            decide(keysym::ESCAPE, PLAIN, false),
            Take(KeyChord::plain(Key::Escape))
        );
        assert_eq!(decide(keysym::SPACE, PLAIN, true), Pass);
        assert_eq!(decide(keysym::ESCAPE, PLAIN, true), Pass);
        // Shift Space types a space; Alt Space is the window manager's.
        assert_eq!(decide(keysym::SPACE, SHIFT, false), Pass);
    }

    #[test]
    fn strg_s_always_comes_back_and_undo_only_outside_text() {
        use Verdict::*;
        for typing in [false, true] {
            assert_eq!(
                decide(keysym::S, CTRL, typing),
                Take(KeyChord::ctrl(Key::S))
            );
        }
        assert_eq!(decide(keysym::Z, CTRL, false), Take(KeyChord::ctrl(Key::Z)));
        assert_eq!(
            decide(keysym::Z, CTRL_SHIFT, false),
            Take(KeyChord::ctrl_shift(Key::Z))
        );
        assert_eq!(decide(keysym::Z, CTRL, true), Pass);
        assert_eq!(decide(keysym::S, CTRL_SHIFT, false), Pass);
    }

    #[test]
    fn letters_digits_and_navigation_stay_with_the_plugin() {
        for sym in [
            u32::from(b'r'),
            u32::from(b'l'),
            u32::from(b'c'),
            u32::from(b'1'),
            keysym::RETURN,
            keysym::TAB,
            0xff52, // Up
            0xff50, // Home
        ] {
            assert_eq!(decide(sym, PLAIN, false), Verdict::Pass, "{sym:#x}");
        }
        assert_eq!(decide(u32::from(b't'), CTRL, false), Verdict::Pass);
    }

    #[test]
    fn typing_starts_with_text_and_ends_with_confirm_or_after_the_grace_time() {
        let t = Instant::now();
        let mut typing = Typing::default();
        assert!(!typing.active(t));
        typing.passed(kind(u32::from(b'W'), SHIFT), t);
        assert!(typing.active(t));
        // "Warm Strings": the space goes to the plugin.
        assert_eq!(
            decide(keysym::SPACE, PLAIN, typing.active(t)),
            Verdict::Pass
        );
        typing.passed(kind(keysym::SPACE, PLAIN), t);
        typing.passed(kind(keysym::RETURN, PLAIN), t);
        assert!(!typing.active(t));
        typing.passed(kind(keysym::BACKSPACE, PLAIN), t);
        assert!(typing.active(t + TYPING_GRACE / 2));
        assert!(!typing.active(t + TYPING_GRACE));
        // Arrows and shortcuts do not count as typing.
        let mut typing = Typing::default();
        typing.passed(kind(0xff52, PLAIN), t);
        typing.passed(kind(u32::from(b'c'), CTRL), t);
        assert!(!typing.active(t));
    }

    #[test]
    fn keys_are_classified_by_keysym() {
        assert_eq!(kind(u32::from(b'a'), PLAIN), KeyKind::Text);
        assert_eq!(kind(0x00e4, PLAIN), KeyKind::Text); // ä
        assert_eq!(kind(0x0100_20ac, PLAIN), KeyKind::Text); // € as Unicode keysym
        assert_eq!(kind(0xffb5, PLAIN), KeyKind::Text); // KP_5
        assert_eq!(kind(keysym::KP_ENTER, PLAIN), KeyKind::Confirm);
        assert_eq!(kind(keysym::ISO_LEFT_TAB, SHIFT), KeyKind::Confirm);
        assert_eq!(kind(0xffbe, PLAIN), KeyKind::Other); // F1
        assert_eq!(kind(u32::from(b'a'), CTRL), KeyKind::Other);
        assert_eq!(
            Mods::from_state(0x0004 | 0x0001 | 0x0010 | 0x0002),
            CTRL_SHIFT
        );
    }
}
