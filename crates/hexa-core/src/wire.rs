//! Compact binary encoding of a [`Patch`] for links between devices (the web
//! UI's host process → the Raspberry Pi). Built on postcard, which needs
//! neither `std` nor allocation.
//!
//! Layout: one format version byte, then the postcard encoding of the patch.
//! Postcard is not self-describing, so both sides must be built from the same
//! `Patch` definition; bump [`VERSION`] whenever it changes.

use crate::Patch;

/// Format version, the first byte of every encoded patch.
pub const VERSION: u8 = 26;

/// Upper bound of an encoded patch's size in bytes.
pub const MAX_PATCH_BYTES: usize = 2048;

/// Samples per `@scope` telemetry line from the Pi (the output's newest,
/// 16 bit, 4 hex digits each).
pub const SCOPE_SAMPLES: usize = 1024;

/// Encodes `patch` into `buf` and returns the used length, or `None` if
/// `buf` is too small.
pub fn encode_patch(patch: &Patch, buf: &mut [u8]) -> Option<usize> {
    let (version, rest) = buf.split_first_mut()?;
    *version = VERSION;
    let used = postcard::to_slice(patch, rest).ok()?.len();
    Some(1 + used)
}

/// Decodes a patch written by [`encode_patch`]. Returns `None` for another
/// format version, malformed data or trailing bytes. The patch is not
/// sanitized.
pub fn decode_patch(bytes: &[u8]) -> Option<Patch> {
    let (&version, rest) = bytes.split_first()?;
    if version != VERSION {
        return None;
    }
    match postcard::take_from_bytes(rest) {
        Ok((patch, [])) => Some(patch),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets;

    #[test]
    fn presets_round_trip_within_size_bound() {
        let mut buf = [0u8; MAX_PATCH_BYTES];
        for preset in presets() {
            let len = encode_patch(&preset.patch, &mut buf).expect("patch too large");
            assert_eq!(
                decode_patch(&buf[..len]),
                Some(preset.patch),
                "{}",
                preset.name
            );
        }
    }

    #[test]
    fn fully_modulated_patch_with_all_lfos_fits() {
        use crate::{
            EnvelopeParams, LfoParams, MAX_ENVELOPES, MAX_LFOS, MAX_ROUTES, ModRoute, ModSource,
            ModTarget,
        };
        let mut patch = Patch::default();
        for _ in 0..MAX_LFOS {
            patch.lfos.push(LfoParams::default());
        }
        for _ in 0..MAX_ENVELOPES {
            patch.envelopes.push(EnvelopeParams::default());
        }
        // The largest encoding: indices and depths that need the most bytes.
        for i in 0..MAX_ROUTES {
            let route = ModRoute {
                source: ModSource::Lfo(MAX_LFOS - 1),
                target: ModTarget::LfoRate(MAX_LFOS - 1 - i % MAX_LFOS),
                depth: 1.0,
                via: ModSource::Envelope(MAX_ENVELOPES - 1),
            };
            assert!(patch.modulation.push(route).is_some());
        }
        patch.filters[0].kind = crate::FilterKind::LowPass;
        let mut buf = [0u8; MAX_PATCH_BYTES];
        let len = encode_patch(&patch, &mut buf).expect("patch too large");
        assert_eq!(decode_patch(&buf[..len]), Some(patch));
        println!("fully loaded patch: {len} bytes");
    }

    /// Fails when the encoding of `Patch` changes: then bump [`VERSION`]
    /// (host and kernel must be rebuilt together) and update the pair here.
    #[test]
    fn encoding_changes_bump_the_version() {
        const FINGERPRINT: (u8, u64) = (26, 0xc4b7_535c_591c_23ab);
        let mut buf = [0u8; MAX_PATCH_BYTES];
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for patch in [Patch::default(), crate::preset(1).unwrap().patch] {
            let len = encode_patch(&patch, &mut buf).unwrap();
            for &b in &buf[1..len] {
                hash = (hash ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        assert_eq!(
            (VERSION, hash),
            FINGERPRINT,
            "Patch encoding changed: bump wire::VERSION, then pin the new pair"
        );
    }

    #[test]
    fn rejects_wrong_version_truncation_and_trailing_bytes() {
        let mut buf = [0u8; MAX_PATCH_BYTES];
        let len = encode_patch(&Patch::default(), &mut buf).unwrap();
        assert!(decode_patch(&buf[..len - 1]).is_none());
        assert!(decode_patch(&buf[..len + 1]).is_none());
        assert!(decode_patch(&[]).is_none());
        buf[0] = VERSION + 1;
        assert!(decode_patch(&buf[..len]).is_none());
        assert!(encode_patch(&Patch::default(), &mut [0u8; 8]).is_none());
    }
}
