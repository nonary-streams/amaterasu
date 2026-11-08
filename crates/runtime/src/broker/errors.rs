use amaterasu_contracts::Response;
use serde_json::Value;

const MAX_PAYLOAD_BYTES: usize = 16_384;

pub(super) fn permission_denied() -> Response {
    Response::error(
        "permission_denied",
        "operation not granted to this endpoint",
    )
}

pub(super) fn host_lifecycle_capacity_exhausted() -> Response {
    Response::error("capacity_exhausted", "host lifecycle action queue is full")
}

pub(super) fn not_found() -> Response {
    Response::error("not_found", "unknown handle or interface")
}

pub(super) fn capacity_exhausted() -> Response {
    Response::error("resource_exhausted", "bounded runtime capacity reached")
}

pub(super) fn payload_fits(payload: &Value) -> bool {
    serde_json::to_vec(payload).is_ok_and(|bytes| bytes.len() <= MAX_PAYLOAD_BYTES)
}

pub(super) fn result_fits(payload: &Value) -> bool {
    serde_json::to_vec(payload)
        .is_ok_and(|bytes| bytes.len() <= amaterasu_contracts::MAX_BROKER_RESULT_BYTES)
}
