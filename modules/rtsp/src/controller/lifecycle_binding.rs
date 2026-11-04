//! A joined capture binding retains the original source mode. Client encode
//! geometry remains independent and must be supported by the selected video path.
use amaterasu_extension_api::{
    CaptureBinding, LifecycleTarget, StreamJoinContext, validate_stream_join_binding,
};
use serde_json::{Value, json};

pub(super) fn valid_join_binding(
    binding: &CaptureBinding,
    target: &LifecycleTarget,
    payload: &Value,
) -> bool {
    let Ok(context) = serde_json::from_value::<StreamJoinContext>(payload["join_context"].clone())
    else {
        return false;
    };
    payload["shared_source"] == json!(context.source)
        && validate_stream_join_binding(&context, target, binding).is_ok()
}

pub(super) fn valid_external_join(target: &LifecycleTarget, payload: &Value) -> bool {
    let Ok(context) = serde_json::from_value::<StreamJoinContext>(payload["join_context"].clone())
    else {
        return false;
    };
    context.capture_binding_mode.is_external()
        && context.binding.is_none()
        && context.binding_owner.is_none()
        && payload["shared_source"] == json!(context.source)
        && amaterasu_extension_api::validate_stream_join_context(&context, target).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (CaptureBinding, LifecycleTarget, Value) {
        let source = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"source","generation":3});
        let target = serde_json::from_value(json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"subscriber","generation":1})).unwrap();
        let binding: CaptureBinding = serde_json::from_value(json!({
            "capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","selector":{"output":"kwin:Virtual-12"},
            "resource":{"provider":"test.provider","resource_id":"output","incarnation":"one","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":["desktop"]},
            "mode":{"width":1920,"height":1080,"refresh_millihz":120000,"hdr":false},
            "plan_revision":1,"geometry_revision":1,"generation":1,"fallback":"forbidden"})).unwrap();
        let mut original = binding.clone();
        original.generation = 3;
        let payload = json!({"shared_source":source,"join_context":{"source":source,
            "source_transition":"source-prepare","binding_owner":"test.provider","binding":original}});
        (binding, target, payload)
    }

    #[test]
    fn joined_candidate_matches_the_frozen_original_source_except_consumer_generation() {
        let (binding, target, payload) = fixture();
        assert!(valid_join_binding(&binding, &target, &payload));
        for field in ["width", "height", "refresh_millihz"] {
            let mut changed = payload.clone();
            changed["join_context"]["binding"]["mode"][field] = json!(1);
            assert!(!valid_join_binding(&binding, &target, &changed));
        }
        let mut changed = payload.clone();
        changed["join_context"]["binding"]["mode"]["hdr"] = json!(true);
        assert!(!valid_join_binding(&binding, &target, &changed));
        let mut changed = binding.clone();
        changed.geometry_revision += 1;
        assert!(!valid_join_binding(&changed, &target, &payload));
        let mut changed = payload.clone();
        changed["shared_source"]["generation"] = json!(4);
        assert!(!valid_join_binding(&binding, &target, &changed));
        let mut changed = payload.clone();
        changed["join_context"]["source"]["generation"] = json!(4);
        assert!(!valid_join_binding(&binding, &target, &changed));
        let mut missing = payload;
        missing.as_object_mut().unwrap().remove("join_context");
        assert!(!valid_join_binding(&binding, &target, &missing));
    }
}
