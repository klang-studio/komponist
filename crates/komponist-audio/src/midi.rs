//! MIDI inputs through midir (ALSA sequencer; JACK where enabled) with per-device enable, activity
//! flags and timestamps (ARCHITECTURE §7, S07 "MIDI-Eingänge").
//!
//! midir callbacks run on their own threads. They stamp each message with `Instant::now()` and push
//! it into a lock-free queue that the audio callback drains through [`MidiReceiver`] (via
//! [`crate::MidiIn`]). Only channel messages (note on/off, CC, pitch bend, aftertouch, program
//! change) are forwarded; SysEx and system real-time messages are dropped.

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

/// The settings name of a port reported by midir. The ALSA backend reports
/// `"client:port C:P"`; the numeric address changes when a device is re-plugged, so it is
/// dropped, and the client prefix is dropped when the port name repeats it
/// (`"Midi Through:Midi Through Port-0 14:0"` → `"Midi Through Port-0"`).
fn port_name(raw: &str) -> String {
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

/// ALSA sequencer client name of the hub and its connections.
const CLIENT_NAME: &str = "komponist";

/// Flags shared between a port's midir callback and the hub.
#[derive(Debug, Default)]
struct PortFlags {
    /// Forward messages (cleared before the connection closes, so nothing slips through).
    enabled: AtomicBool,
    /// A channel message arrived since the last [`MidiInputHub::take_activity`].
    activity: AtomicBool,
}

/// A port seen by the hub.
struct PortEntry {
    info: MidiPortInfo,
    /// The midir port while present.
    port: Option<MidiInputPort>,
    /// The open connection while enabled and present.
    connection: Option<MidiInputConnection<()>>,
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
        };
        Ok((hub, receiver))
    }

    /// Re-enumerates the input ports, connects enabled ones that appeared and marks vanished ones
    /// as not connected. New ports start enabled, except names starting with "Midi Through".
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
                    let enabled = !name.starts_with("Midi Through");
                    self.ports.push(PortEntry {
                        info: MidiPortInfo {
                            id,
                            name,
                            enabled,
                            connected: false,
                        },
                        port: None,
                        connection: None,
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
            return Ok(());
        }
        // midir's `connect` consumes its client, so every connection gets its own.
        let input = open_client()?;
        let sender = self.sender.clone();
        let flags = Arc::clone(&entry.flags);
        let waker = Arc::clone(&self.waker);
        let id = entry.info.id;
        flags.enabled.store(true, Ordering::Relaxed);
        let callback = move |_stamp: u64, bytes: &[u8], _: &mut ()| {
            if !flags.enabled.load(Ordering::Relaxed) {
                return;
            }
            if let Some(data) = channel_message(bytes) {
                flags.activity.store(true, Ordering::Relaxed);
                // A full queue means the audio callback is not draining it; drop the message.
                let _ = sender.push(id, data);
                if let Some(wake) = waker.get() {
                    wake();
                }
            }
        };
        match input.connect(port, "komponist-in", callback, ()) {
            Ok(connection) => {
                entry.connection = Some(connection);
                Ok(())
            }
            Err(e) => {
                entry.flags.enabled.store(false, Ordering::Relaxed);
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
    if let Some(connection) = entry.connection.take() {
        connection.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use midir::MidiOutput;
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
            port_name("Midi Through:Midi Through Port-0 14:0"),
            "Midi Through Port-0"
        );
        assert_eq!(
            port_name("Arturia KeyStep 37:Arturia KeyStep 37 MIDI 1 20:0"),
            "Arturia KeyStep 37 MIDI 1"
        );
        assert_eq!(
            port_name("komponist-test:probe 130:0"),
            "komponist-test:probe"
        );
        assert_eq!(port_name("Some Device"), "Some Device");
        assert_eq!(port_name("Synth:in 1:x"), "Synth:in 1:x");
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

    /// Waits up to 2 s for the next event in `receiver`.
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

    fn virtual_output(name: &str) -> midir::MidiOutputConnection {
        MidiOutput::new("komponist-test")
            .unwrap()
            .create_virtual(name)
            .unwrap()
    }

    /// End to end through the ALSA sequencer with a midir virtual port. Skipped where the
    /// sequencer is unavailable.
    #[test]
    fn virtual_port_round_trip() {
        let Ok((mut hub, mut receiver)) = MidiInputHub::new() else {
            eprintln!("ALSA sequencer unavailable: skipped");
            return;
        };
        let port = format!("probe-{}", std::process::id());
        let name = format!("komponist-test:{port}");
        let mut out = virtual_output(&port);
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
        let mut out = virtual_output(&port);
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
