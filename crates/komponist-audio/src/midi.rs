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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use midir::{Ignore, MidiInput, MidiInputConnection, MidiInputPort};

/// Capacity of the queue between the MIDI threads and the audio callback, in messages. Messages
/// beyond it are dropped and counted ([`crate::StreamStats::midi_overflows`]); the receiver then
/// releases the notes it delivered, since a dropped note-off would leave one hanging.
pub const MIDI_QUEUE_CAPACITY: usize = 4096;

/// Ports a hub opens at most, over its lifetime: port ids are `0 .. MAX_MIDI_PORTS`. Every
/// [`MidiReceiver`] tracks the held notes of each of them, so a release covers every port that
/// can deliver. The hub refuses further ports ([`MidiInputHub::refused_ports`]); a message from a
/// larger id is not queued.
pub const MAX_MIDI_PORTS: usize = 128;

/// Release messages one callback delivers at most, so a release has a bounded cost; the rest
/// follow at frame 0 of the next callbacks. 16 channels of sustain plus a ten-finger chord on
/// each of them fit.
const RELEASES_PER_CALLBACK: u32 = 256;

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

/// The notes and sustain pedals of one port that the receiver delivered and that are still down.
#[derive(Debug, Clone, Copy)]
struct HeldPort {
    /// One bit per key and channel.
    keys: [u128; 16],
    /// One bit per channel with a key in `keys`.
    channels: u16,
    /// One bit per channel whose sustain pedal (CC 64) is down.
    sustain: u16,
}

impl HeldPort {
    const IDLE: HeldPort = HeldPort {
        keys: [0; 16],
        channels: 0,
        sustain: 0,
    };

    fn idle(&self) -> bool {
        self.channels == 0 && self.sustain == 0
    }

    fn set_key(&mut self, channel: usize, key: u8, down: bool) {
        let keys = &mut self.keys[channel];
        if down {
            *keys |= 1u128 << key;
        } else {
            *keys &= !(1u128 << key);
        }
        self.set_channel(channel);
    }

    fn set_channel(&mut self, channel: usize) {
        let bit = 1u16 << channel;
        if self.keys[channel] == 0 {
            self.channels &= !bit;
        } else {
            self.channels |= bit;
        }
    }
}

/// What every port holds; allocated with the receiver.
#[derive(Debug)]
struct Held {
    /// Indexed by port id.
    ports: [HeldPort; MAX_MIDI_PORTS],
    /// One bit per port with something held.
    active: u128,
}

/// Messages the MIDI threads could not queue, shared by the sender and the receiver.
#[derive(Debug, Default)]
struct Losses {
    /// Lost messages so far.
    lost: AtomicU64,
    /// The highest count of messages queued before a loss. A release waits until the receiver
    /// has taken that many: a lost note-off can belong to any note-on queued before it.
    accepted_before: AtomicU64,
}

/// A pending release of everything held.
#[derive(Debug, Clone, Copy)]
struct Release {
    /// Starts once the receiver has taken this many messages from the queue.
    after: u64,
    /// … and every queued message stamped before this time.
    at: Option<Instant>,
}

/// The audio callback's end of the MIDI queue. Moves into the stream together with the engine.
///
/// It keeps the notes and sustain pedals it delivered and still down, for every port id below
/// [`MAX_MIDI_PORTS`]. When messages were lost (a full queue) or discarded as stale (see
/// [`crate::MidiIn`]), a lost note-off or pedal release would leave a note hanging, so the
/// receiver then delivers a note-off for every held note and a pedal release for every held pedal.
/// After a loss the release follows every message queued before it; after a discard it comes
/// first. It keeps the time order of the messages.
#[derive(Debug)]
pub struct MidiReceiver {
    consumer: rtrb::Consumer<MidiStamped>,
    losses: Arc<Losses>,
    /// `losses.lost` when the receiver last looked.
    seen_lost: u64,
    /// Messages taken from the queue (delivered or discarded).
    consumed: u64,
    /// Stamp of the last message taken from the queue.
    last_time: Option<Instant>,
    /// A stream has just taken the receiver over: its first callback discards stale messages.
    attached: bool,
    held: Box<Held>,
    release: Option<Release>,
    /// Release messages delivered in the current callback.
    released: u32,
}

impl MidiReceiver {
    /// A receiver that no MIDI input feeds: it never yields an event. For an
    /// [`crate::AudioSystem`] without a MIDI backend (no ALSA sequencer, headless tests and tools),
    /// where [`MidiInputHub::new`] is not available.
    pub fn disconnected() -> MidiReceiver {
        let (_sender, receiver) = queue(1);
        receiver
    }

    /// RT. A stream took the receiver over: the next [`MidiReceiver::begin`] discards what queued
    /// up while no stream drained it.
    pub(crate) fn mark_attached(&mut self) {
        self.attached = true;
    }

    /// RT. Starts a callback: counts messages lost to a full queue and, after an attach, discards
    /// messages older than one period of this callback (they would all land on frame 0). Either
    /// loss schedules a release of the held notes.
    pub(crate) fn begin(&mut self, info: &crate::CallbackInfo, stats: &crate::StreamStats) {
        self.released = 0;
        let lost = self.losses.lost.load(Ordering::Acquire);
        if lost != self.seen_lost {
            stats.record_midi_overflows(lost.wrapping_sub(self.seen_lost));
            self.seen_lost = lost;
            // After every message queued before the loss, whenever it arrives: its note-on may
            // belong to a lost note-off.
            let after = self.losses.accepted_before.load(Ordering::Acquire);
            self.schedule_release(Release { after, at: None });
        }
        if std::mem::take(&mut self.attached) {
            let period = crate::frames_to_duration(u64::from(info.frames), info.sample_rate);
            let cutoff = info.now.checked_sub(period).unwrap_or(info.now);
            let stale = self.discard(cutoff);
            if stale > 0 {
                stats.record_midi_stale(stale);
                self.schedule_release(Release {
                    after: self.consumed,
                    at: Some(cutoff),
                });
            }
        }
    }

    /// RT. The next message for a callback that started at `now`: queued messages stamped before
    /// `now`, oldest first, merged with a scheduled release.
    pub(crate) fn pop_before(&mut self, now: Instant) -> Option<MidiStamped> {
        if let Some(release) = self.release {
            let earlier_queued = self.consumer.peek().is_ok_and(|event| {
                self.consumed < release.after || release.at.is_some_and(|at| event.time < at)
            });
            if !earlier_queued {
                if self.released >= RELEASES_PER_CALLBACK {
                    // Later messages wait until the release is complete.
                    return None;
                }
                // Never before a message already delivered.
                let at = match (release.at, self.last_time) {
                    (Some(at), Some(last)) => at.max(last),
                    (at, last) => at.or(last).unwrap_or(now),
                };
                match self.next_release(at) {
                    Some(message) => {
                        self.released += 1;
                        return Some(message);
                    }
                    None => self.release = None,
                }
            }
        }
        match self.consumer.peek() {
            Ok(event) if event.time < now => {
                let event = self.consumer.pop().ok()?;
                self.consumed += 1;
                self.last_time = Some(event.time);
                self.track(&event);
                Some(event)
            }
            _ => None,
        }
    }

    /// RT. Drops every message stamped before `now` while no engine runs, so they do not reach an
    /// engine attached later. Counted as stale; the engine that comes back gets the release.
    pub(crate) fn discard_before(&mut self, now: Instant, stats: &crate::StreamStats) {
        let stale = self.discard(now);
        if stale > 0 {
            stats.record_midi_stale(stale);
            self.schedule_release(Release {
                after: self.consumed,
                at: Some(now),
            });
        }
    }

    /// RT. Drops queued messages stamped before `before`; returns how many.
    fn discard(&mut self, before: Instant) -> u64 {
        let mut count = 0;
        while let Ok(event) = self.consumer.peek()
            && event.time < before
        {
            self.last_time = Some(event.time);
            let _ = self.consumer.pop();
            self.consumed += 1;
            count += 1;
        }
        count
    }

    /// Releases everything held once both conditions hold (a later release covers an earlier
    /// one).
    fn schedule_release(&mut self, next: Release) {
        self.release = Some(match self.release {
            Some(pending) => Release {
                after: pending.after.max(next.after),
                at: pending.at.max(next.at),
            },
            None => next,
        });
    }

    /// RT. Follows a delivered message: note-ons and pedal presses are held until their release.
    fn track(&mut self, event: &MidiStamped) {
        let index = usize::from(event.port.0);
        // The sender queues no message from a larger id.
        let Some(held) = self.held.ports.get_mut(index) else {
            return;
        };
        let [status, data1, data2] = event.data;
        let channel = usize::from(status & 0x0f);
        let key = data1 & 0x7f;
        match status & 0xf0 {
            0x90 if data2 > 0 => held.set_key(channel, key, true),
            0x80 | 0x90 => held.set_key(channel, key, false),
            0xb0 if data1 == 64 && data2 >= 64 => held.sustain |= 1 << channel,
            0xb0 if data1 == 64 => held.sustain &= !(1 << channel),
            // All sound off, all notes off.
            0xb0 if data1 == 120 || data1 == 123 => {
                held.keys[channel] = 0;
                held.set_channel(channel);
            }
            _ => return,
        }
        let bit = 1u128 << index;
        if held.idle() {
            self.held.active &= !bit;
        } else {
            self.held.active |= bit;
        }
    }

    /// RT. The next message of a release, stamped `at`, or `None` once nothing is held.
    fn next_release(&mut self, at: Instant) -> Option<MidiStamped> {
        let Held { ports, active } = &mut *self.held;
        if *active == 0 {
            return None;
        }
        let index = active.trailing_zeros() as usize;
        let held = &mut ports[index];
        let data = if held.channels != 0 {
            let channel = held.channels.trailing_zeros() as usize;
            let key = held.keys[channel].trailing_zeros() as u8;
            held.set_key(channel, key, false);
            [0x80 | channel as u8, key, 0]
        } else {
            let channel = held.sustain.trailing_zeros() as u8;
            held.sustain &= !(1u16 << channel);
            [0xb0 | channel, 64, 0]
        };
        if held.idle() {
            *active &= !(1u128 << index);
        }
        Some(MidiStamped {
            time: at,
            port: MidiPortId(index as u16),
            data,
        })
    }
}

/// What the MIDI threads share under the sender's lock.
#[derive(Debug)]
struct SenderQueue {
    producer: rtrb::Producer<MidiStamped>,
    /// Messages queued so far.
    accepted: u64,
}

/// The MIDI threads' end of the queue. The midir callbacks of all ports share one producer, so
/// they take turns through a mutex (MIDI threads only; the audio side never locks). The stamp is
/// taken under the lock, so the queue stays ordered by time.
#[derive(Debug, Clone)]
pub(crate) struct MidiSender {
    queue: Arc<Mutex<SenderQueue>>,
    losses: Arc<Losses>,
}

impl MidiSender {
    /// Stamps `data` with `Instant::now()` and queues it. `false` if the queue is full (counted
    /// for the receiver) or `port` is not below [`MAX_MIDI_PORTS`].
    pub(crate) fn push(&self, port: MidiPortId, data: [u8; 3]) -> bool {
        self.push_stamped(port, data, Instant::now)
    }

    fn push_stamped(
        &self,
        port: MidiPortId,
        data: [u8; 3],
        stamp: impl FnOnce() -> Instant,
    ) -> bool {
        if usize::from(port.0) >= MAX_MIDI_PORTS {
            return false;
        }
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let event = MidiStamped {
            time: stamp(),
            port,
            data,
        };
        if queue.producer.push(event).is_ok() {
            queue.accepted += 1;
            return true;
        }
        // Published before the count, which the receiver reads first.
        self.losses
            .accepted_before
            .fetch_max(queue.accepted, Ordering::Release);
        self.losses.lost.fetch_add(1, Ordering::Release);
        false
    }
}

/// A queue of `capacity` messages between the MIDI threads and the audio callback.
pub(crate) fn queue(capacity: usize) -> (MidiSender, MidiReceiver) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let losses = Arc::new(Losses::default());
    let sender = MidiSender {
        queue: Arc::new(Mutex::new(SenderQueue {
            producer,
            accepted: 0,
        })),
        losses: Arc::clone(&losses),
    };
    let receiver = MidiReceiver {
        consumer,
        losses,
        seen_lost: 0,
        consumed: 0,
        last_time: None,
        attached: false,
        held: Box::new(Held {
            ports: [HeldPort::IDLE; MAX_MIDI_PORTS],
            active: 0,
        }),
        release: None,
        released: 0,
    };
    (sender, receiver)
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
    /// is full or `port` is not below [`MAX_MIDI_PORTS`] (like a hub, a source has no more ports).
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
            // A full queue means the audio callback is not draining it: the message is dropped,
            // counted, and the receiver releases the held notes.
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
    /// Ports seen after [`MAX_MIDI_PORTS`] others: they get no id and are never opened.
    refused: Vec<String>,
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
            refused: Vec::new(),
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
        let mut first_error = None;
        for (name, port) in present {
            let index = match self.ports.iter().position(|e| e.info.name == name) {
                Some(index) => index,
                None => {
                    let enabled = (self.remembered_enabled.contains(&name)
                        || !starts_disabled(&name, cfg!(target_os = "macos")))
                        && !self.remembered_disabled.contains(&name)
                        && self.only.as_deref().is_none_or(|p| name.starts_with(p));
                    match self.register(name, enabled) {
                        Ok(index) => index,
                        Err(error) => {
                            first_error.get_or_insert(error);
                            continue;
                        }
                    }
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

        for index in 0..self.ports.len() {
            if let Err(e) = self.connect(index) {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Gives a newly seen port the next id, or refuses it once [`MAX_MIDI_PORTS`] ports have one:
    /// the receiver could not release its held notes (#219).
    fn register(&mut self, name: String, enabled: bool) -> Result<usize, MidiError> {
        if self.ports.len() >= MAX_MIDI_PORTS {
            let error = MidiError::Backend(format!(
                "more than {MAX_MIDI_PORTS} MIDI inputs: {name} is not opened"
            ));
            if !self.refused.contains(&name) {
                self.refused.push(name);
            }
            return Err(error);
        }
        self.ports.push(PortEntry {
            info: MidiPortInfo {
                id: MidiPortId(self.ports.len() as u16),
                name,
                enabled,
                connected: false,
            },
            port: None,
            connection: None,
            busy: false,
            // Forwarding starts when the connection opens.
            flags: Arc::new(PortFlags::default()),
        });
        Ok(self.ports.len() - 1)
    }

    /// Ports refused because [`MAX_MIDI_PORTS`] others were seen before them, in the order they
    /// first appeared. [`MidiInputHub::refresh`] also reports them as an error.
    pub fn refused_ports(&self) -> &[String] {
        &self.refused
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
    fn add_port_for_test(&mut self, name: &str) -> Result<MidiPortId, MidiError> {
        let index = self.register(name.to_owned(), true)?;
        let entry = &mut self.ports[index];
        entry.info.connected = true;
        entry.flags.enabled.store(true, Ordering::Relaxed);
        Ok(entry.info.id)
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
    use crate::{CallbackInfo, StreamStats};
    use assert_no_alloc::assert_no_alloc;
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
        let id = hub.add_port_for_test("Test Keyboard").unwrap();
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

    /// Queues a message stamped `time`, as the MIDI threads would have at that moment.
    fn push_at(sender: &MidiSender, time: Instant, data: [u8; 3]) -> bool {
        sender.push_stamped(MidiPortId(5), data, || time)
    }

    /// 480 frames at 48 kHz: a 10 ms period.
    fn info(now: Instant) -> CallbackInfo {
        CallbackInfo {
            sample_rate: 48_000,
            frames: 480,
            now,
            output_latency: Duration::ZERO,
        }
    }

    /// One callback: what the receiver delivers, as (frame, data), RT-checked.
    fn callback(
        receiver: &mut MidiReceiver,
        now: Instant,
        stats: &StreamStats,
    ) -> Vec<(u32, [u8; 3])> {
        let mut out = Vec::with_capacity(512);
        let info = info(now);
        assert_no_alloc(|| {
            receiver.begin(&info, stats);
            let mut midi = crate::MidiIn::new(receiver, info);
            while let Some(event) = midi.pop() {
                if out.len() < out.capacity() {
                    out.push((midi.frame_of(event.time), event.data));
                }
            }
        });
        out
    }

    #[test]
    fn stale_messages_are_discarded_on_attach_and_release_the_held_notes() {
        let (sender, mut receiver) = queue(16);
        let stats = StreamStats::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        push_at(&sender, t0, [0x90, 60, 100]);
        push_at(&sender, t0 + ms(1), [0xb1, 64, 127]);
        assert_eq!(
            callback(&mut receiver, t0 + ms(5), &stats),
            [(240, [0x90, 60, 100]), (288, [0xb1, 64, 127])]
        );

        // The stream changes. While no stream drains the queue the key and the pedal come up
        // and a controller moves; one key goes down shortly before the new stream runs.
        push_at(&sender, t0 + ms(100), [0x80, 60, 0]);
        push_at(&sender, t0 + ms(200), [0xb1, 64, 0]);
        push_at(&sender, t0 + ms(300), [0xb0, 1, 90]);
        let now = t0 + ms(3000);
        push_at(&sender, now - ms(5), [0x90, 64, 90]);
        receiver.mark_attached();
        // The held note and pedal are released at frame 0 instead of hanging; the fresh note
        // keeps its place in the period. No burst of old messages at frame 0.
        assert_eq!(
            callback(&mut receiver, now, &stats),
            [
                (0, [0x80, 60, 0]),
                (0, [0xb1, 64, 0]),
                (240, [0x90, 64, 90])
            ]
        );
        assert_eq!(stats.midi_stale(), 3);
        assert_eq!(stats.midi_overflows(), 0);
        // Once only.
        assert_eq!(callback(&mut receiver, now + ms(10), &stats), []);
    }

    #[test]
    fn a_full_queue_is_counted_and_releases_held_notes_after_the_queued_messages() {
        let (sender, mut receiver) = queue(3);
        let stats = StreamStats::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        push_at(&sender, t0, [0x92, 48, 100]);
        push_at(&sender, t0, [0x92, 52, 100]);
        assert_eq!(callback(&mut receiver, t0 + ms(1), &stats).len(), 2);

        // The audio side stalls: the queue fills and the note-offs are lost.
        assert!(push_at(&sender, t0 + ms(2), [0x92, 55, 100]));
        assert!(push_at(&sender, t0 + ms(3), [0xb2, 1, 10]));
        assert!(push_at(&sender, t0 + ms(4), [0xb2, 1, 20]));
        assert!(!push_at(&sender, t0 + ms(5), [0x82, 48, 0]));
        assert!(!sender.push(MidiPortId(5), [0x82, 52, 0]));
        let now = t0 + ms(6);
        let delivered = callback(&mut receiver, now, &stats);
        assert_eq!(stats.midi_overflows(), 2);
        // The queued messages first (the third note among them), then every held note off
        // right after the last message queued before the loss.
        assert_eq!(
            delivered,
            [
                (288, [0x92, 55, 100]),
                (336, [0xb2, 1, 10]),
                (384, [0xb2, 1, 20]),
                (384, [0x82, 48, 0]),
                (384, [0x82, 52, 0]),
                (384, [0x82, 55, 0]),
            ]
        );
        assert_eq!(callback(&mut receiver, now + ms(10), &stats), []);
        assert_eq!(stats.midi_stale(), 0);
    }

    /// The loss is seen in a callback that started before the note-on was queued: the note-on
    /// cannot be delivered yet, and nothing is held. The release must wait for it, or its lost
    /// note-off leaves it hanging (CodeRabbit on #273).
    #[test]
    fn a_release_waits_for_messages_queued_before_the_loss() {
        let (sender, mut receiver) = queue(2);
        let stats = StreamStats::new();
        let ms = Duration::from_millis;
        let now = Instant::now();
        assert!(push_at(&sender, now + ms(1), [0x90, 60, 100]));
        assert!(push_at(&sender, now + ms(1), [0xb0, 1, 1]));
        assert!(!push_at(&sender, now + ms(2), [0x80, 60, 0]));
        assert_eq!(callback(&mut receiver, now, &stats), []);
        assert_eq!(stats.midi_overflows(), 1);
        // Next callback: the note-on, then its release; nothing in the callback after.
        assert_eq!(
            callback(&mut receiver, now + ms(10), &stats),
            [
                (48, [0x90, 60, 100]),
                (48, [0xb0, 1, 1]),
                (48, [0x80, 60, 0])
            ]
        );
        assert_eq!(callback(&mut receiver, now + ms(20), &stats), []);
        // Without a loss nothing is released.
        assert!(push_at(&sender, now + ms(21), [0x90, 62, 100]));
        assert_eq!(
            callback(&mut receiver, now + ms(30), &stats),
            [(48, [0x90, 62, 100])]
        );
        assert_eq!(callback(&mut receiver, now + ms(40), &stats), []);
    }

    /// Every port id the sender queues has its own entry, so a release covers notes from all of
    /// them; larger ids are not queued at all.
    #[test]
    fn every_queued_port_is_tracked_for_a_release() {
        let (sender, mut receiver) = queue(MAX_MIDI_PORTS + 1);
        let stats = StreamStats::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        for port in 0..MAX_MIDI_PORTS as u16 {
            assert!(sender.push_stamped(MidiPortId(port), [0x90, 60, 100], || t0));
        }
        assert!(!sender.push_stamped(MidiPortId(MAX_MIDI_PORTS as u16), [0x90, 60, 100], || t0));
        assert_eq!(stats.midi_overflows(), 0, "a refused port is not a loss");
        let delivered = callback(&mut receiver, t0 + ms(1), &stats);
        assert_eq!(delivered.len(), MAX_MIDI_PORTS);
        receiver.mark_attached();
        sender.push_stamped(MidiPortId(0), [0x80, 60, 0], || t0 + ms(2));
        let mut ports = Vec::with_capacity(MAX_MIDI_PORTS);
        let info = info(t0 + ms(100));
        assert_no_alloc(|| {
            receiver.begin(&info, &stats);
            while let Some(event) = receiver.pop_before(info.now) {
                assert_eq!(event.data, [0x80, 60, 0]);
                if ports.len() < ports.capacity() {
                    ports.push(event.port.0);
                }
            }
        });
        assert_eq!(ports, (0..MAX_MIDI_PORTS as u16).collect::<Vec<_>>());
        assert_eq!(stats.midi_stale(), 1);
    }

    #[test]
    fn the_hub_refuses_ports_it_could_not_track() {
        let Ok((mut hub, _receiver)) = MidiInputHub::new() else {
            eprintln!("MIDI backend unavailable: skipped");
            return;
        };
        for n in 0..MAX_MIDI_PORTS {
            let id = hub.add_port_for_test(&format!("Port {n}")).unwrap();
            assert_eq!(usize::from(id.0), n);
        }
        assert!(matches!(
            hub.add_port_for_test("One too many"),
            Err(MidiError::Backend(_))
        ));
        assert!(hub.add_port_for_test("One too many").is_err());
        assert_eq!(hub.refused_ports(), ["One too many"]);
        assert_eq!(hub.port_id("One too many"), None);
        assert_eq!(hub.ports().len(), MAX_MIDI_PORTS);
    }

    #[test]
    fn a_large_release_is_spread_over_callbacks_in_order() {
        let (sender, mut receiver) = queue(512);
        let stats = StreamStats::new();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        for channel in 0..3u8 {
            for key in 0..100 {
                push_at(&sender, t0, [0x90 | channel, key, 1]);
            }
        }
        assert_eq!(callback(&mut receiver, t0 + ms(1), &stats).len(), 300);
        sender.losses.accepted_before.store(300, Ordering::Release);
        sender.losses.lost.fetch_add(1, Ordering::Release);
        let now = t0 + ms(20);
        // Arrives after the release began: it waits until the release is complete.
        push_at(&sender, now + ms(5), [0x90, 1, 1]);
        let first = callback(&mut receiver, now, &stats);
        assert_eq!(first.len(), RELEASES_PER_CALLBACK as usize);
        assert!(first.iter().all(|&(_, [status, ..])| status & 0xf0 == 0x80));
        // The rest at frame 0 of the next callback, then the new note in its place.
        let second = callback(&mut receiver, now + ms(10), &stats);
        assert_eq!(second.len(), 300 - RELEASES_PER_CALLBACK as usize + 1);
        assert_eq!(second[second.len() - 2], (0, [0x82, 99, 0]));
        assert_eq!(second.last(), Some(&(240, [0x90, 1, 1])));
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
