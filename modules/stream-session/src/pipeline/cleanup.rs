use super::input::*;
use super::responses::*;
use super::*;

impl Pipeline {
    pub(super) fn consume_cleanup(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        payload: Value,
        send: &mut Send<'_>,
    ) -> bool {
        match job.step {
            Step::FindInput => {
                if input_missing(&payload) || input_released(record, &payload) {
                    clear_input(record);
                    job.input_tried = true;
                } else if adopt_input_for_cleanup(record, &payload).is_err() {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::ReleaseInput => {
                if accept_input_release(record, &payload).is_ok() {
                    job.input_tried = true;
                } else {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::Open(resource) => {
                if let Some(id) = allocation_id(record, resource, &payload) {
                    record.resources.insert(resource, id);
                }
            }
            Step::Find(resource) => {
                if let Some(id) = allocation_id(record, resource, &payload) {
                    record.resources.insert(resource, id);
                } else if missing(&payload) {
                    record.attempted.remove(&resource);
                } else {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::Close(resource) => {
                if payload["ok"] != true && !missing(&payload) {
                    return self.cleanup_rejected(record, job, send);
                }
                if resource == Resource::Video && !missing(&payload) {
                    job.confirming = Some(resource);
                } else {
                    record.resources.remove(&resource);
                    record.attempted.remove(&resource);
                    job.tried.insert(resource);
                }
            }
            Step::AwaitClosed(resource) => {
                if missing(&payload) {
                    record.resources.remove(&resource);
                    record.attempted.remove(&resource);
                    job.tried.insert(resource);
                    job.confirming = None;
                } else if allocation_id(record, resource, &payload).is_none()
                    || payload["state"] == "cleanup_failed"
                {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::StopSession => {
                if session_response(record, &payload).is_none_or(|value| {
                    !matches!(value["state"].as_str(), Some("stopping" | "stopped"))
                }) {
                    return self.cleanup_rejected(record, job, send);
                }
                job.step = Step::FinishSession;
                // Preserve this explicit transition rather than restarting cleanup selection.
                job.pending = Some(self.cleanup_rpc(record, job));
            }
            Step::FinishSession => {
                if session_response(record, &payload).is_none_or(|value| {
                    value["state"] != "stopped"
                        || !value["leases"]
                            .as_object()
                            .is_some_and(|leases| leases.is_empty())
                }) {
                    return self.cleanup_rejected(record, job, send);
                }
                record.history.teardown_confirmed();
                record.resources.remove(&Resource::Session);
                record.attempted.remove(&Resource::Session);
            }
            _ => {}
        }
        true
    }

    fn cleanup_rpc(&mut self, record: &Record, job: &Job) -> rpc::Pending {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("bounded operation lifetime");
        let (service, payload) = job.command(record);
        rpc::Pending::new(
            &service,
            payload,
            format!("ss-{:x}-{}", record.id, self.sequence),
            None,
        )
    }

    pub(super) fn cleanup_rejected(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        send: &mut Send<'_>,
    ) -> bool {
        record.cleanup_error = Some("component_cleanup_failed");
        match job.step {
            Step::ReleaseInput => {
                record.input_lookup_required = true;
                job.input_tried = false;
                true
            }
            Step::FindInput => {
                job.input_tried = true;
                true
            }
            Step::Find(resource) | Step::Close(resource) | Step::AwaitClosed(resource)
                if resource != Resource::Session =>
            {
                job.confirming = None;
                job.tried.insert(resource);
                true
            }
            _ => {
                record.state = State::CleanupFailed;
                self.reply(record, job, send);
                false
            }
        }
    }

    pub(super) fn finish_cleanup(
        &self,
        record: &mut Record,
        job: &Job,
        send: &mut Send<'_>,
    ) -> bool {
        record.state = if record.attempted.is_empty()
            && record.uncertain.is_none()
            && !record.input_attempted
            && record.input_lease.is_none()
        {
            record.cleanup_error = None;
            record.outcome
        } else {
            State::CleanupFailed
        };
        self.reply(record, job, send);
        false
    }
}
