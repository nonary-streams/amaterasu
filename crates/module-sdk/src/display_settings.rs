//! Versioned display adjustments. Providers own native state and durable restoration.
//! A temporary adjustment needs a source-owner scope; possession of an ID alone
//! does not authorize mutation. Native plugins remain trusted code.
use crate::{
    ApiFieldKind as Kind, ApiMethod, Command, ModuleResult, operation_api, requests::RequestOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod preparation;

pub const SERVICE: &str = "nonary.display-settings/1";
pub const MAX_SCOPES: usize = 32;
pub const MAX_LEASES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub generation: String,
}
impl Target {
    pub fn validate(&self) -> ModuleResult<()> {
        if valid_id(&self.id) && valid_id(&self.generation) {
            Ok(())
        } else {
            Err("invalid_display_target".into())
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrMode {
    Disabled,
    Enabled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}
/// An exact scale ratio, so restoring a native fractional scale does not round it.
#[derive(Clone, Copy, Debug, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scale {
    pub numerator: u64,
    pub denominator: u64,
}
impl PartialEq for Scale {
    fn eq(&self, other: &Self) -> bool {
        u128::from(self.numerator) * u128::from(other.denominator)
            == u128::from(other.numerator) * u128::from(self.denominator)
    }
}
impl Scale {
    pub fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Adjustment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<HdrMode>,
    /// Wide color gamut is independent of HDR. SDR processors may need both off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wide_color_gamut: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    /// Exact refresh in millihertz, independently of pixel resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_millihz: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<Scale>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}
impl Adjustment {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn hdr(mut self, hdr: HdrMode) -> Self {
        self.hdr = Some(hdr);
        self
    }
    pub fn wide_color_gamut(mut self, enabled: bool) -> Self {
        self.wide_color_gamut = Some(enabled);
        self
    }
    pub fn resolution(mut self, width: u32, height: u32) -> Self {
        self.resolution = Some(Resolution { width, height });
        self
    }
    pub fn refresh_millihz(mut self, refresh: u32) -> Self {
        self.refresh_millihz = Some(refresh);
        self
    }
    pub fn scale_percent(mut self, scale: u32) -> Self {
        self.scale = Some(Scale {
            numerator: u64::from(scale),
            denominator: 100,
        });
        self
    }
    pub fn position(mut self, x: i32, y: i32) -> Self {
        self.position = Some(Position { x, y });
        self
    }
    pub fn validate(&self) -> ModuleResult<()> {
        if self == &Self::default()
            || self.resolution.is_some_and(|r| {
                r.width == 0 || r.height == 0 || r.width > 32768 || r.height > 32768
            })
            || self
                .refresh_millihz
                .is_some_and(|r| !(1000..=1_000_000).contains(&r))
            || self.scale.is_some_and(|s| {
                s.denominator == 0
                    || u128::from(s.numerator) * 2 < u128::from(s.denominator)
                    || u128::from(s.numerator) > u128::from(s.denominator) * 5
            })
        {
            return Err("invalid_display_adjustment".into());
        }
        Ok(())
    }
    pub fn merge(&mut self, other: &Self) -> ModuleResult<()> {
        macro_rules! merge {
            ($field:ident) => {
                if let Some(value) = other.$field {
                    if self.$field.is_some_and(|old| old != value) {
                        return Err("display_adjustment_conflict".into());
                    }
                    self.$field = Some(value);
                }
            };
        }
        merge!(hdr);
        merge!(wide_color_gamut);
        merge!(resolution);
        merge!(refresh_millihz);
        merge!(scale);
        merge!(position);
        Ok(())
    }
    pub fn restore_missing(&mut self, previous: &Self, baseline: &Self) {
        macro_rules! restore {
            ($field:ident) => {
                if previous.$field.is_some() && self.$field.is_none() {
                    self.$field = baseline.$field;
                }
            };
        }
        restore!(hdr);
        restore!(wide_color_gamut);
        restore!(resolution);
        restore!(refresh_millihz);
        restore!(scale);
        restore!(position);
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub id: String,
    pub target: Target,
}
impl Scope {
    pub fn validate(&self) -> ModuleResult<()> {
        self.target.validate()?;
        if valid_id(&self.id) {
            Ok(())
        } else {
            Err("invalid_display_scope".into())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub id: String,
    pub scope: Scope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Enumerate,
    Read {
        target: Target,
    },
    /// Only configured source registrars may create scopes and delegate access.
    OpenScope {
        operation_id: String,
        source: String,
        consumers: Vec<String>,
    },
    Acquire {
        operation_id: String,
        scope: Scope,
        adjustment: Adjustment,
    },
    Release {
        lease: Lease,
    },
    /// The source owner revokes the scope after consumers retire, before source removal.
    CloseScope {
        operation_id: String,
    },
    Status {
        scope: Scope,
    },
}
impl Operation {
    pub fn payload(&self) -> ModuleResult<Value> {
        let mut args = serde_json::to_value(self).map_err(|e| e.to_string())?;
        let method = args.as_object_mut().unwrap().remove("action").unwrap();
        Ok(json!({"method":method,"interface_major":1,"interface_minor":0,"arguments":args}))
    }
    pub fn command(&self, options: RequestOptions<'_>) -> ModuleResult<Command> {
        let envelope = self.payload()?;
        Ok(Command::CallApi {
            service: options.service.into(),
            method: envelope["method"].as_str().unwrap().into(),
            interface_major: 1,
            interface_minor: 0,
            payload: envelope["arguments"].clone(),
            timeout_ms: options.timeout_ms,
            operation_id: options.operation_id.into(),
            parent: options.parent,
        })
    }
}
pub fn methods(service: &str) -> Vec<ApiMethod> {
    [
        ("enumerate", vec![]),
        ("read", vec![("target", Kind::Object)]),
        (
            "open_scope",
            vec![
                ("operation_id", Kind::String),
                ("source", Kind::String),
                ("consumers", Kind::Array),
            ],
        ),
        (
            "acquire",
            vec![
                ("operation_id", Kind::String),
                ("scope", Kind::Object),
                ("adjustment", Kind::Object),
            ],
        ),
        ("release", vec![("lease", Kind::Object)]),
        ("close_scope", vec![("operation_id", Kind::String)]),
        ("status", vec![("scope", Kind::Object)]),
    ]
    .into_iter()
    .map(|(name, fields)| operation_api::method(service, name, &fields, &[]))
    .collect()
}
pub fn decode(value: Value) -> ModuleResult<Operation> {
    if value.get("method").is_none() {
        return Err("display_settings_requires_versioned_api".into());
    }
    let value = operation_api::decode(value, &methods(SERVICE))?;
    serde_json::from_value(value).map_err(|_| "invalid_display_settings_request".into())
}
/// Accept a provider's lease only after verified native enforcement of the
/// caller's exact scope. Admission or a pending response is not success.
pub fn lease_from_reply(payload: Value, scope: &Scope) -> ModuleResult<Lease> {
    if payload["ok"] != true || payload["enforced"] != true {
        return Err("display_adjustment_unconfirmed".into());
    }
    let lease: Lease =
        serde_json::from_value(payload["lease"].clone()).map_err(|_| "invalid_display_lease")?;
    scope.validate()?;
    if lease.scope != *scope || !valid_id(&lease.id) {
        return Err("display_lease_scope_mismatch".into());
    }
    Ok(lease)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-:/".contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incompatible_overrides_are_explicit() {
        let mut a = Adjustment::new().hdr(HdrMode::Disabled);
        assert!(a.merge(&Adjustment::new().hdr(HdrMode::Disabled)).is_ok());
        assert!(a.merge(&Adjustment::new().hdr(HdrMode::Enabled)).is_err());
    }
    #[test]
    fn release_restores_only_properties_previously_owned() {
        let before = Adjustment::new().hdr(HdrMode::Disabled);
        let baseline = Adjustment::new().hdr(HdrMode::Enabled);
        let mut after = Adjustment {
            resolution: Some(Resolution {
                width: 1280,
                height: 720,
            }),
            ..Default::default()
        };
        after.restore_missing(&before, &baseline);
        assert_eq!(after.hdr, Some(HdrMode::Enabled));
        assert_eq!(after.resolution.unwrap().width, 1280);
        assert!(after.position.is_none());
    }
}
