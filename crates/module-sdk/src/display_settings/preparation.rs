//! Generic post-allocation preparation hook. Plugins use the scoped SDK handle
//! before capture opens; the scheduler does not interpret adjustment settings.
use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub display_scope: Scope,
    pub settings_service: String,
    pub lifecycle_target: crate::extension_api::LifecycleTarget,
    pub output_format: crate::VideoStreamFormat,
    pub settings_revision: String,
    pub settings: Value,
}
pub fn method(service: &str) -> ApiMethod {
    operation_api::method(service, "prepare_source", &[("context", Kind::Object)], &[])
}
pub fn payload(context: &Context) -> Value {
    json!({"method":"prepare_source","interface_major":1,"interface_minor":0,"arguments":{"context":context}})
}
