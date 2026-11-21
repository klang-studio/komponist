//! Persistent parameter values for authenticated bundles using the pinned CLAP wrapper.

use std::collections::BTreeMap;

use komponist_builtin::BuiltinKind;
use komponist_device_core::pinned_clap_state;
use komponist_ipc::native_layout::NativeLayout;
use komponist_model::params::ParamId;

pub(crate) fn kind_for_id(id: &str) -> Option<BuiltinKind> {
    BuiltinKind::all().find(|&kind| crate::legacy_bundled::clap_id(kind) == id)
}

/// Restore the persistent values of known automation targets in a captured snapshot.
/// The live processor, unrelated parameter entries and all wrapper fields are left untouched.
pub(crate) fn restore_param_bases(
    kind: BuiltinKind,
    layout: &NativeLayout,
    state: &[u8],
    corrections: impl IntoIterator<Item = (u32, f64)>,
) -> Option<Vec<u8>> {
    let mut corrections: BTreeMap<_, _> = corrections.into_iter().collect();
    if corrections.is_empty() {
        return Some(state.to_vec());
    }

    let mut document = pinned_clap_state::decode(state)?;
    let params = document.get_mut("params")?.as_object_mut()?;
    for (author, wire) in layout.author_ids()? {
        let Some(value) = corrections.remove(wire) else {
            continue;
        };
        let param = ParamId::new(author);
        let info = match kind {
            BuiltinKind::Hexa => &komponist_hexa::device::param(&param)?.info,
            BuiltinKind::Internal(kind) => {
                &kind.params()[usize::try_from(kind.wire(&param)?).ok()?]
            }
        };
        let (min, max) = pinned_clap_state::clap_range(info);
        if !(min..=max).contains(&value) {
            return None;
        }
        let persistent = pinned_clap_state::from_clap(info, value);
        // The pinned wrapper also accepts ordinal values for enums with stable string IDs.
        // The live snapshot's enum string cannot be reused when it names an automated value.
        params.insert(
            author.clone(),
            pinned_clap_state::parameter(info, persistent).0,
        );
    }
    if !corrections.is_empty() {
        return None;
    }

    Some(pinned_clap_state::encode_document(&document))
}
