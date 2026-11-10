//! Bounded service timing; request payloads and operation identifiers never enter logs.
use super::*;
use amaterasu_timing_metrics::LatencyMetric;
use serde_json::json;

#[derive(Default)]
pub(super) struct RequestTiming {
    pub(super) started: Option<Instant>,
    services: BTreeMap<(String, String), ServiceTiming>,
}
#[derive(Default)]
pub(super) struct ServiceTiming {
    queue: LatencyMetric,
    handler: LatencyMetric,
    total: LatencyMetric,
    result_wait: LatencyMetric,
    cancelled: u64,
    timed_out: u64,
}
fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}
impl RequestTiming {
    pub(super) fn service(&mut self, record: &RequestRecord) -> &mut ServiceTiming {
        let key = (record.caller.clone(), record.options.service.clone());
        let key = if self.services.contains_key(&key) || self.services.len() < 127 {
            key
        } else {
            ("other".into(), "other".into())
        };
        self.services.entry(key).or_default()
    }
    pub(super) fn accepted(&mut self, record: &RequestRecord, now: Instant) {
        self.started.get_or_insert(now);
        self.service(record)
            .queue
            .record(micros(now.saturating_duration_since(record.enqueued_at)));
    }
    pub(super) fn finished(&mut self, record: &RequestRecord, now: Instant) {
        self.started.get_or_insert(now);
        let timing = self.service(record);
        timing
            .total
            .record(micros(now.saturating_duration_since(record.enqueued_at)));
        if let Some(accepted) = record.accepted_at {
            timing
                .handler
                .record(micros(now.saturating_duration_since(accepted)));
        }
        if let Some(Response::Error { code, .. }) = &record.result {
            timing.cancelled = timing
                .cancelled
                .saturating_add(u64::from(code == "cancelled"));
            timing.timed_out = timing
                .timed_out
                .saturating_add(u64::from(code == "deadline_exceeded"));
        }
    }
    pub(super) fn observed(&mut self, record: &RequestRecord, now: Instant) {
        if let Some(completed) = record.completed_at {
            self.service(record)
                .result_wait
                .record(micros(now.saturating_duration_since(completed)));
        }
    }
}
impl RequestRouter {
    pub(in crate::broker) fn take_timing(&mut self, now: Instant) -> Option<Vec<String>> {
        let started = *self.timing.started.get_or_insert(now);
        if now.saturating_duration_since(started) < Duration::from_secs(10) {
            return None;
        }
        let mut services = std::mem::take(&mut self.timing.services);
        let known: BTreeSet<_> = services
            .keys()
            .filter(|(caller, service)| caller != "other" || service != "other")
            .cloned()
            .collect();
        let rows: Vec<_> = services.iter_mut().map(|((caller, service), timing)| {
            let pending: Vec<_> = self.requests.values().filter(|r|
                r.result.is_none() && if caller == "other" && service == "other" { !known.contains(&(r.caller.clone(),r.options.service.clone())) } else { r.caller == *caller && r.options.service == *service }).collect();
            json!({"caller":caller,"service":service,
                "timing_us":{"queue_us":timing.queue,"handler_us":timing.handler,
                    "total_us":timing.total,"result_wait_us":timing.result_wait},
                "cancelled":timing.cancelled,"timed_out":timing.timed_out,
                "pending":pending.len(),
                "oldest_queued_us":pending.iter().filter(|r| r.accepted_at.is_none()).map(|r| micros(now.saturating_duration_since(r.enqueued_at))).max(),
                "oldest_inflight_us":pending.iter().filter_map(|r|r.accepted_at).map(|at|micros(now.saturating_duration_since(at))).max()})
        }).collect();
        // Retain active-service identities across resets so no completion is needed
        // for a stuck service's oldest age to remain visible.
        self.timing.services = services
            .into_keys()
            .map(|key| (key, ServiceTiming::default()))
            .collect();
        self.timing.started = Some(now);
        Some(rows.into_iter().map(|row| json!({"event":"broker_request_window","window_us":micros(now.saturating_duration_since(started)),
            "services":[row],"retained_requests":self.requests.len(),"unretired_deliveries":self.unretired.len(),
            "scope":"broker_admission_to_provider_acceptance_to_terminal_result_to_first_caller_observation"}).to_string()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(now: Instant) -> (RequestRouter, u64) {
        let mut router = RequestRouter::default();
        let provider = Grants {
            provide: ["example.service/1".to_owned()].into(),
            ..Default::default()
        };
        assert!(matches!(
            router.register_service("example.provider", &provider, "example.service/1".into()),
            Response::Ok
        ));
        let caller = Grants {
            call: ["example.service/1".to_owned()].into(),
            ..Default::default()
        };
        let result = router.submit(
            "example.caller",
            &caller,
            RequestOptions {
                service: "example.service/1".into(),
                payload: json!({"private":"must_not_be_logged"}),
                timeout_ms: 30_000,
                operation_id: "private-operation-id".into(),
                parent: None,
                response_schema: None,
            },
            now,
        );
        let Response::Accepted { id } = result else {
            panic!("request not accepted");
        };
        (router, id)
    }
    #[test]
    fn stages_use_first_acceptance_and_first_terminal_observation_without_payloads() {
        let now = Instant::now();
        let (mut router, id) = setup(now);
        assert!(matches!(
            router.poll("example.provider", now + Duration::from_millis(10), None),
            Response::Request { .. }
        ));
        assert!(matches!(
            router.complete(
                "example.provider",
                id,
                json!({"ok":true}),
                now + Duration::from_millis(30)
            ),
            Response::Ok
        ));
        router.result_at("example.caller", id, now + Duration::from_millis(50));
        router.result_at("example.caller", id, now + Duration::from_millis(80));
        let reports = router.take_timing(now + Duration::from_secs(11)).unwrap();
        let report: Value = serde_json::from_str(&reports[0]).unwrap();
        let stages = &report["services"][0]["timing_us"];
        for (name, value) in [
            ("queue_us", 10_000),
            ("handler_us", 20_000),
            ("total_us", 30_000),
            ("result_wait_us", 20_000),
        ] {
            assert_eq!(stages[name]["calls"], 1);
            assert_eq!(stages[name]["max_us"], value);
        }
        assert!(!reports[0].contains("must_not_be_logged"));
        assert!(!reports[0].contains("private-operation-id"));
        assert!(reports[0].len() < 7500);
    }
    #[test]
    fn unaccepted_request_keeps_its_original_age_across_reporting_windows() {
        let now = Instant::now();
        let (mut router, _) = setup(now);
        for seconds in [11, 22] {
            let reports = router
                .take_timing(now + Duration::from_secs(seconds))
                .unwrap();
            let report: Value = serde_json::from_str(&reports[0]).unwrap();
            assert_eq!(report["services"][0]["pending"], 1);
            assert_eq!(
                report["services"][0]["oldest_queued_us"],
                seconds * 1_000_000
            );
            assert_eq!(report["services"][0]["timing_us"]["queue_us"]["calls"], 0);
        }
    }
}
