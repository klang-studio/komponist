//! The per-block COM objects of `IAudioProcessor::process`, allocated once at activation:
//!
//! - [`EventList`] (`IEventList`): note events in, plugin events out.
//! - [`ParameterChanges`] (`IParameterChanges`) with a fixed pool of [`ParamQueue`]s
//!   (`IParamValueQueue`): parameter changes in and out.
//! - [`MidiMap`]: the controller's `IMidiMapping` (MIDI controllers → parameters), shared with the
//!   processor through atomics so the main thread can refresh it while the RT thread reads it.
//!
//! None of these allocate after construction: lists have fixed capacities and drop what does not
//! fit (a full parameter queue keeps its last point up to date instead, so the final value of a
//! block always arrives).

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::sync::atomic::{AtomicU32, Ordering};

use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent, kPolyPressureEvent};
use vst3::Steinberg::Vst::{
    Event, Event__type0, IEventList, IEventListTrait, IMidiMapping, IMidiMappingTrait,
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait,
    NoteOffEvent, NoteOnEvent, ParamID, ParamValue, PolyPressureEvent,
};
use vst3::Steinberg::{int32, tresult};
use vst3::Steinberg::{kInvalidArgument, kResultFalse, kResultOk, kResultTrue};
use vst3::{Class, ComPtr, ComWrapper};

/// MIDI controller numbers the map covers: 0–127 CC, 128 channel pressure, 129 pitch bend
/// (VST3 `ControllerNumbers`).
pub(crate) const MIDI_CONTROLLERS: usize = 130;
/// VST3 controller number of channel pressure (`kAfterTouch`).
pub(crate) const CTRL_AFTERTOUCH: usize = 128;
/// VST3 controller number of pitch bend (`kPitchBend`).
pub(crate) const CTRL_PITCH_BEND: usize = 129;
const NO_PARAM: u32 = u32::MAX;

/// A COM object owned by one processor and touched only by the thread that runs it (and by the
/// plugin during `process` on that same thread).
pub(crate) struct RtCom<C: Class>(ComWrapper<C>);

// SAFETY: the wrapped object is used by one thread at a time: the main thread creates it, then the
// processor (and the plugin inside `process`) use it on the RT thread only. The reference count of
// `ComWrapper` is atomic, so a plugin that add-refs it elsewhere does not race with us.
unsafe impl<C: Class> Send for RtCom<C> {}

impl<C: Class + 'static> RtCom<C>
where
    C::Interfaces: vst3::com_scrape_types::MakeHeader<C, ComWrapper<C>>,
{
    pub(crate) fn new(object: C) -> RtCom<C> {
        RtCom(ComWrapper::new(object))
    }
}

impl<C: Class> RtCom<C> {
    /// The object as interface `I` (no reference added; valid while `self` lives).
    pub(crate) fn ptr<I: vst3::Interface>(&self) -> *mut I {
        self.0
            .as_com_ref::<I>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

impl<C: Class> Deref for RtCom<C> {
    type Target = C;

    fn deref(&self) -> &C {
        &self.0
    }
}

// ------------------------------------------------------------------------------------------------
// IEventList
// ------------------------------------------------------------------------------------------------

/// A fixed-capacity event list.
pub(crate) struct EventList {
    events: RefCell<Vec<Event>>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl EventList {
    pub(crate) fn with_capacity(capacity: usize) -> EventList {
        EventList {
            events: RefCell::new(Vec::with_capacity(capacity)),
        }
    }

    pub(crate) fn clear(&self) {
        self.events.borrow_mut().clear();
    }

    /// Appends `event`; `false` (dropped) when full.
    pub(crate) fn push(&self, event: Event) -> bool {
        let mut events = self.events.borrow_mut();
        if events.len() < events.capacity() {
            events.push(event);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.events.borrow().len()
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.events.borrow().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        match usize::try_from(index)
            .ok()
            .and_then(|i| self.events.borrow().get(i).copied())
        {
            Some(event) => {
                // SAFETY: valid out-pointer (checked for NULL).
                unsafe { *e = event };
                kResultOk
            }
            None => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        if e.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: the plugin passes a valid event.
        if self.push(unsafe { *e }) {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

fn event(offset: u32, kind: u32, body: Event__type0) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: offset as i32,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kind as u16,
        __field0: body,
    }
}

/// A note-on event (velocity 1–127 → 0..1; `note_id` −1 for none).
pub(crate) fn note_on(offset: u32, channel: u8, key: u8, velocity: u8, note_id: i32) -> Event {
    event(
        offset,
        kNoteOnEvent,
        Event__type0 {
            noteOn: NoteOnEvent {
                channel: i16::from(channel),
                pitch: i16::from(key),
                tuning: 0.0,
                velocity: f32::from(velocity) / 127.0,
                length: 0,
                noteId: note_id,
            },
        },
    )
}

/// A note-off event.
pub(crate) fn note_off(offset: u32, channel: u8, key: u8, velocity: u8, note_id: i32) -> Event {
    event(
        offset,
        kNoteOffEvent,
        Event__type0 {
            noteOff: NoteOffEvent {
                channel: i16::from(channel),
                pitch: i16::from(key),
                velocity: f32::from(velocity) / 127.0,
                noteId: note_id,
                tuning: 0.0,
            },
        },
    )
}

/// A polyphonic key pressure event.
pub(crate) fn poly_pressure(offset: u32, channel: u8, key: u8, pressure: u8) -> Event {
    event(
        offset,
        kPolyPressureEvent,
        Event__type0 {
            polyPressure: PolyPressureEvent {
                channel: i16::from(channel),
                pitch: i16::from(key),
                pressure: f32::from(pressure) / 127.0,
                noteId: -1,
            },
        },
    )
}

// ------------------------------------------------------------------------------------------------
// IParameterChanges, IParamValueQueue
// ------------------------------------------------------------------------------------------------

/// The changes of one parameter in one block, sorted by offset.
pub(crate) struct ParamQueue {
    id: Cell<ParamID>,
    points: RefCell<Vec<(i32, f64)>>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ParamQueue {
    fn with_capacity(points: usize) -> ParamQueue {
        ParamQueue {
            id: Cell::new(NO_PARAM),
            points: RefCell::new(Vec::with_capacity(points.max(1))),
        }
    }

    /// Inserts a point in offset order; a point at an existing offset replaces it. When full, the
    /// last point takes the value if it is not later than `offset` (so the final value survives).
    /// Returns the point's index, or `None` if it was dropped.
    fn add(&self, offset: i32, value: f64) -> Option<usize> {
        let mut points = self.points.borrow_mut();
        let at = points.partition_point(|(o, _)| *o < offset);
        if let Some(point) = points.get_mut(at)
            && point.0 == offset
        {
            point.1 = value;
            return Some(at);
        }
        if points.len() < points.capacity() {
            points.insert(at, (offset, value));
            return Some(at);
        }
        let last = points.len() - 1;
        if points[last].0 <= offset {
            points[last].1 = value;
            return Some(last);
        }
        None
    }

    /// The points, for reading output queues.
    pub(crate) fn points(&self) -> std::cell::Ref<'_, Vec<(i32, f64)>> {
        self.points.borrow()
    }

    pub(crate) fn id(&self) -> ParamID {
        self.id.get()
    }
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.points.borrow().len() as int32
    }

    unsafe fn getPoint(
        &self,
        index: int32,
        sample_offset: *mut int32,
        value: *mut ParamValue,
    ) -> tresult {
        let points = self.points.borrow();
        let Some(&(offset, v)) = usize::try_from(index).ok().and_then(|i| points.get(i)) else {
            return kInvalidArgument;
        };
        if sample_offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: valid out-pointers (checked for NULL).
        unsafe {
            *sample_offset = offset;
            *value = v;
        }
        kResultOk
    }

    unsafe fn addPoint(
        &self,
        sample_offset: int32,
        value: ParamValue,
        index: *mut int32,
    ) -> tresult {
        match self.add(sample_offset, value) {
            Some(i) => {
                if !index.is_null() {
                    // SAFETY: valid out-pointer (checked for NULL).
                    unsafe { *index = i as int32 };
                }
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

/// A fixed pool of parameter queues.
pub(crate) struct ParameterChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    used: Cell<usize>,
}

impl Class for ParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParameterChanges {
    /// Room for `params` distinct parameters with `points` points each per block.
    pub(crate) fn new(params: usize, points: usize) -> ParameterChanges {
        ParameterChanges {
            queues: (0..params)
                .map(|_| ComWrapper::new(ParamQueue::with_capacity(points)))
                .collect(),
            used: Cell::new(0),
        }
    }

    pub(crate) fn clear(&self) {
        for queue in &self.queues[..self.used.get()] {
            queue.points.borrow_mut().clear();
            queue.id.set(NO_PARAM);
        }
        self.used.set(0);
    }

    /// The queue of `id`, added if needed; `None` when the pool is exhausted.
    fn queue(&self, id: ParamID) -> Option<(usize, &ComWrapper<ParamQueue>)> {
        let used = self.used.get();
        if let Some(found) = self.queues[..used].iter().position(|q| q.id.get() == id) {
            return Some((found, &self.queues[found]));
        }
        let queue = self.queues.get(used)?;
        queue.id.set(id);
        self.used.set(used + 1);
        Some((used, queue))
    }

    /// Adds a change of `id` to `value` at `offset`; `false` if it was dropped.
    pub(crate) fn add(&self, id: ParamID, offset: u32, value: f64) -> bool {
        self.queue(id)
            .is_some_and(|(_, q)| q.add(offset as i32, value).is_some())
    }

    /// The queues in use.
    pub(crate) fn queues(&self) -> &[ComWrapper<ParamQueue>] {
        &self.queues[..self.used.get()]
    }
}

impl IParameterChangesTrait for ParameterChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.get() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        // Like the SDK's ParameterChanges: no reference is added.
        usize::try_from(index)
            .ok()
            .and_then(|i| self.queues().get(i))
            .and_then(|q| q.as_com_ref::<IParamValueQueue>())
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: valid pointer (checked for NULL).
        let Some((i, queue)) = self.queue(unsafe { *id }) else {
            return std::ptr::null_mut();
        };
        if !index.is_null() {
            // SAFETY: valid out-pointer (checked for NULL).
            unsafe { *index = i as int32 };
        }
        queue
            .as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |q| q.as_ptr())
    }
}

// ------------------------------------------------------------------------------------------------
// IMidiMapping
// ------------------------------------------------------------------------------------------------

/// MIDI controller → parameter assignments of event bus 0, per channel.
pub(crate) struct MidiMap {
    entries: Box<[AtomicU32]>,
}

impl MidiMap {
    pub(crate) fn new() -> MidiMap {
        MidiMap {
            entries: (0..16 * MIDI_CONTROLLERS)
                .map(|_| AtomicU32::new(NO_PARAM))
                .collect(),
        }
    }

    /// Re-reads the assignments (main thread); without `IMidiMapping` there are none.
    pub(crate) fn refresh(&self, mapping: Option<&ComPtr<IMidiMapping>>) {
        for channel in 0..16usize {
            for controller in 0..MIDI_CONTROLLERS {
                let mut id: ParamID = NO_PARAM;
                let found = mapping.is_some_and(|m| {
                    // SAFETY: the controller's IMidiMapping, on the main thread.
                    let result = unsafe {
                        m.getMidiControllerAssignment(0, channel as i16, controller as i16, &mut id)
                    };
                    result == kResultTrue
                });
                self.entries[channel * MIDI_CONTROLLERS + controller]
                    .store(if found { id } else { NO_PARAM }, Ordering::Relaxed);
            }
        }
    }

    /// The parameter of `controller` on `channel`.
    pub(crate) fn get(&self, channel: u8, controller: usize) -> Option<ParamID> {
        if channel >= 16 || controller >= MIDI_CONTROLLERS {
            return None;
        }
        let id = self.entries[usize::from(channel) * MIDI_CONTROLLERS + controller]
            .load(Ordering::Relaxed);
        (id != NO_PARAM).then_some(id)
    }

    /// Sets one assignment (tests).
    #[cfg(test)]
    pub(crate) fn set(&self, channel: u8, controller: usize, id: ParamID) {
        self.entries[usize::from(channel) * MIDI_CONTROLLERS + controller]
            .store(id, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_list_is_bounded() {
        let list = RtCom::new(EventList::with_capacity(2));
        let ptr: *mut IEventList = list.ptr();
        let com = unsafe { vst3::ComRef::from_raw(ptr) }.unwrap();
        assert!(list.push(note_on(3, 1, 60, 127, -1)));
        let mut e = note_off(0, 0, 0, 0, -1);
        assert_eq!(unsafe { com.addEvent(&mut e) }, kResultOk);
        assert_eq!(unsafe { com.addEvent(&mut e) }, kResultFalse);
        assert_eq!(unsafe { com.getEventCount() }, 2);
        let mut out = note_off(9, 9, 9, 9, 9);
        assert_eq!(unsafe { com.getEvent(0, &mut out) }, kResultOk);
        assert_eq!((out.sampleOffset, out.r#type), (3, kNoteOnEvent as u16));
        let on = unsafe { out.__field0.noteOn };
        assert_eq!(
            (on.channel, on.pitch, on.velocity, on.noteId),
            (1, 60, 1.0, -1)
        );
        assert_eq!(unsafe { com.getEvent(2, &mut out) }, kInvalidArgument);
        list.clear();
        assert_eq!(list.len(), 0);
        let pp = poly_pressure(1, 2, 64, 127);
        assert_eq!(pp.r#type, kPolyPressureEvent as u16);
        assert_eq!(unsafe { pp.__field0.polyPressure.pressure }, 1.0);
    }

    #[test]
    fn parameter_changes_pool_and_queues() {
        let changes = RtCom::new(ParameterChanges::new(2, 2));
        let com = unsafe { vst3::ComRef::from_raw(changes.ptr::<IParameterChanges>()) }.unwrap();
        assert!(changes.add(7, 10, 0.5));
        assert!(changes.add(7, 0, 0.25));
        // A third point: the queue is full, the last point (offset 10) takes the later value.
        assert!(changes.add(7, 20, 0.75));
        assert!(
            !changes.add(7, 5, 0.1),
            "earlier than the last point of a full queue"
        );
        assert!(changes.add(8, 0, 1.0));
        assert!(!changes.add(9, 0, 1.0), "pool exhausted");
        assert_eq!(unsafe { com.getParameterCount() }, 2);
        let q = unsafe { vst3::ComRef::from_raw(com.getParameterData(0)) }.unwrap();
        assert_eq!(unsafe { q.getParameterId() }, 7);
        assert_eq!(unsafe { q.getPointCount() }, 2);
        let (mut offset, mut value) = (0, 0.0);
        assert_eq!(unsafe { q.getPoint(1, &mut offset, &mut value) }, kResultOk);
        assert_eq!((offset, value), (10, 0.75));
        assert!(unsafe { com.getParameterData(2) }.is_null());

        // The plugin's side: addParameterData finds or adds a queue, addPoint keeps order.
        changes.clear();
        assert_eq!(unsafe { com.getParameterCount() }, 0);
        let mut index = -1;
        let q = unsafe { com.addParameterData(&3, &mut index) };
        assert_eq!(index, 0);
        let q = unsafe { vst3::ComRef::from_raw(q) }.unwrap();
        let mut at = -1;
        assert_eq!(unsafe { q.addPoint(8, 0.4, &mut at) }, kResultOk);
        assert_eq!(unsafe { q.addPoint(2, 0.2, &mut at) }, kResultOk);
        assert_eq!(at, 0);
        assert_eq!(
            unsafe { q.addPoint(2, 0.3, &mut at) },
            kResultOk,
            "replaces"
        );
        let same = unsafe { com.addParameterData(&3, &mut index) };
        assert_eq!((same, index), (q.as_ptr(), 0));
        let queues = changes.queues();
        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0].id(), 3);
        assert_eq!(*queues[0].points(), [(2, 0.3), (8, 0.4)]);
    }

    #[test]
    fn midi_map_lookups() {
        let map = MidiMap::new();
        assert_eq!(map.get(0, 1), None);
        map.set(3, CTRL_PITCH_BEND, 42);
        assert_eq!(map.get(3, CTRL_PITCH_BEND), Some(42));
        assert_eq!(map.get(16, 1), None);
        assert_eq!(map.get(0, MIDI_CONTROLLERS), None);
        map.refresh(None);
        assert_eq!(map.get(3, CTRL_PITCH_BEND), None);
        let _ = CTRL_AFTERTOUCH;
    }
}
