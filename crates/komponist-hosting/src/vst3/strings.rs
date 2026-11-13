//! VST3 string fields: fixed-size `char8` (C) and `char16` (UTF-16) arrays.

use std::ffi::c_char;

/// A NUL-terminated `char8` array as UTF-8 (lossy), cut at the first NUL.
pub(crate) fn c_chars(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A NUL-terminated UTF-16 array (`String128`, `TChar`s) as a string (lossy).
pub(crate) fn utf16(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

/// Writes `text` as NUL-terminated UTF-16 into `out`, truncated to fit.
pub(crate) fn write_utf16(text: &str, out: &mut [u16]) {
    let Some(room) = out.len().checked_sub(1) else {
        return;
    };
    let mut len = 0;
    for (slot, unit) in out[..room].iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
        len += 1;
    }
    // Do not end on the first half of a surrogate pair.
    if len > 0 && (0xD800..0xDC00).contains(&out[len - 1]) {
        len -= 1;
    }
    out[len] = 0;
}

/// Reads a NUL-terminated UTF-16 string from a raw pointer (at most `max` units).
///
/// # Safety
/// `ptr` is NULL or points to a NUL-terminated (or at least `max` long) UTF-16 string.
pub(crate) unsafe fn utf16_ptr(ptr: *const u16, max: usize) -> Vec<u16> {
    let mut units = Vec::new();
    if ptr.is_null() {
        return units;
    }
    for i in 0..max {
        // SAFETY: within the string (caller's contract).
        let unit = unsafe { *ptr.add(i) };
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char8_and_utf16_fields() {
        let mut field = [0 as c_char; 8];
        for (slot, b) in field.iter_mut().zip(b"Fx|Delay") {
            *slot = *b as c_char;
        }
        assert_eq!(c_chars(&field), "Fx|Delay");
        field[2] = 0;
        assert_eq!(c_chars(&field), "Fx");

        let mut wide = [0xFFFFu16; 6];
        write_utf16("Gäin", &mut wide);
        assert_eq!(utf16(&wide), "Gäin");
        write_utf16("abcdefgh", &mut wide);
        assert_eq!(utf16(&wide), "abcde");
        // A surrogate pair that does not fit is dropped whole.
        write_utf16("abcd🎹", &mut wide);
        assert_eq!(utf16(&wide), "abcd");
        write_utf16("x", &mut []);
        assert_eq!(utf16(&[0x41, 0x42]), "AB");

        let text: Vec<u16> = "Hi\0rest".encode_utf16().collect();
        assert_eq!(unsafe { utf16_ptr(text.as_ptr(), 10) }, [0x48, 0x69]);
        assert_eq!(unsafe { utf16_ptr(text.as_ptr(), 1) }, [0x48]);
        assert!(unsafe { utf16_ptr(std::ptr::null(), 4) }.is_empty());
    }
}
