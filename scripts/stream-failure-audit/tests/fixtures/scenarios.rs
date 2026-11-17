// Syntax fixture only: deliberately independent of production implementations.
// Gold labels are manually selected observable failure/propagation scenarios.
fn explicit_security_error() -> Result<(), E> { Err("permission_denied".into()) }
fn propagated_startup() -> Result<(), E> { capture_start()?; Ok(()) }
fn converted_error() -> Result<(), E> { provider().map_err(|_| "provider_failed")?; Ok(()) }
fn absent_required_provider() -> Result<(), E> { provider().ok_or("encoder_unavailable")?; Ok(()) }
fn wire_auth(controller: &Controller) { controller.reply_status(1, 2, 401); }
fn wire_expired(controller: &Controller) { controller.reply_status(1, 2, 454); }
fn wire_sequence(controller: &Controller) { controller.reply_status(1, 2, 455); }
fn wire_busy(controller: &Controller) { controller.reply_status(1, 2, 503); }
fn wire_dynamic(controller: &Controller, error: E) { controller.reply_status(1, 2, error.status()); }
fn wire_with_failure_report(controller: &Controller) { controller.reply_status(1, 2, 455u16, StreamFailure::advisory("request_wrong_state")); }
fn timeout(job: &mut Job, record: &mut Record) { job.fail(record, "deadline_expired", 12); }
fn failed_state(record: &mut Record) { record.state = State::Failed; }
fn stopping_state(record: &mut Record) { record.state = State::Stopping; }
fn negative_json() { json!({"ok": false, "error": "capture_unavailable"}); }
fn nested_negative_json() { json!({"reply": {"ok": false}}); }
fn broker_dispatch(runtime: &Runtime) { runtime.send(Command::Request { service: "nonary.encoder/1".into() }); }
fn ipc_literal() { Pending::new("nonary.capture/1", json!({"action":"open"}), "op".into(), None); }
fn ipc_dynamic(service: &str) { Pending::new(service, payload(), operation(), None); }
fn uncertain_cleanup(record: &mut Record) { record.close(Some("ownership_unconfirmed")); }
fn normal_teardown(record: &mut Record) { record.close(None); }
fn revoke(controller: &mut Controller) { controller.revoke_ticket(17); }
fn transport_disconnected(plane: &mut Plane) { plane.stop(); }
fn process_abort() { std::process::abort(); }
fn panic_on_startup() { panic!("required invariant"); }
fn unwrap_failure() { provider().unwrap(); }
fn expect_failure() { provider().expect("capture required"); }
fn declared_critical() { StreamIssue::critical("auth", "invalid_key"); }
fn declared_demoted() { StreamIssue::demoted("stats", "history_unavailable"); }
fn declared_advisory() { StreamIssue::advisory("input", "dropped_batch"); }
fn sentinel_none() -> Option<T> { return None; }
fn sentinel_false() -> bool { return false; }
fn tail_none() -> Option<T> { None }
fn typed_error_reply() { error("required_media_failed"); }
fn pointer_dispatch(handler: Handler) { (handler.on_error)(); }
fn guarded_failure(controller: &mut Controller, ready: bool, event: Event) {
    if !ready { match event { Event::Disconnected => controller.reject("transport_lost"), _ => {} } }
}
#[cfg(target_os = "windows")]
fn platform_failure() -> Result<(), E> { Err("windows_capture_failed".into()) }
#[cfg(any(test, feature = "optional-provider"))]
fn mixed_cfg_production() -> Result<(), E> { Err("provider_failed".into()) }
fn let_else_provider(provider: Option<T>, controller: &mut Controller) {
    let Some(provider) = provider else { controller.reject("provider_missing"); return; };
    provider.run();
}

// Negative controls: comments, literals, optional fallback, healthy responses,
// same-name methods, and tests must not become definite stream rejections.
fn comment_only() { /* controller.reply_status(1, 2, 503); Err("oops") */ }
fn string_only() { let text = r#"Err("failed"); reply_status(1, 2, 503)"#; }
fn optional_fallback() { provider().unwrap_or_default(); }
fn optional_caught() { if let Err(_) = provider() { warn!("optional history unavailable"); } }
fn healthy_wire(controller: &Controller) { controller.reply_status(1, 2, 200); }
fn healthy_wire_with_report(controller: &Controller) { controller.reply_status(1, 2, 200u16, StreamFailure::advisory("healthy")); }
fn healthy_json() { json!({"ok": true}); }
fn healthy_json_string() { json!({"text": "ok: false", "ok": true}); }
fn format_message() { format!("rejected: {}", 503); }
fn log_only() { eprintln!("stream failed status=503"); }
fn warning_only() { warn!("permission_denied"); }
fn identifier_only() { let rejected = false; let failure_status = 503; }
fn error_text_variable() { let reason = "authentication_failed"; }
fn harmless_name() { rejection_counter(); }
#[test]
fn test_function() { controller.reject("not_production"); }
#[cfg(test)]
mod test_module { fn fake_failure() { controller.reject("not_production"); } }
fn nested_declaration() { fn not_executed() { panic!("not executed"); } }

struct Worker;
impl Worker {
    fn start(&mut self) { self.prepare(); }
    fn prepare(&mut self) { self.fail("native_start_failed"); }
    fn fail(&mut self, reason: &str) { self.state = State::Failed; }
}
struct File;
impl File { fn close(&mut self) {} }
struct Stream;
impl Stream { fn close(&mut self) {} }
fn file_close(file: &mut File) { file.close(); }
fn stream_close(stream: &mut Stream) { stream.close(); }
fn unknown_close(value: T) { value.close(); }
fn caught_origin() -> Result<(), E> { Err("optional_history_unavailable".into()) }
fn caught_at_caller() { let _ = caught_origin(); }
fn opaque_macro() { hidden_stream_failure!(); }
fn private_wire_error() { private_error(409, "resume settings unavailable"); }
fn reply_struct() { Reply { status_code: 503, body: empty() }; }
fn conditional_failed_state(record: &mut Record, cancelled: bool) { record.state = if cancelled { State::Cancelled } else { State::Failed }; }
fn closing_state(record: &mut Record) { record.state = State::Closing; }
fn failed_state_return() -> State { State::Failed }
