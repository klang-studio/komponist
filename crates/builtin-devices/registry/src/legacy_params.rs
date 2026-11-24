//! Parameter compatibility for projects that still refer to linked-in devices by name.

use komponist_device_core::StateError;
use komponist_hexa::{self as hexa, Macro, device::Field};
use komponist_i18n::Lang;
use komponist_model::Device;
use komponist_model::params::{ParamId, ParamInfo};

use crate::BuiltinKind;

/// One legacy parameter, addressed by its project key and its processor wire ID.
pub struct LegacyParameter {
    pub info: ParamInfo,
    pub wire: u32,
    pub value: f64,
}

/// Descriptor and state access for the linked-in processors used by older projects.
pub struct LegacyParameters<'a> {
    kind: BuiltinKind,
    state: Option<&'a [u8]>,
}

impl<'a> LegacyParameters<'a> {
    pub fn new(device: &'a Device) -> Option<Self> {
        Some(Self {
            kind: BuiltinKind::from_plugin(&device.plugin)?,
            state: device.state.as_deref(),
        })
    }

    pub fn wire(&self, id: &ParamId) -> Option<u32> {
        match self.kind {
            BuiltinKind::Hexa => hexa::device::param(id).map(|p| p.wire).or_else(|| {
                Macro::ALL
                    .into_iter()
                    .find(|m| m.key() == id.as_str())
                    .map(Macro::id)
            }),
            BuiltinKind::Internal(kind) => kind.wire(id),
        }
    }

    pub fn id(&self, wire: u32) -> Option<ParamId> {
        match self.kind {
            BuiltinKind::Hexa => Some(ParamId::new(Field::from_wire(wire)?.key())),
            BuiltinKind::Internal(kind) => Some(kind.params().get(wire as usize)?.id.clone()),
        }
    }

    pub fn info(&self, lang: Lang, wire: u32) -> Option<ParamInfo> {
        match self.kind {
            BuiltinKind::Hexa => match Field::from_wire(wire)? {
                Field::Macro(m) => Some(m.param_info_in(lang)),
                field => Some(
                    hexa::device::param_in(lang, &ParamId::new(field.key()))?
                        .info
                        .clone(),
                ),
            },
            BuiltinKind::Internal(kind) => kind.params_in(lang).get(wire as usize).cloned(),
        }
    }

    pub fn get(&self, lang: Lang, id: &ParamId) -> Option<LegacyParameter> {
        let wire = self.wire(id)?;
        let info = self.info(lang, wire)?;
        let value = match self.kind {
            BuiltinKind::Hexa => {
                let patch =
                    hexa::decode_state(self.state).unwrap_or_else(|_| hexa::default_patch());
                Field::from_wire(wire)?.get(&patch)
            }
            BuiltinKind::Internal(kind) => kind
                .decode_state(self.state)
                .ok()
                .map_or(info.default, |values| values[wire as usize]),
        };
        Some(LegacyParameter { info, wire, value })
    }

    pub fn all(&self, lang: Lang) -> Vec<LegacyParameter> {
        match self.kind {
            BuiltinKind::Hexa => {
                let patch =
                    hexa::decode_state(self.state).unwrap_or_else(|_| hexa::default_patch());
                hexa::device::params_in(lang)
                    .iter()
                    .map(|p| LegacyParameter {
                        info: p.info.clone(),
                        wire: p.wire,
                        value: p.field.get(&patch),
                    })
                    .chain(Macro::ALL.map(|m| LegacyParameter {
                        info: m.param_info_in(lang),
                        wire: m.id(),
                        value: m.get(&patch),
                    }))
                    .collect()
            }
            BuiltinKind::Internal(kind) => kind
                .params_in(lang)
                .iter()
                .cloned()
                .zip(
                    kind.decode_state(self.state)
                        .unwrap_or_else(|_| kind.defaults()),
                )
                .enumerate()
                .map(|(wire, (info, value))| LegacyParameter {
                    info,
                    wire: wire as u32,
                    value,
                })
                .collect(),
        }
    }

    /// Parameter writes that update an existing processor without resetting its voices or tails.
    /// A state change outside the parameter model requires a fresh processor.
    pub fn changes_from(&self, previous: Option<&[u8]>) -> Option<Vec<(u32, f64)>> {
        match self.kind {
            BuiltinKind::Hexa => hexa::device::param_changes(
                &hexa::decode_state(previous).ok()?,
                &hexa::decode_state(self.state).ok()?,
            )
            .filter(|changes| changes.len() <= komponist_engine::MAX_PENDING_PARAMS),
            BuiltinKind::Internal(kind) => {
                let decode = |state| match kind.decode_state(state) {
                    Err(StateError::NoParams) => Ok(kind.defaults()),
                    other => other,
                };
                let was = decode(previous).ok()?;
                let now = decode(self.state).ok()?;
                Some(
                    was.into_iter()
                        .zip(now)
                        .enumerate()
                        .filter(|(_, (a, b))| a != b)
                        .map(|(id, (_, value))| (id as u32, value))
                        .collect(),
                )
            }
        }
    }

    /// Returns the edited state, preserving unreadable original bytes on failure.
    pub fn with_value(&self, wire: u32, value: f64) -> Option<Vec<u8>> {
        match self.kind {
            BuiltinKind::Hexa => {
                let mut patch = hexa::decode_state(self.state).ok()?;
                Field::from_wire(wire)?.set(&mut patch, value);
                Some(hexa::encode_state(&patch))
            }
            BuiltinKind::Internal(kind) => kind.with_value(self.state, wire, value),
        }
    }
}
