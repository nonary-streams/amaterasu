#![cfg(feature = "process")]
//! Two in-memory process providers exercise the same SDK consumer. These
//! fixtures model effects; they do not qualify OS process or sink ownership.
use amaterasu_module_sdk::{
    abi::Status,
    process::{Process, ProcessFactory, Provider, api},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn capabilities() -> api::Capabilities {
    api::Capabilities {
        managed_groups: true,
        detached: true,
        shared_output: true,
        elevated: false,
        elevated_output: false,
        max_payload_bytes: api::MAX_PAYLOAD_BYTES as u32,
        max_queued_operations: 8,
        max_operation_receipts: 32,
        max_processes: 2,
        max_reservations: 1,
        max_sinks: 2,
        max_epochs: 8,
    }
}

#[derive(Default)]
struct Maps {
    epoch: u64,
    receipts: BTreeMap<String, (api::Request, api::Response)>,
    reservations: BTreeMap<String, api::ProcessKind>,
    sinks: BTreeMap<String, Vec<String>>,
    processes: BTreeMap<String, api::ProcessState>,
    serial: u64,
    closed: usize,
    polled: std::collections::BTreeSet<String>,
}
#[derive(Default)]
struct MapProvider(Mutex<Maps>);
impl Provider for MapProvider {
    fn capabilities(&self) -> Result<api::Capabilities, Status> {
        Ok(capabilities())
    }
    fn open(&self, caller: &str) -> Result<String, Status> {
        assert_eq!(caller, "consumer");
        let mut s = self.0.lock().unwrap();
        s.epoch += 1;
        Ok(s.epoch.to_string())
    }
    fn close(&self, _: &str, _: &str) {
        self.0.lock().unwrap().closed += 1;
    }
    fn submit(&self, _: &str, _: &str, id: &str, req: api::Request) -> Result<(), Status> {
        let mut s = self.0.lock().unwrap();
        if let Some((old, _)) = s.receipts.get(id) {
            return if *old == req {
                Ok(())
            } else {
                Err(api::CONFLICT)
            };
        }
        let response = match &req {
            api::Request::Reserve { kind } => {
                if !s.reservations.is_empty() {
                    return Err(api::RESOURCE_EXHAUSTED);
                }
                s.serial += 1;
                let token = format!("r{}", s.serial);
                s.reservations.insert(token.clone(), *kind);
                api::Response::Reservation {
                    reservation: api::ReservationToken(token),
                }
            }
            api::Request::CancelReservation { reservation } => {
                if s.reservations.remove(&reservation.0).is_none() {
                    return Err(api::INVALID_STATE);
                }
                api::Response::Ok
            }
            api::Request::OpenSink { .. } => {
                s.serial += 1;
                let token = format!("s{}", s.serial);
                s.sinks.insert(token.clone(), Vec::new());
                api::Response::Sink {
                    sink: api::OutputSinkToken(token),
                }
            }
            api::Request::Spawn {
                reservation,
                kind,
                command,
            } => {
                if s.reservations.remove(&reservation.0) != Some(*kind) {
                    return Err(api::INVALID_STATE);
                }
                if let api::OutputTarget::Sink { sink } = &command.stdout {
                    s.sinks
                        .get_mut(&sink.0)
                        .unwrap()
                        .push(command.executable.clone());
                }
                s.serial += 1;
                let token = format!("p{}", s.serial);
                s.processes
                    .insert(token.clone(), api::ProcessState::Running);
                api::Response::Process {
                    process: api::ProcessToken(token),
                }
            }
            api::Request::ReleaseObservation { .. } | api::Request::CloseSink { .. } => {
                api::Response::Ok
            }
            _ => return Err(api::INVALID_STATE),
        };
        s.receipts.insert(id.into(), (req, response));
        Ok(())
    }
    fn poll(&self, _: &str, id: &str) -> Result<api::Response, Status> {
        let mut state = self.0.lock().unwrap();
        if state.receipts.contains_key(id) && state.polled.insert(id.to_owned()) {
            return Ok(api::Response::Pending);
        }
        Ok(state
            .receipts
            .get(id)
            .map_or(api::Response::NotFound, |(_, r)| r.clone()))
    }
    fn inspect(&self, _: &str, p: &api::ProcessToken) -> Result<api::Response, Status> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .processes
            .get(&p.0)
            .map_or(api::Response::NotFound, |s| api::Response::ProcessStatus {
                state: s.clone(),
            }))
    }
    fn release_operation(&self, _: &str, _: &str) -> Result<(), Status> {
        Ok(())
    }
}

#[derive(Default)]
struct Lists {
    epoch: u64,
    operations: Vec<(String, api::Request, api::Response)>,
    reservation: Option<(String, api::ProcessKind)>,
    sinks: Vec<(String, Vec<String>)>,
    processes: Vec<(String, api::ProcessState)>,
    next: u64,
    closed: usize,
    polled: Vec<String>,
}
#[derive(Default)]
struct ListProvider(Mutex<Lists>);
impl Provider for ListProvider {
    fn capabilities(&self) -> Result<api::Capabilities, Status> {
        Ok(capabilities())
    }
    fn open(&self, caller: &str) -> Result<String, Status> {
        assert_eq!(caller, "consumer");
        let mut s = self.0.lock().unwrap();
        s.epoch += 1;
        Ok(s.epoch.to_string())
    }
    fn close(&self, _: &str, _: &str) {
        self.0.lock().unwrap().closed += 1;
    }
    fn submit(&self, _: &str, _: &str, id: &str, req: api::Request) -> Result<(), Status> {
        let mut s = self.0.lock().unwrap();
        if let Some((_, old, _)) = s.operations.iter().find(|(key, _, _)| key == id) {
            return if old == &req {
                Ok(())
            } else {
                Err(api::CONFLICT)
            };
        }
        let reply = match &req {
            api::Request::Reserve { kind } => {
                if s.reservation.is_some() {
                    return Err(api::RESOURCE_EXHAUSTED);
                }
                s.next += 1;
                let key = format!("reservation-{}", s.next);
                s.reservation = Some((key.clone(), *kind));
                api::Response::Reservation {
                    reservation: api::ReservationToken(key),
                }
            }
            api::Request::CancelReservation { reservation } => {
                if s.reservation.as_ref().map(|(key, _)| key) != Some(&reservation.0) {
                    return Err(api::INVALID_STATE);
                }
                s.reservation = None;
                api::Response::Ok
            }
            api::Request::OpenSink { .. } => {
                s.next += 1;
                let key = format!("sink-{}", s.next);
                s.sinks.push((key.clone(), Vec::new()));
                api::Response::Sink {
                    sink: api::OutputSinkToken(key),
                }
            }
            api::Request::Spawn {
                reservation,
                kind,
                command,
            } => {
                if s.reservation.as_ref() != Some(&(reservation.0.clone(), *kind)) {
                    return Err(api::INVALID_STATE);
                }
                s.reservation = None;
                if let api::OutputTarget::Sink { sink } = &command.stdout {
                    s.sinks
                        .iter_mut()
                        .find(|(key, _)| key == &sink.0)
                        .unwrap()
                        .1
                        .push(command.executable.clone())
                }
                s.next += 1;
                let key = format!("process-{}", s.next);
                s.processes.push((key.clone(), api::ProcessState::Running));
                api::Response::Process {
                    process: api::ProcessToken(key),
                }
            }
            api::Request::ReleaseObservation { .. } | api::Request::CloseSink { .. } => {
                api::Response::Ok
            }
            _ => return Err(api::INVALID_STATE),
        };
        s.operations.push((id.into(), req, reply));
        Ok(())
    }
    fn poll(&self, _: &str, id: &str) -> Result<api::Response, Status> {
        let mut state = self.0.lock().unwrap();
        if state.operations.iter().any(|(key, _, _)| key == id)
            && !state.polled.iter().any(|key| key == id)
        {
            state.polled.push(id.to_owned());
            return Ok(api::Response::Pending);
        }
        Ok(state
            .operations
            .iter()
            .find(|(key, _, _)| key == id)
            .map_or(api::Response::NotFound, |(_, _, r)| r.clone()))
    }
    fn inspect(&self, _: &str, p: &api::ProcessToken) -> Result<api::Response, Status> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .processes
            .iter()
            .find(|(key, _)| key == &p.0)
            .map_or(api::Response::NotFound, |(_, s)| {
                api::Response::ProcessStatus { state: s.clone() }
            }))
    }
    fn release_operation(&self, _: &str, _: &str) -> Result<(), Status> {
        Ok(())
    }
}

fn operation(client: &Process, request: api::Request) -> api::Response {
    let prepared = client.prepare(&request).unwrap();
    client.submit(&prepared).unwrap();
    assert_eq!(
        client.poll(prepared.operation_id()).unwrap(),
        api::Response::Pending
    );
    client.poll(prepared.operation_id()).unwrap()
}
fn consumer(client: &Process) -> api::ProcessToken {
    let api::Response::Reservation { reservation } = operation(
        client,
        api::Request::Reserve {
            kind: api::ProcessKind::Managed,
        },
    ) else {
        panic!("reserve")
    };
    let blocked = client
        .prepare(&api::Request::Reserve {
            kind: api::ProcessKind::Detached,
        })
        .unwrap();
    assert_eq!(client.submit(&blocked), Err(api::RESOURCE_EXHAUSTED));
    client.discard_prepared(&blocked).unwrap();
    assert_eq!(
        operation(client, api::Request::CancelReservation { reservation }),
        api::Response::Ok
    );
    let api::Response::Sink { sink } = operation(
        client,
        api::Request::OpenSink {
            path: "/virtual/output".into(),
        },
    ) else {
        panic!("sink")
    };
    let mut last = None;
    for kind in [api::ProcessKind::Managed, api::ProcessKind::Detached] {
        let api::Response::Reservation { reservation } =
            operation(client, api::Request::Reserve { kind })
        else {
            panic!("reserve")
        };
        let command = api::CommandSpec {
            executable: format!("/{kind:?}"),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            working_directory: None,
            elevated: false,
            stdout: api::OutputTarget::Sink { sink: sink.clone() },
            stderr: api::OutputTarget::Null,
        };
        let api::Response::Process { process } = operation(
            client,
            api::Request::Spawn {
                reservation,
                kind,
                command,
            },
        ) else {
            panic!("spawn")
        };
        last = Some(process);
    }
    let process = last.unwrap();
    assert!(matches!(
        client.inspect(&process),
        Ok(api::Response::ProcessStatus {
            state: api::ProcessState::Running
        })
    ));
    process
}
fn bind<P: Provider + 'static>(factory: &ProcessFactory<P>) -> (Process, *const api::ProcessApi) {
    // SAFETY: factory remains alive until every client is dropped in this test.
    let table = unsafe { factory.bind_standalone("consumer") }.unwrap();
    // SAFETY: the factory supplies a uniquely owned complete process table.
    (unsafe { Process::from_native_table(table) }.unwrap(), table)
}

fn check_immutable_retry(table: *const api::ProcessApi) {
    let operation = amaterasu_module_sdk::abi::BorrowedBytes::new(b"1:1");
    for (kind, expected) in [
        (api::ProcessKind::Managed, 0),
        (api::ProcessKind::Detached, api::CONFLICT),
    ] {
        let payload = serde_json::to_vec(&api::Request::Reserve { kind }).unwrap();
        // SAFETY: the SDK consumer still owns this complete live binding and the
        // borrowed bytes remain valid until this synchronous callback returns.
        let status = unsafe {
            ((*table).submit)(
                (*table).base.context,
                operation,
                amaterasu_module_sdk::abi::BorrowedBytes::new(&payload),
            )
        };
        assert_eq!(status, expected);
    }
}
#[test]
fn unchanged_sdk_consumer_works_with_two_fixture_providers() {
    let first = Arc::new(MapProvider::default());
    let factory = ProcessFactory::new(first.clone());
    let (client, table) = bind(&factory);
    let first_process = consumer(&client);
    check_immutable_retry(table);
    drop(client);
    let (replacement, _) = bind(&factory);
    assert!(matches!(
        replacement.inspect(&first_process),
        Ok(api::Response::ProcessStatus {
            state: api::ProcessState::Running
        })
    ));
    assert_eq!(
        operation(
            &replacement,
            api::Request::ReleaseObservation {
                process: first_process.clone()
            }
        ),
        api::Response::Ok
    );
    drop(replacement);
    assert_eq!(first.0.lock().unwrap().closed, 2);
    let second = Arc::new(ListProvider::default());
    let factory = ProcessFactory::new(second.clone());
    let (client, table) = bind(&factory);
    let second_process = consumer(&client);
    check_immutable_retry(table);
    drop(client);
    let (replacement, _) = bind(&factory);
    assert!(matches!(
        replacement.inspect(&second_process),
        Ok(api::Response::ProcessStatus {
            state: api::ProcessState::Running
        })
    ));
    assert_eq!(
        operation(
            &replacement,
            api::Request::ReleaseObservation {
                process: second_process.clone()
            }
        ),
        api::Response::Ok
    );
    drop(replacement);
    assert_eq!(second.0.lock().unwrap().closed, 2);
    assert!(
        first
            .0
            .lock()
            .unwrap()
            .processes
            .contains_key(&first_process.0)
    );
    assert!(
        second
            .0
            .lock()
            .unwrap()
            .processes
            .iter()
            .any(|(key, _)| key == &second_process.0)
    );
    assert_eq!(
        first.0.lock().unwrap().sinks.values().next().unwrap(),
        &["/Managed", "/Detached"]
    );
    assert_eq!(
        second.0.lock().unwrap().sinks[0].1,
        &["/Managed", "/Detached"]
    );
}
