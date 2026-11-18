//! Owns the JACK client and its callbacks through partial registration and activation failures.

use std::cell::UnsafeCell;
use std::ffi::{c_int, c_void};

use jack::{Client, Control, ProcessScope};
use jack_sys as ffi;

use super::{Duplex, Notifications};
use crate::{AudioError, AudioProcess};

struct CallbackOwner<C, T> {
    // Client::drop calls jack_client_close, which stops JACK callbacks before their state is freed.
    client: C,
    state: Box<T>,
}

impl<C, T> CallbackOwner<C, T> {
    fn try_activate(
        self,
        activate: impl FnOnce(&C, &T) -> Result<(), AudioError>,
    ) -> Result<Self, AudioError> {
        activate(&self.client, &self.state)?;
        Ok(self)
    }
}

struct Callbacks<P: AudioProcess> {
    client: *mut ffi::jack_client_t,
    // JACK serializes process calls for a client. Other callbacks use notifications only.
    duplex: UnsafeCell<Duplex<P>>,
    notifications: Notifications,
}

// SAFETY: moving the registered pointer does not move its boxed storage. JACK calls the process
// handler one at a time, and notifications access their fields only through atomics.
unsafe impl<P: AudioProcess> Send for Callbacks<P> {}

pub(super) struct ActiveClient<P: AudioProcess>(CallbackOwner<Client, Callbacks<P>>);

impl<P: AudioProcess + 'static> ActiveClient<P> {
    pub(super) fn new(
        client: Client,
        notifications: Notifications,
        duplex: Duplex<P>,
    ) -> Result<Self, AudioError> {
        let state = Box::new(Callbacks {
            client: client.raw(),
            duplex: UnsafeCell::new(duplex),
            notifications,
        });
        CallbackOwner { client, state }
            .try_activate(|client, state| {
                let raw = client.raw();
                let data = std::ptr::from_ref(state).cast_mut().cast();
                // SAFETY: both owners stay live during registration, activation and any callback.
                // On an error the client is closed before the boxed callbacks are dropped.
                let status = unsafe {
                    ffi::jack_on_shutdown(raw, Some(shutdown::<P>), data);
                    [
                        ffi::jack_set_process_callback(raw, Some(process::<P>), data),
                        ffi::jack_set_buffer_size_callback(raw, Some(buffer_size::<P>), data),
                        ffi::jack_set_sample_rate_callback(raw, Some(sample_rate::<P>), data),
                        ffi::jack_set_port_registration_callback(
                            raw,
                            Some(port_registration::<P>),
                            data,
                        ),
                        ffi::jack_set_port_connect_callback(raw, Some(ports_connected::<P>), data),
                        ffi::jack_set_graph_order_callback(raw, Some(graph_reorder::<P>), data),
                        ffi::jack_set_xrun_callback(raw, Some(xrun::<P>), data),
                    ]
                    .into_iter()
                    .find(|result| *result != 0)
                };
                if let Some(status) = status {
                    return Err(AudioError::Backend(format!(
                        "JACK: callback registration failed ({status})"
                    )));
                }
                // SAFETY: every callback is registered and its state has a stable address.
                let status = unsafe { ffi::jack_activate(raw) };
                if status != 0 {
                    return Err(AudioError::Backend(format!(
                        "JACK: client activation failed ({status})"
                    )));
                }
                Ok(())
            })
            .map(Self)
    }

    pub(super) fn as_client(&self) -> &Client {
        &self.0.client
    }
}

// SAFETY: JACK supplies the same non-null data registered by ActiveClient::new.
unsafe fn callback<'a, P: AudioProcess>(data: *mut c_void) -> &'a Callbacks<P> {
    unsafe { &*data.cast::<Callbacks<P>>() }
}

unsafe extern "C" fn process<P: AudioProcess + 'static>(frames: u32, data: *mut c_void) -> c_int {
    // SAFETY: JACK runs a single process callback at a time and supplies a scope for this period.
    let state = unsafe { callback::<P>(data) };
    let scope = unsafe { ProcessScope::from_raw(frames, state.client) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { &mut *state.duplex.get() }.process(&scope)
    }));
    match result {
        Ok(Control::Continue) => 0,
        Ok(Control::Quit) => -1,
        Err(error) => {
            state.notifications.shutdown();
            // No potentially user-defined panic destructor on JACK's process thread.
            std::mem::forget(error);
            -1
        }
    }
}

unsafe extern "C" fn shutdown<P: AudioProcess>(data: *mut c_void) {
    unsafe { callback::<P>(data) }.notifications.shutdown();
}

unsafe extern "C" fn sample_rate<P: AudioProcess>(rate: u32, data: *mut c_void) -> c_int {
    unsafe { callback::<P>(data) }
        .notifications
        .sample_rate(rate);
    0
}

unsafe extern "C" fn buffer_size<P: AudioProcess>(frames: u32, data: *mut c_void) -> c_int {
    unsafe { callback::<P>(data) }
        .notifications
        .buffer_size(frames);
    0
}

unsafe extern "C" fn port_registration<P: AudioProcess>(_: u32, _: c_int, data: *mut c_void) {
    unsafe { callback::<P>(data) }.notifications.graph_changed();
}

unsafe extern "C" fn ports_connected<P: AudioProcess>(
    a: u32,
    b: u32,
    connected: c_int,
    data: *mut c_void,
) {
    let state = unsafe { callback::<P>(data) };
    state.notifications.ports_connected(a, b, connected != 0);
}

unsafe extern "C" fn graph_reorder<P: AudioProcess>(data: *mut c_void) -> c_int {
    unsafe { callback::<P>(data) }.notifications.graph_changed();
    0
}

unsafe extern "C" fn xrun<P: AudioProcess>(data: *mut c_void) -> c_int {
    unsafe { callback::<P>(data) }.notifications.xrun();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jack_activation_failure_closes_before_reclaiming_callback_state() {
        use std::cell::RefCell;
        struct DropStep<'a>(&'a RefCell<Vec<&'static str>>, &'static str);
        impl Drop for DropStep<'_> {
            fn drop(&mut self) {
                self.0.borrow_mut().push(self.1);
            }
        }

        let steps = RefCell::new(Vec::new());
        let owner = CallbackOwner {
            client: DropStep(&steps, "client closed"),
            state: Box::new(DropStep(&steps, "callback freed")),
        };
        assert!(
            owner
                .try_activate(|_, _| Err(AudioError::Backend("activation failed".into())))
                .is_err()
        );
        assert_eq!(&*steps.borrow(), &["client closed", "callback freed"]);
    }
}
