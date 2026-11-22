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
            BuiltinKind::Hexa => komponist_hexa::Macro::ALL
                .into_iter()
                .find(|mac| mac.key() == author)
                .map(komponist_hexa::Macro::param_info)
                .or_else(|| komponist_hexa::device::param(&param).map(|card| card.info.clone()))?,
            BuiltinKind::Internal(kind) => {
                kind.params()[usize::try_from(kind.wire(&param)?).ok()?].clone()
            }
        };
        let (min, max) = pinned_clap_state::clap_range(&info);
        if !(min..=max).contains(&value) {
            return None;
        }
        let persistent = pinned_clap_state::from_clap(&info, value);
        // The pinned wrapper also accepts ordinal values for enums with stable string IDs.
        // The live snapshot's enum string cannot be reused when it names an automated value.
        params.insert(
            author.clone(),
            pinned_clap_state::parameter(&info, persistent).0,
        );
    }
    if !corrections.is_empty() {
        return None;
    }

    Some(pinned_clap_state::encode_document(&document))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_builtin::Internal;
    use serde_json::json;

    #[test]
    fn only_known_automation_parameters_are_replaced_in_the_saved_state() {
        let layout = NativeLayout::parse(
            br#"{
                "schema_version":1, "default_locale":"en", "locales":["en"],
                "author_ids":{"b1_freq":42,"b1_type":7},
                "pages":[{"key":"main","labels":{"en":"Main"},"columns":1,
                          "cells":[{"col":0,"row":0,"cols":1,"rows":1,"widget":"knob","param_id":42}]}]
            }"#,
        )
        .unwrap();
        let document = json!({
            "version":"0.1.0",
            "params":{
                "b1_type":{"string":"high-shelf"}, "b1_freq":{"f32":0.19}, "b2_gain":{"f32":0.75}
            },
            "fields":{"factory_preset":"\"Telephone\"","other":"{\"authored\":true}"}
        });
        let state = pinned_clap_state::encode_document(&document);
        let kind = BuiltinKind::Internal(Internal::Eq);
        let corrected =
            restore_param_bases(kind, &layout, &state, [(42, 0.625), (7, 2.0)]).unwrap();
        let mut expected = document;
        expected["params"]["b1_type"] = json!({"i32":2});
        expected["params"]["b1_freq"] = json!({"f32":0.625});
        assert_eq!(pinned_clap_state::decode(&corrected), Some(expected));
        assert!(restore_param_bases(kind, &layout, &state, [(99, 0.4)]).is_none());
        assert!(restore_param_bases(kind, &layout, &state, [(42, f64::NAN)]).is_none());
    }
    #[test]
    fn persistent_state_repairs_every_exposed_hexa_macro() {
        use komponist_hexa::Macro;

        let author_ids: BTreeMap<_, _> = Macro::ALL
            .into_iter()
            .map(|mac| (mac.key(), mac.id()))
            .collect();
        let layout = NativeLayout::parse(&serde_json::to_vec(&json!({
            "schema_version":1, "default_locale":"en", "locales":["en"],
            "author_ids":author_ids,
            "pages":[{"key":"main","labels":{"en":"Main"},"columns":1,
                      "cells":[{"col":0,"row":0,"cols":1,"rows":1,"widget":"knob","param_id":0}]}]
        })).unwrap()).unwrap();
        let applied = Macro::ALL.map(|mac| (mac.id(), 1.0));
        let state =
            komponist_hexa::state::encode_clap_patch(komponist_hexa::default_patch(), "0.1.0");
        let restored = restore_param_bases(BuiltinKind::Hexa, &layout, &state, applied).unwrap();
        let document = pinned_clap_state::decode(&restored).unwrap();
        for mac in Macro::ALL {
            let info = mac.param_info();
            let plain = pinned_clap_state::from_clap(&info, 1.0);
            assert_eq!(
                document["params"][mac.key()],
                pinned_clap_state::parameter(&info, plain).0,
                "{}",
                mac.key()
            );
        }
    }
}
