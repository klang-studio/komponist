//! The device's audio workgroup (macOS, `os_workgroup`; #49, #45).
//!
//! On Apple silicon the scheduler places realtime threads on the right cores and clocks only when
//! they belong to the device's IO workgroup (`kAudioDevicePropertyIOThreadOSWorkgroup`; spike #43,
//! internal notes). The HAL's IO thread, on which cpal calls the stream callback, is a member
//! already, so the callback does not join (a second join returns `EALREADY`). The threads that
//! work to the callback's deadline besides it, the engine's workers, join it: [`lookup`] runs when
//! a stream is built (non-RT, UI thread) and the handle travels in
//! [`crate::StreamParams::workgroup`] to [`crate::AudioProcess::prepare`], where the app hands it
//! to the worker pool. The workers leave it when the pool is replaced or dropped, which happens
//! when the next stream is prepared or the engine goes away.
//!
//! The property is readable without starting IO. Each stream looks it up again, for the device the
//! stream actually opened.

use komponist_rt::Workgroup;

/// Non-RT. The IO workgroup of `device`, retained; `None` where there is none (Linux, a device
/// CoreAudio no longer knows, a refused property read).
pub(crate) fn lookup(device: &cpal::Device) -> Option<Workgroup> {
    #[cfg(target_os = "macos")]
    {
        use cpal::traits::DeviceTrait;
        // cpal's CoreAudio device id is the device UID; its `AudioObjectID` is not public.
        let id = device.id().ok()?;
        sys::io_workgroup(sys::device_by_uid(id.id())?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = device;
        None
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use core::ffi::c_void;

    use komponist_rt::Workgroup;

    /// `AudioObjectID` (`<CoreAudio/AudioHardwareBase.h>`).
    pub(super) type AudioObjectId = u32;

    /// `AudioObjectPropertyAddress`.
    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    /// `kAudioObjectSystemObject`.
    const SYSTEM_OBJECT: AudioObjectId = 1;
    /// `kAudioObjectUnknown`.
    const UNKNOWN: AudioObjectId = 0;
    /// `kAudioHardwarePropertyTranslateUIDToDevice` (macOS 14+): UID (CFString qualifier) → device.
    const TRANSLATE_UID_TO_DEVICE: u32 = u32::from_be_bytes(*b"uidd");
    /// `kAudioDevicePropertyIOThreadOSWorkgroup`: the IO thread's `os_workgroup_t`, retained for
    /// the caller.
    const IO_THREAD_OS_WORKGROUP: u32 = u32::from_be_bytes(*b"oswg");
    /// `kAudioObjectPropertyScopeGlobal`.
    const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    /// `kAudioObjectPropertyElementMain`.
    const ELEMENT_MAIN: u32 = 0;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            object: AudioObjectId,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    /// Reads property `selector` of `object` into a `T` (with an optional qualifier).
    ///
    /// # Safety
    ///
    /// `T` must be the property's data type; `qualifier` (if not null) must be what the property
    /// takes as qualifier, `qualifier_size` bytes.
    unsafe fn read<T: Copy>(
        object: AudioObjectId,
        selector: u32,
        qualifier: *const c_void,
        qualifier_size: u32,
        empty: T,
    ) -> Option<T> {
        let address = PropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        };
        let mut value = empty;
        let mut size = size_of::<T>() as u32;
        // SAFETY: `address` and `value` live for the call and `size` is the size of `value`; the
        // caller guarantees the type and the qualifier.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address,
                qualifier_size,
                qualifier,
                &mut size,
                (&raw mut value).cast(),
            )
        };
        (status == 0 && size as usize == size_of::<T>()).then_some(value)
    }

    /// The device with this UID, `None` if CoreAudio knows none.
    pub(super) fn device_by_uid(uid: &str) -> Option<AudioObjectId> {
        let len = isize::try_from(uid.len()).ok()?;
        // SAFETY: `uid` is valid UTF-8 of `len` bytes; null is the default allocator. The string
        // is released below.
        let string =
            unsafe { CFStringCreateWithBytes(core::ptr::null(), uid.as_ptr(), len, UTF8, 0) };
        if string.is_null() {
            return None;
        }
        // SAFETY: the property takes a CFStringRef as qualifier and returns an AudioObjectID.
        let device = unsafe {
            read(
                SYSTEM_OBJECT,
                TRANSLATE_UID_TO_DEVICE,
                (&raw const string).cast(),
                size_of::<*const c_void>() as u32,
                UNKNOWN,
            )
        };
        // SAFETY: `string` came from a Create function: this code owns its reference.
        unsafe { CFRelease(string) };
        device.filter(|&id| id != UNKNOWN)
    }

    /// The IO workgroup of `device`, `None` if it has none.
    pub(super) fn io_workgroup(device: AudioObjectId) -> Option<Workgroup> {
        // SAFETY: the property returns an `os_workgroup_t` (a pointer) without qualifier.
        let raw = unsafe {
            read(
                device,
                IO_THREAD_OS_WORKGROUP,
                core::ptr::null(),
                0,
                core::ptr::null_mut::<c_void>(),
            )
        }?;
        // SAFETY: the header says the caller owns (and must release) the returned workgroup.
        unsafe { Workgroup::from_retained(raw) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn an_unknown_uid_has_no_device() {
            assert_eq!(device_by_uid("komponist-no-such-device"), None);
            assert!(io_workgroup(UNKNOWN).is_none());
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use cpal::traits::HostTrait;

    /// The default output device's IO workgroup is readable without starting IO (spike #43), and
    /// a thread can join and leave it. Skipped on a Mac without an output device.
    #[test]
    fn the_default_device_has_a_joinable_workgroup() {
        let Some(device) = cpal::default_host().default_output_device() else {
            eprintln!("skipped: no default output device");
            return;
        };
        let workgroup = lookup(&device).expect("CoreAudio devices have an IO workgroup");
        // Each lookup is its own reference; dropping one leaves the other usable.
        drop(lookup(&device).expect("second lookup"));
        std::thread::spawn(move || {
            let token = workgroup.join().expect("join");
            workgroup.leave(token);
        })
        .join()
        .unwrap();
    }
}
