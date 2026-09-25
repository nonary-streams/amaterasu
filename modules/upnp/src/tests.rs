use std::cell::RefCell;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::rc::Rc;

use super::{
    Gateway, GatewayError, ListenerEndpoint, Mapping, MappingKey, MappingManager, Protocol,
    ReconcileReport, RemovalOutcome, ShutdownReport, UpnpError,
};

#[derive(Clone, Default)]
struct FakeGateway(Rc<RefCell<FakeGatewayState>>);

#[derive(Default)]
struct FakeGatewayState {
    mappings: BTreeMap<MappingKey, Mapping>,
    fail_add_for: Option<MappingKey>,
    fail_remove_for: Option<MappingKey>,
    add_attempts: usize,
}

impl FakeGateway {
    fn seed(&self, mapping: Mapping) {
        self.0.borrow_mut().mappings.insert(mapping.key(), mapping);
    }

    fn set_add_failure(&self, key: MappingKey) {
        self.0.borrow_mut().fail_add_for = Some(key);
    }

    fn mappings(&self) -> BTreeMap<MappingKey, Mapping> {
        self.0.borrow().mappings.clone()
    }

    fn add_attempts(&self) -> usize {
        self.0.borrow().add_attempts
    }

    fn change_mapping(&self, mapping: Mapping) {
        self.0.borrow_mut().mappings.insert(mapping.key(), mapping);
    }
}

impl Gateway for FakeGateway {
    fn find_mappings(&mut self, key: MappingKey) -> Result<Vec<Mapping>, GatewayError> {
        Ok(self
            .0
            .borrow()
            .mappings
            .get(&key)
            .cloned()
            .into_iter()
            .collect())
    }

    fn create_if_absent(&mut self, mapping: &Mapping) -> Result<(), GatewayError> {
        let mut state = self.0.borrow_mut();
        state.add_attempts += 1;

        if state.fail_add_for == Some(mapping.key()) {
            return Err(GatewayError::new("injected add failure"));
        }
        if state.mappings.contains_key(&mapping.key()) {
            return Err(GatewayError::new("mapping already exists"));
        }

        state.mappings.insert(mapping.key(), mapping.clone());
        Ok(())
    }

    fn remove_if_matches(&mut self, mapping: &Mapping) -> Result<RemovalOutcome, GatewayError> {
        let mut state = self.0.borrow_mut();
        let key = mapping.key();

        if state.fail_remove_for == Some(key) {
            return Err(GatewayError::new("injected remove failure"));
        }

        match state.mappings.get(&key) {
            None => Ok(RemovalOutcome::Missing),
            Some(current) if current == mapping => {
                state.mappings.remove(&key);
                Ok(RemovalOutcome::Removed)
            }
            Some(_) => Ok(RemovalOutcome::Changed),
        }
    }
}

fn endpoint(address: [u8; 4], port: u16, active: bool) -> ListenerEndpoint {
    ListenerEndpoint::new(IpAddr::V4(Ipv4Addr::from(address)), port, active)
}

fn key(port: u16, protocol: Protocol) -> MappingKey {
    MappingKey {
        external_port: port,
        protocol,
    }
}

fn manager(gateway: FakeGateway) -> MappingManager<FakeGateway> {
    MappingManager::with_owner_id(gateway, "deterministic-test")
}

#[test]
fn reconcile_is_idempotent_for_owned_tcp_and_udp_mappings() {
    let fake_gateway = FakeGateway::default();
    let mut manager = manager(fake_gateway.clone());
    let configured = [endpoint([192, 168, 1, 10], 47989, true)];

    assert_eq!(
        manager.reconcile(&configured).unwrap(),
        ReconcileReport {
            created: 2,
            removed: 0,
            already_active: 0,
            changed_mappings_preserved: 0,
        }
    );
    assert_eq!(manager.reconcile(&configured).unwrap().created, 0);
    assert_eq!(manager.owned_mappings().count(), 2);
    assert_eq!(fake_gateway.add_attempts(), 2);
    assert_eq!(fake_gateway.mappings().len(), 2);
}

#[test]
fn reconcile_updates_to_a_distinct_endpoint_and_port() {
    let fake_gateway = FakeGateway::default();
    let mut manager = manager(fake_gateway.clone());

    manager
        .reconcile(&[endpoint([192, 168, 1, 10], 47989, true)])
        .unwrap();
    let report = manager
        .reconcile(&[endpoint([192, 168, 1, 20], 48000, true)])
        .unwrap();

    assert_eq!(report.created, 2);
    assert_eq!(report.removed, 2);
    assert_eq!(manager.owned_mappings().count(), 2);
    let mappings = fake_gateway.mappings();
    assert_eq!(mappings.len(), 2);
    assert!(mappings.contains_key(&key(48000, Protocol::Tcp)));
    assert!(mappings.contains_key(&key(48000, Protocol::Udp)));
    assert!(!mappings.contains_key(&key(47989, Protocol::Tcp)));
}

#[test]
fn address_change_on_the_same_external_port_preserves_the_old_mapping() {
    let fake_gateway = FakeGateway::default();
    let mut manager = manager(fake_gateway.clone());
    manager
        .reconcile(&[endpoint([192, 168, 1, 10], 47989, true)])
        .unwrap();
    let mappings_before = fake_gateway.mappings();

    let error = manager
        .reconcile(&[endpoint([192, 168, 1, 20], 47989, true)])
        .unwrap_err();

    assert!(matches!(error, UpnpError::ExistingMapping { .. }));
    assert_eq!(fake_gateway.mappings(), mappings_before);
    assert_eq!(manager.owned_mappings().count(), 2);
}

#[test]
fn inactive_zero_port_and_non_ipv4_endpoints_are_ignored() {
    let fake_gateway = FakeGateway::default();
    let mut manager = manager(fake_gateway.clone());
    let endpoints = [
        endpoint([192, 168, 1, 10], 47989, false),
        endpoint([192, 168, 1, 10], 0, true),
        ListenerEndpoint::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 47990, true),
    ];

    assert_eq!(
        manager.reconcile(&endpoints).unwrap(),
        ReconcileReport::default()
    );
    assert!(fake_gateway.mappings().is_empty());
    assert_eq!(fake_gateway.add_attempts(), 0);
}

#[test]
fn preexisting_mapping_is_preserved_and_never_adopted() {
    let fake_gateway = FakeGateway::default();
    let preexisting_mapping = Mapping {
        external_port: 47989,
        protocol: Protocol::Tcp,
        internal_address: Ipv4Addr::new(192, 168, 1, 99),
        internal_port: 47989,
        remote_host: String::new(),
        enabled: true,
        description: "another service".into(),
        lease_duration_seconds: 0,
    };
    fake_gateway.seed(preexisting_mapping.clone());
    let mut manager = manager(fake_gateway.clone());

    let error = manager
        .reconcile(&[endpoint([192, 168, 1, 10], 47989, true)])
        .unwrap_err();

    assert!(matches!(error, UpnpError::ExistingMapping { .. }));
    assert_eq!(
        fake_gateway.mappings().get(&key(47989, Protocol::Tcp)),
        Some(&preexisting_mapping)
    );
    assert_eq!(fake_gateway.add_attempts(), 0);
    assert_eq!(manager.shutdown().unwrap(), ShutdownReport::default());
    assert_eq!(fake_gateway.mappings().len(), 1);
}

#[test]
fn failed_replacement_rolls_back_new_mappings_and_retains_old_mappings() {
    let fake_gateway = FakeGateway::default();
    let mut manager = manager(fake_gateway.clone());
    manager
        .reconcile(&[endpoint([192, 168, 1, 10], 47989, true)])
        .unwrap();
    let old_mappings = fake_gateway.mappings();
    fake_gateway.set_add_failure(key(48000, Protocol::Udp));

    let error = manager
        .reconcile(&[endpoint([192, 168, 1, 20], 48000, true)])
        .unwrap_err();

    assert!(matches!(error, UpnpError::AddFailed { .. }));
    assert_eq!(fake_gateway.mappings(), old_mappings);
    assert_eq!(manager.owned_mappings().count(), 2);
    assert_eq!(fake_gateway.add_attempts(), 4);
}

#[test]
fn shutdown_removes_only_unchanged_mappings_owned_by_this_manager() {
    let fake_gateway = FakeGateway::default();
    let mut manager = manager(fake_gateway.clone());
    manager
        .reconcile(&[endpoint([192, 168, 1, 10], 47989, true)])
        .unwrap();
    let mut changed_udp_mapping = fake_gateway
        .mappings()
        .get(&key(47989, Protocol::Udp))
        .unwrap()
        .clone();
    changed_udp_mapping.internal_address = Ipv4Addr::new(192, 168, 1, 55);
    changed_udp_mapping.description = "replaced by another owner".into();
    fake_gateway.change_mapping(changed_udp_mapping.clone());

    let report = manager.shutdown().unwrap();

    assert_eq!(
        report,
        ShutdownReport {
            removed: 1,
            already_missing: 0,
            changed_mappings_preserved: 1,
        }
    );
    assert_eq!(manager.owned_mappings().count(), 0);
    assert_eq!(fake_gateway.mappings().len(), 1);
    assert_eq!(
        fake_gateway.mappings().get(&key(47989, Protocol::Udp)),
        Some(&changed_udp_mapping)
    );
}
