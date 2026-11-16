//! MIDI inputs through midir (ALSA sequencer, JACK where enabled; CoreMIDI on macOS; WinMM on
//! Windows) with per-device enable, activity flags and timestamps (ARCHITECTURE §7, S07 "MIDI-Eingänge").
//!
//! midir callbacks run on their own threads. They stamp each message with `Instant::now()` and push
//! it into a lock-free queue that the audio callback drains through [`MidiReceiver`] (via
//! [`crate::MidiIn`]). Only channel messages (note on/off, CC, pitch bend, aftertouch, program
//! change) are forwarded; SysEx and system real-time messages are dropped.
//!
//! **Windows (WinMM):** midir has no virtual ports there and a WinMM input device is exclusive per
//! process: while another program (a DAW, a MIDI monitor) holds it, [`MidiInputHub::refresh`] and
//! [`MidiInputHub::set_enabled`] report a [`MidiError::Backend`], and the port stays listed as
//! present. Port names are the device names WinMM reports (it cuts them at 31 characters; the hub
//! appends " (2)" to a repeated name). Tests therefore cannot create ports; they feed the input
//! path through `MidiInputHub::inject_for_test` or [`TestMidiSource`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use midir::{Ignore, MidiInput, MidiInputConnection, MidiInputPort};

/// Capacity of the queue between the MIDI threads and the audio callback, in messages. Messages
/// beyond it are dropped.
pub const MIDI_QUEUE_CAPACITY: usize = 4096;

/// Id of a MIDI input port, stable per device name for the lifetime of the hub (a re-plugged device
/// gets its old id back). The app maps it 1:1 onto `komponist_engine::MidiPort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MidiPortId(pub u16);

/// A MIDI message with its arrival time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiStamped {
    /// `Instant::now()` when the message reached komponist.
    pub time: Instant,
    /// The port it came from.
    pub port: MidiPortId,
    /// Channel message bytes (status, data1, data2); unused bytes are 0.
    pub data: [u8; 3],
}

/// A MIDI input as listed in the settings (S07).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MidiPortInfo {
    /// Stable id.
    pub id: MidiPortId,
    /// Device name as reported by the backend ("Arturia KeyStep 37").
    pub name: String,
    /// Whether messages from it are forwarded (switch in S07).
    pub enabled: bool,
    /// Whether the device is currently present.
    pub connected: bool,
}

/// Why a MIDI operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiError {
    /// The MIDI backend could not be opened or failed.
    Backend(String),
    /// No port with this name.
    UnknownPort(String),
}

impl core::fmt::Display for MidiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MidiError::Backend(m) => write!(f, "MIDI backend error: {m}"),
            MidiError::UnknownPort(n) => write!(f, "unknown MIDI port {n}"),
        }
    }
}

impl std::error::Error for MidiError {}

/// The audio callback's end of the MIDI queue. Moves into the stream together with the engine.
#[derive(Debug)]
pub struct MidiReceiver {
    consumer: rtrb::Consumer<MidiStamped>,
}

impl MidiReceiver {
    /// A receiver that no MIDI input feeds: it never yields an event. For an
    /// [`crate::AudioSystem`] without a MIDI backend (no ALSA sequencer, headless tests and tools),
    /// where [`MidiInputHub::new`] is not available.
    pub fn disconnected() -> MidiReceiver {
        let (_sender, receiver) = queue(1);
        receiver
    }

    /// RT. Pops the oldest event if it was stamped before `now`.
    pub(crate) fn pop_before(&mut self, now: Instant) -> Option<MidiStamped> {
        match self.consumer.peek() {
            Ok(event) if event.time < now => self.consumer.pop().ok(),
            _ => None,
        }
    }

    /// RT. Drops every event stamped before `now` (used while no engine runs, so stale notes do
    /// not reach an engine attached later).
    pub(crate) fn discard_before(&mut self, now: Instant) {
        while self.pop_before(now).is_some() {}
    }
}

/// The MIDI threads' end of the queue. The midir callbacks of all ports share one producer, so
/// they take turns through a mutex (MIDI threads only; the audio side never locks). The stamp is
/// taken under the lock, so the queue stays ordered by time.
#[derive(Debug, Clone)]
pub(crate) struct MidiSender {
    producer: Arc<Mutex<rtrb::Producer<MidiStamped>>>,
}

impl MidiSender {
    /// Stamps `data` with `Instant::now()` and queues it. `false` if the queue is full.
    pub(crate) fn push(&self, port: MidiPortId, data: [u8; 3]) -> bool {
        let mut producer = self.producer.lock().unwrap_or_else(PoisonError::into_inner);
        let event = MidiStamped {
            time: Instant::now(),
            port,
            data,
        };
        producer.push(event).is_ok()
    }
}

/// A queue of `capacity` messages between the MIDI threads and the audio callback.
pub(crate) fn queue(capacity: usize) -> (MidiSender, MidiReceiver) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let sender = MidiSender {
        producer: Arc::new(Mutex::new(producer)),
    };
    (sender, MidiReceiver { consumer })
}

/// A MIDI source without hardware: pushes messages into the queue of a [`MidiReceiver`] exactly as
/// a midir port does (stamped with `Instant::now()`), so tests on the null backend cover the real
/// input path including [`crate::MidiIn::frame_of`].
#[derive(Debug, Clone)]
pub struct TestMidiSource {
    sender: MidiSender,
}

impl TestMidiSource {
    /// A source and the receiver for [`crate::AudioSystem::new`].
    pub fn new() -> (TestMidiSource, MidiReceiver) {
        let (sender, receiver) = queue(MIDI_QUEUE_CAPACITY);
        (TestMidiSource { sender }, receiver)
    }

    /// Queues the channel message `data` (status, data1, data2) from `port`. `false` if the queue
    /// is full.
    pub fn send(&self, port: MidiPortId, data: [u8; 3]) -> bool {
        self.sender.push(port, data)
    }

    /// Note on, channel 1.
    pub fn note_on(&self, port: MidiPortId, key: u8, velocity: u8) -> bool {
        self.send(port, [0x90, key, velocity])
    }

    /// Note off, channel 1.
    pub fn note_off(&self, port: MidiPortId, key: u8) -> bool {
        self.send(port, [0x80, key, 0])
    }
}

/// The channel message at the start of `bytes` as `[status, data1, data2]` (unused bytes 0), or
/// `None` for SysEx, system common and real-time messages, running status and truncated input.
pub(crate) fn channel_message(bytes: &[u8]) -> Option<[u8; 3]> {
    let (&status, data) = bytes.split_first()?;
    if !(0x80..0xF0).contains(&status) {
        return None;
    }
    // Program change and channel pressure carry one data byte, the others two.
    let len = if matches!(status & 0xF0, 0xC0 | 0xD0) {
        1
    } else {
        2
    };
    let data = data.get(..len)?;
    if data.iter().any(|&b| b >= 0x80) {
        return None;
    }
    let mut out = [status, 0, 0];
    out[1..=len].copy_from_slice(data);
    Some(out)
}

/// The settings name of a port reported by midir.
///
/// macOS: CoreMIDI reports the endpoint's display name (`kMIDIPropertyDisplayName`: the device
/// name, plus the endpoint name when the device has several, e.g. `"Arturia KeyStep 37"`,
/// `"IAC Driver Bus 1"`). It does not contain the endpoint's unique id, so it survives restarts
/// and re-plugging; midir tells re-plugged ports apart by that unique id. Two identical devices
/// get a " (2)" suffix from the hub.
///
/// Windows: WinMM reports the device name (`MIDIINCAPS::szPname`, e.g. `"Arturia KeyStep 37"`). It
/// carries no address, so it is used as is (trimmed).
///
/// Elsewhere (ALSA): see [`alsa_port_name`].
fn port_name(raw: &str) -> String {
    if cfg!(any(target_os = "macos", windows)) {
        raw.trim().to_owned()
    } else {
        alsa_port_name(raw)
    }
}

/// The settings name of an ALSA sequencer port. The ALSA backend reports
/// `"client:port C:P"`; the numeric address changes when a device is re-plugged, so it is
/// dropped, and the client prefix is dropped when the port name repeats it
/// (`"Midi Through:Midi Through Port-0 14:0"` → `"Midi Through Port-0"`).
fn alsa_port_name(raw: &str) -> String {
    let is_address = |s: &str| {
        s.split_once(':').is_some_and(|(client, port)| {
            !client.is_empty()
                && !port.is_empty()
                && client
                    .bytes()
                    .chain(port.bytes())
                    .all(|b| b.is_ascii_digit())
        })
    };
    let base = match raw.rsplit_once(' ') {
        Some((head, address)) if is_address(address) => head,
        _ => raw,
    };
    match base.split_once(':') {
        Some((client, port)) if port.starts_with(client) => port.to_owned(),
        _ => base.to_owned(),
    }
}

/// Whether a newly seen port starts disabled: the system-wide virtual MIDI ports, which would
/// otherwise feed komponist with whatever other programs send. "Midi Through" (ALSA) everywhere,
/// the IAC Driver buses (CoreMIDI display name "IAC Driver Bus 1", midir exposes no driver
/// property) only on macOS (`mac`). Windows has no system-wide virtual ports: virtual cables there
/// (loopMIDI and the like) are drivers the user installed on purpose.
fn starts_disabled(name: &str, mac: bool) -> bool {
    name.starts_with("Midi Through") || (mac && name.starts_with("IAC"))
}

/// Client name of the hub and its connections.
const CLIENT_NAME: &str = "komponist";

/// Flags shared between a port's midir callback and the hub.
#[derive(Debug, Default)]
struct PortFlags {
    /// Forward messages (cleared before the connection closes, so nothing slips through).
    enabled: AtomicBool,
    /// A channel message arrived since the last [`MidiInputHub::take_activity`].
    activity: AtomicBool,
}

/// What a port's midir callback does with a message, on the MIDI thread: forwards a channel
/// message into the queue, sets the activity flag and wakes the UI.
struct PortSink {
    id: MidiPortId,
    flags: Arc<PortFlags>,
    sender: MidiSender,
    waker: Arc<OnceLock<Waker>>,
}

impl PortSink {
    fn deliver(&self, bytes: &[u8]) {
        if !self.flags.enabled.load(Ordering::Relaxed) {
            return;
        }
        if let Some(data) = channel_message(bytes) {
            self.flags.activity.store(true, Ordering::Relaxed);
            // A full queue means the audio callback is not draining it; drop the message.
            let _ = self.sender.push(self.id, data);
            if let Some(wake) = self.waker.get() {
                wake();
            }
        }
    }
}

/// A port seen by the hub.
struct PortEntry {
    info: MidiPortInfo,
    /// The midir port while present.
    port: Option<MidiInputPort>,
    /// The open connection while enabled and present.
    connection: Option<MidiInputConnection<()>>,
    /// The last attempt to open the port failed (on Windows: another program holds it).
    busy: bool,
    flags: Arc<PortFlags>,
}

/// Called on a MIDI thread after a channel message was queued (see [`MidiInputHub::set_waker`]).
type Waker = Box<dyn Fn() + Send + Sync>;

/// Owner of the MIDI input connections. UI thread.
pub struct MidiInputHub {
    /// The client used for enumeration.
    probe: MidiInput,
    sender: MidiSender,
    ports: Vec<PortEntry>,
    /// Shared with every connection's callback.
    waker: Arc<OnceLock<Waker>>,
    /// Tests: only ports whose name starts with this start enabled (see
    /// [`MidiInputHub::only_connect_to`]).
    only: Option<String>,
    /// Names the user disabled (settings) of ports not seen yet: they start disabled when they
    /// first appear (see [`MidiInputHub::remember_disabled`]).
    remembered_disabled: Vec<String>,
    /// Names the user enabled (settings) although they start disabled by default (see
    /// [`starts_disabled`]): the stored choice wins when such a port first appears.
    remembered_enabled: Vec<String>,
}

impl core::fmt::Debug for MidiInputHub {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MidiInputHub")
            .field("ports", &self.ports())
            .finish_non_exhaustive()
    }
}

impl MidiInputHub {
    /// Opens the MIDI backend (no ports connected yet) and returns the hub and the receiver for the
    /// audio callback. Call [`MidiInputHub::refresh`] afterwards.
    pub fn new() -> Result<(MidiInputHub, MidiReceiver), MidiError> {
        let probe = open_client()?;
        let (sender, receiver) = queue(MIDI_QUEUE_CAPACITY);
        let hub = MidiInputHub {
            probe,
            sender,
            ports: Vec::new(),
            waker: Arc::new(OnceLock::new()),
            only: None,
            remembered_disabled: Vec::new(),
            remembered_enabled: Vec::new(),
        };
        Ok((hub, receiver))
    }

    /// Tests: from now on a newly seen port starts enabled only if its name starts with `prefix`,
    /// so a test never subscribes to the hardware ports of the machine it runs on. Call before
    /// [`MidiInputHub::refresh`].
    pub fn only_connect_to(&mut self, prefix: &str) {
        self.only = Some(prefix.to_owned());
    }

    /// Settings: the user disabled the port `name`. Disables it now if the hub knows it, and
    /// otherwise when it first appears, so a device that is unplugged at startup stays disabled
    /// once it is plugged in. Unknown names are harmless (settings written on another OS).
    pub fn remember_disabled(&mut self, name: &str) {
        if self.entry(name).is_some() {
            let _ = self.set_enabled(name, false);
        } else if !self.remembered_disabled.iter().any(|n| n == name) {
            self.remembered_disabled.push(name.to_owned());
        }
    }

    /// Settings: the user enabled the port `name`. Enables it now if the hub knows it, and
    /// otherwise when it first appears, so a port that starts disabled by default
    /// ([`starts_disabled`]) stays enabled once the user switched it on. Unknown names are harmless.
    pub fn remember_enabled(&mut self, name: &str) {
        self.remembered_disabled.retain(|n| n != name);
        if self.entry(name).is_some() {
            let _ = self.set_enabled(name, true);
        } else if !self.remembered_enabled.iter().any(|n| n == name) {
            self.remembered_enabled.push(name.to_owned());
        }
    }

    /// Re-enumerates the input ports, connects enabled ones that appeared and marks vanished ones
    /// as not connected. New ports start enabled, except the system-wide virtual ones
    /// ([`starts_disabled`]: "Midi Through" on Linux, the IAC Driver buses on macOS) unless the
    /// user enabled them before.
    ///
    /// A port that cannot be connected stays listed; the first such error is returned after all
    /// ports were processed.
    pub fn refresh(&mut self) -> Result<(), MidiError> {
        let mut present: Vec<(String, MidiInputPort)> = Vec::new();
        for port in self.probe.ports() {
            let Ok(raw) = self.probe.port_name(&port) else {
                continue; // vanished during enumeration
            };
            let base = port_name(&raw);
            // Two identical devices: the second one is "Name (2)".
            let mut name = base.clone();
            let mut n = 1;
            while present.iter().any(|(seen, _)| *seen == name) {
                n += 1;
                name = format!("{base} ({n})");
            }
            present.push((name, port));
        }

        for entry in &mut self.ports {
            if !present.iter().any(|(name, _)| *name == entry.info.name) {
                entry.info.connected = false;
                entry.port = None;
                close(entry);
            }
        }
        for (name, port) in present {
            let index = match self.ports.iter().position(|e| e.info.name == name) {
                Some(index) => index,
                None => {
                    let id = MidiPortId(self.ports.len() as u16);
                    let enabled = (self.remembered_enabled.contains(&name)
                        || !starts_disabled(&name, cfg!(target_os = "macos")))
                        && !self.remembered_disabled.contains(&name)
                        && self.only.as_deref().is_none_or(|p| name.starts_with(p));
                    self.ports.push(PortEntry {
                        info: MidiPortInfo {
                            id,
                            name,
                            enabled,
                            connected: false,
                        },
                        port: None,
                        connection: None,
                        busy: false,
                        flags: Arc::new(PortFlags::default()),
                    });
                    self.ports.len() - 1
                }
            };
            let entry = &mut self.ports[index];
            entry.info.connected = true;
            if entry.port.as_ref() != Some(&port) {
                // Re-plugged under a new address: the old subscription is dead.
                close(entry);
                entry.port = Some(port);
            }
        }

        let mut first_error = None;
        for index in 0..self.ports.len() {
            if let Err(e) = self.connect(index) {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// All ports seen since the hub was created, in enumeration order.
    pub fn ports(&self) -> Vec<MidiPortInfo> {
        self.ports.iter().map(|e| e.info.clone()).collect()
    }

    /// Whether the port named `name` is enabled and present but could not be opened. On Windows
    /// this is a WinMM input another program holds (exclusive per process): the port stays listed
    /// and a later [`MidiInputHub::refresh`] or [`MidiInputHub::set_enabled`] tries again.
    pub fn port_busy(&self, name: &str) -> bool {
        self.entry(name).is_some_and(|e| e.busy)
    }

    /// The id of the port named `name`, if seen.
    pub fn port_id(&self, name: &str) -> Option<MidiPortId> {
        self.entry(name).map(|e| e.info.id)
    }

    /// Enables or disables forwarding from the port named `name` (connects or disconnects it).
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), MidiError> {
        let index = self
            .ports
            .iter()
            .position(|e| e.info.name == name)
            .ok_or_else(|| MidiError::UnknownPort(name.to_owned()))?;
        self.ports[index].info.enabled = enabled;
        if enabled {
            self.connect(index)
        } else {
            close(&mut self.ports[index]);
            Ok(())
        }
    }

    /// Sets the function every connection calls (on its MIDI thread) after it queued a channel
    /// message, e.g. a repaint request, so the UI shows held keys and activity while it is idle.
    /// Only the first call counts.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        let _ = self.waker.set(Box::new(waker));
    }

    /// Whether a message arrived on `port` since the last call (the activity dot in S07 and the
    /// status bar). Resets the flag.
    pub fn take_activity(&self, port: MidiPortId) -> bool {
        self.ports
            .iter()
            .find(|e| e.info.id == port)
            .is_some_and(|e| e.flags.activity.swap(false, Ordering::Relaxed))
    }

    /// Tests: lists a port named `name` as present and enabled without a backend port behind it
    /// (WinMM has no virtual ports to create) and returns its id.
    #[cfg(test)]
    fn add_port_for_test(&mut self, name: &str) -> MidiPortId {
        let id = MidiPortId(self.ports.len() as u16);
        let flags = Arc::new(PortFlags::default());
        flags.enabled.store(true, Ordering::Relaxed);
        self.ports.push(PortEntry {
            info: MidiPortInfo {
                id,
                name: name.to_owned(),
                enabled: true,
                connected: true,
            },
            port: None,
            connection: None,
            busy: false,
            flags,
        });
        id
    }

    /// Tests: what the midir callback of port `id` does with `bytes`: the input path without a
    /// backend port (enabled check, channel filter, activity flag, queue, waker).
    #[cfg(test)]
    fn inject_for_test(&self, id: MidiPortId, bytes: &[u8]) {
        let entry = self.ports.iter().find(|e| e.info.id == id).unwrap();
        PortSink {
            id,
            flags: Arc::clone(&entry.flags),
            sender: self.sender.clone(),
            waker: Arc::clone(&self.waker),
        }
        .deliver(bytes);
    }

    fn entry(&self, name: &str) -> Option<&PortEntry> {
        self.ports.iter().find(|e| e.info.name == name)
    }

    /// Opens the connection of port `index` if it is enabled, present and not yet connected.
    fn connect(&mut self, index: usize) -> Result<(), MidiError> {
        let entry = &mut self.ports[index];
        let Some(port) = entry.port.as_ref() else {
            return Ok(());
        };
        if !entry.info.enabled || entry.connection.is_some() {
            entry.busy = false;
            return Ok(());
        }
        // midir's `connect` consumes its client, so every connection gets its own.
        let input = open_client()?;
        let sink = PortSink {
            id: entry.info.id,
            flags: Arc::clone(&entry.flags),
            sender: self.sender.clone(),
            waker: Arc::clone(&self.waker),
        };
        sink.flags.enabled.store(true, Ordering::Relaxed);
        let callback = move |_stamp: u64, bytes: &[u8], _: &mut ()| sink.deliver(bytes);
        match input.connect(port, "komponist-in", callback, ()) {
            Ok(connection) => {
                entry.connection = Some(connection);
                entry.busy = false;
                Ok(())
            }
            Err(e) => {
                entry.flags.enabled.store(false, Ordering::Relaxed);
                entry.busy = true;
                Err(MidiError::Backend(format!("{}: {e}", entry.info.name)))
            }
        }
    }
}

/// A midir client that ignores SysEx, timing and active sensing.
fn open_client() -> Result<MidiInput, MidiError> {
    let mut input = MidiInput::new(CLIENT_NAME).map_err(|e| MidiError::Backend(e.to_string()))?;
    input.ignore(Ignore::All);
    Ok(input)
}

/// Stops forwarding from `entry` and closes its connection (joins the midir thread).
fn close(entry: &mut PortEntry) {
    entry.flags.enabled.store(false, Ordering::Relaxed);
    entry.busy = false;
    if let Some(connection) = entry.connection.take() {
        connection.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // midir has virtual ports on unix only (ALSA, CoreMIDI), not on WinMM.
    #[cfg(unix)]
    use midir::MidiOutput;
    #[cfg(unix)]
    use midir::os::unix::VirtualOutput;

    #[test]
    fn test_source_feeds_the_receiver_in_order() {
        let (source, mut receiver) = TestMidiSource::new();
        assert!(source.note_on(MidiPortId(3), 60, 100));
        assert!(source.note_off(MidiPortId(3), 60));
        let later = Instant::now() + Duration::from_millis(1);
        let on = receiver.pop_before(later).unwrap();
        assert_eq!((on.port, on.data), (MidiPortId(3), [0x90, 60, 100]));
        assert_eq!(receiver.pop_before(later).unwrap().data, [0x80, 60, 0]);
        assert!(receiver.pop_before(later).is_none());
    }

    #[test]
    fn channel_messages_pass_and_system_messages_do_not() {
        assert_eq!(channel_message(&[0x90, 60, 100]), Some([0x90, 60, 100]));
        assert_eq!(channel_message(&[0x8F, 60, 0, 0x90]), Some([0x8F, 60, 0]));
        assert_eq!(channel_message(&[0xC3, 5]), Some([0xC3, 5, 0]));
        assert_eq!(channel_message(&[0xD0, 77]), Some([0xD0, 77, 0]));
        assert_eq!(channel_message(&[0xE0, 0, 64]), Some([0xE0, 0, 64]));
        assert_eq!(channel_message(&[0xF0, 0x7E, 0xF7]), None, "SysEx");
        assert_eq!(channel_message(&[0xF8]), None, "clock");
        assert_eq!(channel_message(&[0xFE]), None, "active sensing");
        assert_eq!(channel_message(&[0x90, 60]), None, "truncated");
        assert_eq!(channel_message(&[60, 100]), None, "running status");
        assert_eq!(channel_message(&[0x90, 0x80, 1]), None, "bad data byte");
        assert_eq!(channel_message(&[]), None);
    }

    #[test]
    fn port_names_drop_the_alsa_address_and_repeated_client() {
        assert_eq!(
            alsa_port_name("Midi Through:Midi Through Port-0 14:0"),
            "Midi Through Port-0"
        );
        assert_eq!(
            alsa_port_name("Arturia KeyStep 37:Arturia KeyStep 37 MIDI 1 20:0"),
            "Arturia KeyStep 37 MIDI 1"
        );
        assert_eq!(
            alsa_port_name("komponist-test:probe 130:0"),
            "komponist-test:probe"
        );
        assert_eq!(alsa_port_name("Some Device"), "Some Device");
        assert_eq!(alsa_port_name("Synth:in 1:x"), "Synth:in 1:x");
    }

    #[test]
    fn virtual_system_ports_start_disabled() {
        for mac in [false, true] {
            assert!(starts_disabled("Midi Through Port-0", mac));
            assert!(!starts_disabled("Arturia KeyStep 37", mac));
            assert!(!starts_disabled("Synth IAC", mac), "prefix, not substring");
        }
        assert!(starts_disabled("IAC Driver Bus 1", true));
        assert!(starts_disabled("IAC-Treiber Bus 2", true));
        assert!(
            !starts_disabled("IAC Driver Bus 1", false),
            "Linux unchanged"
        );
    }

    #[test]
    fn port_names_per_platform() {
        if cfg!(target_os = "macos") {
            // CoreMIDI display names pass through; the ALSA address rules do not apply.
            assert_eq!(port_name("IAC Driver Bus 1"), "IAC Driver Bus 1");
            assert_eq!(port_name("Synth 14:0"), "Synth 14:0");
            assert_eq!(port_name(" Arturia KeyStep 37 "), "Arturia KeyStep 37");
        } else if cfg!(windows) {
            // WinMM names carry no address; they pass through trimmed.
            assert_eq!(port_name("Arturia KeyStep 37"), "Arturia KeyStep 37");
            assert_eq!(port_name(" Synth 14:0 "), "Synth 14:0");
        } else {
            assert_eq!(port_name("Synth:Synth 1 14:0"), "Synth 1");
        }
    }

    /// The hub's input path without a backend port (the only way on Windows, where midir has no
    /// virtual ports and a real device must never be opened by a test): channel messages reach the
    /// queue and set the activity flag, SysEx and a disabled port do not.
    #[test]
    fn hub_input_path_without_a_backend_port() {
        let Ok((mut hub, mut receiver)) = MidiInputHub::new() else {
            eprintln!("MIDI backend unavailable: skipped");
            return;
        };
        // No `refresh`: the machine's real ports are neither listed nor opened.
        let id = hub.add_port_for_test("Test Keyboard");
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        hub.set_waker(move || flag.store(true, Ordering::SeqCst));
        assert_eq!(hub.port_id("Test Keyboard"), Some(id));
        assert!(!hub.take_activity(id));

        hub.inject_for_test(id, &[0xF0, 0x7D, 0x01, 0xF7]);
        hub.inject_for_test(id, &[0xF8]);
        assert!(!hub.take_activity(id), "SysEx and clock are dropped");
        assert!(!woken.load(Ordering::SeqCst));

        hub.inject_for_test(id, &[0x90, 60, 100]);
        let event = receiver.pop_before(Instant::now()).expect("note on queued");
        assert_eq!((event.port, event.data), (id, [0x90, 60, 100]));
        assert!(hub.take_activity(id));
        assert!(!hub.take_activity(id), "reset by the read");
        assert!(woken.load(Ordering::SeqCst));

        hub.set_enabled("Test Keyboard", false).unwrap();
        hub.inject_for_test(id, &[0x90, 61, 100]);
        assert!(receiver.pop_before(Instant::now()).is_none());
        assert!(!hub.take_activity(id));
        assert_eq!(hub.ports().len(), 1);
    }

    #[test]
    fn a_disconnected_receiver_stays_empty() {
        let mut receiver = MidiReceiver::disconnected();
        assert!(receiver.pop_before(Instant::now()).is_none());
    }

    #[test]
    fn a_full_queue_drops_messages() {
        let (sender, mut receiver) = queue(2);
        assert!(sender.push(MidiPortId(0), [0x90, 1, 1]));
        assert!(sender.push(MidiPortId(0), [0x90, 2, 1]));
        assert!(!sender.push(MidiPortId(0), [0x90, 3, 1]));
        let now = Instant::now();
        assert_eq!(receiver.pop_before(now).unwrap().data[1], 1);
        assert_eq!(receiver.pop_before(now).unwrap().data[1], 2);
        assert!(receiver.pop_before(now).is_none());
    }

    /// Waits up to 2 s for the next event in `receiver` (the virtual-port test).
    #[cfg(unix)]
    fn next_event(receiver: &mut MidiReceiver) -> Option<MidiStamped> {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Some(event) = receiver.pop_before(Instant::now()) {
                return Some(event);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        None
    }

    #[cfg(unix)]
    fn virtual_output(name: &str) -> midir::MidiOutputConnection {
        MidiOutput::new("komponist-test")
            .unwrap()
            .create_virtual(name)
            .unwrap()
    }

    /// End to end through the platform's MIDI backend (ALSA sequencer, CoreMIDI) with a midir
    /// virtual port. Skipped where there is no backend. On macOS the virtual source
    /// "komponist-test probe-<pid>" is visible system-wide while the test runs. Not on Windows:
    /// WinMM has no virtual ports (`hub_input_path_without_a_backend_port` covers the path there).
    #[cfg(unix)]
    #[test]
    fn virtual_port_round_trip() {
        let Ok((mut hub, mut receiver)) = MidiInputHub::new() else {
            eprintln!("MIDI backend unavailable: skipped");
            return;
        };
        // Never subscribe to the machine's real devices.
        let prefix = if cfg!(target_os = "macos") {
            "komponist-test "
        } else {
            "komponist-test:"
        };
        hub.only_connect_to(prefix);
        let port = format!("probe-{}", std::process::id());
        let name = format!("{prefix}{port}");
        // The name the virtual port is created with: ALSA prefixes the client name itself.
        let created = if cfg!(target_os = "macos") {
            name.clone()
        } else {
            port.clone()
        };
        let mut out = virtual_output(&created);
        hub.refresh().unwrap();
        let id = hub.port_id(&name).expect("virtual port listed");
        let info = hub.ports().into_iter().find(|p| p.id == id).unwrap();
        assert!(info.enabled && info.connected, "{info:?}");
        assert!(!hub.take_activity(id));

        out.send(&[0x90, 60, 100]).unwrap();
        let event = next_event(&mut receiver).expect("note on arrives");
        assert_eq!((event.port, event.data), (id, [0x90, 60, 100]));
        assert!(event.time <= Instant::now());
        assert!(hub.take_activity(id));
        assert!(!hub.take_activity(id));

        // SysEx and clock are dropped.
        out.send(&[0xF0, 0x7D, 0x01, 0xF7]).unwrap();
        out.send(&[0xF8]).unwrap();
        out.send(&[0xB0, 7, 99]).unwrap();
        assert_eq!(next_event(&mut receiver).unwrap().data, [0xB0, 7, 99]);

        hub.set_enabled(&name, false).unwrap();
        assert!(!hub.ports().iter().find(|p| p.id == id).unwrap().enabled);
        out.send(&[0x90, 61, 100]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(receiver.pop_before(Instant::now()).is_none());
        hub.set_enabled(&name, true).unwrap();
        out.send(&[0x90, 62, 100]).unwrap();
        assert_eq!(next_event(&mut receiver).unwrap().data, [0x90, 62, 100]);

        // Unplug and re-plug: same id, connected again.
        drop(out);
        hub.refresh().unwrap();
        let gone = hub.ports().into_iter().find(|p| p.id == id).unwrap();
        assert!(!gone.connected && gone.enabled);
        let mut out = virtual_output(&created);
        hub.refresh().unwrap();
        assert_eq!(hub.port_id(&name), Some(id));
        assert!(
            hub.ports()
                .into_iter()
                .find(|p| p.id == id)
                .unwrap()
                .connected
        );
        out.send(&[0x80, 62, 0]).unwrap();
        assert_eq!(next_event(&mut receiver).unwrap().data, [0x80, 62, 0]);

        // A name remembered from the settings disables the port when it appears, and a name from
        // another OS is harmless.
        hub.remember_disabled("Linux ALSA Name:Port 20:0");
        drop(out);
        hub.refresh().unwrap();
        hub.remember_disabled(&name);
        assert!(!hub.ports().iter().find(|p| p.id == id).unwrap().enabled);

        assert_eq!(
            hub.set_enabled("komponist test: no such port", true),
            Err(MidiError::UnknownPort(
                "komponist test: no such port".to_owned()
            ))
        );
        // Real ports depend on the machine: reported, not asserted.
        for port in hub.ports() {
            println!("{port:?}");
        }
    }
}
