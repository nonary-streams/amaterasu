//! Transactional operational handoff for explicitly supported predecessors.
//!
//! This is deliberately an abstraction: platform code supplies service control,
//! while product policy never accepts a service name or executable path from an
//! MSI property, config file, or command line.

use crate::PackageResult;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum LegacyFamily {
    Apollo,
    Sunshine,
    Vibeshine,
    Vibepollo,
}

impl LegacyFamily {
    pub const ALL: [Self; 4] = [
        Self::Apollo,
        Self::Sunshine,
        Self::Vibeshine,
        Self::Vibepollo,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyService {
    family: LegacyFamily,
    name: &'static str,
}

impl LegacyService {
    pub const fn for_family(family: LegacyFamily) -> Self {
        match family {
            LegacyFamily::Apollo => Self {
                family,
                name: "Apollo",
            },
            LegacyFamily::Sunshine => Self {
                family,
                name: "SunshineService",
            },
            LegacyFamily::Vibeshine => Self {
                family,
                name: "VibeshineService",
            },
            LegacyFamily::Vibepollo => Self {
                family,
                name: "VibepolloService",
            },
        }
    }

    pub const fn family(self) -> LegacyFamily {
        self.family
    }
    pub const fn name(self) -> &'static str {
        self.name
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StartMode {
    Disabled,
    Demand,
    Automatic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceSnapshot {
    pub exists: bool,
    pub running: bool,
    pub start_mode: StartMode,
}

/// Platform boundary. A Windows implementation may issue SCM calls only using
/// [`LegacyService`] values minted above; test implementations use synthetic
/// in-memory services.
pub trait ServiceController {
    fn snapshot(&mut self, service: LegacyService) -> PackageResult<ServiceSnapshot>;
    fn stop(&mut self, service: LegacyService) -> PackageResult<()>;
    fn start(&mut self, service: LegacyService) -> PackageResult<()>;
    fn set_start_mode(&mut self, service: LegacyService, mode: StartMode) -> PackageResult<()>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffRecord {
    pub family: LegacyFamily,
    pub before: ServiceSnapshot,
    /// Durable intent written before asking SCM to stop this service. A stop
    /// request can take effect even when its caller loses the response, so
    /// recovery treats an attempted stop as needing restoration.
    #[serde(default)]
    pub stop_attempted: bool,
    pub stopped: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MsiRegistration {
    pub family: LegacyFamily,
    pub product_code: String,
}

/// Registry boundary used before stopping a predecessor. An EXE-style entry is
/// intentionally not executable by this migration path: nested/unbounded
/// uninstallers are not safe inside a replacement transaction.
pub trait RegistrationReader {
    fn registration(&mut self, family: LegacyFamily) -> PackageResult<RegistrationState>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistrationState {
    Absent,
    Msi(MsiRegistration),
    ExeOnly,
}

pub fn capture_msi_registrations<R: RegistrationReader>(
    reader: &mut R,
    families: impl IntoIterator<Item = LegacyFamily>,
) -> PackageResult<Vec<MsiRegistration>> {
    let mut registrations = Vec::new();
    for family in families {
        match reader.registration(family)? {
            RegistrationState::Absent => {}
            RegistrationState::Msi(registration)
                if registration.family == family
                    && valid_product_code(&registration.product_code) =>
            {
                registrations.push(registration)
            }
            RegistrationState::Msi(_) => {
                return Err("predecessor MSI registration is invalid".into());
            }
            RegistrationState::ExeOnly => {
                return Err("legacy EXE-only uninstall registration is unsupported".into());
            }
        }
    }
    Ok(registrations)
}

pub fn valid_product_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 38
        && bytes[0] == b'{'
        && bytes[37] == b'}'
        && bytes[1..37].iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Captures predecessor state before any stop. Callers must complete legacy
/// configuration validation before [`stop_after_import_validation`] is allowed.
pub fn preflight<C: ServiceController>(
    controller: &mut C,
    families: impl IntoIterator<Item = LegacyFamily>,
) -> PackageResult<Vec<HandoffRecord>> {
    let mut records = Vec::new();
    for family in families {
        let service = LegacyService::for_family(family);
        let before = controller.snapshot(service)?;
        if before.exists {
            records.push(HandoffRecord {
                family,
                before,
                stop_attempted: false,
                stopped: false,
            });
        }
    }
    Ok(records)
}

pub fn stop_after_import_validation<C: ServiceController>(
    controller: &mut C,
    records: &mut [HandoffRecord],
    import_validated: bool,
) -> PackageResult<()> {
    if !import_validated {
        return Err("legacy configuration has not passed import validation".into());
    }
    for record in records {
        if record.before.running {
            controller.stop(LegacyService::for_family(record.family))?;
            record.stopped = true;
        }
    }
    Ok(())
}

/// Stops services only after a caller has atomically persisted a recovery
/// record before *and* after every SCM mutation. If a persistence operation
/// fails, every service whose stop might have been requested is restored
/// before returning the error.
pub fn stop_with_recovery_state<C: ServiceController>(
    controller: &mut C,
    records: &mut [HandoffRecord],
    import_validated: bool,
    persist: &mut dyn FnMut(&[HandoffRecord]) -> PackageResult<()>,
) -> PackageResult<()> {
    if !import_validated {
        return Err("legacy configuration has not passed import validation".into());
    }
    for index in 0..records.len() {
        if !records[index].before.running {
            continue;
        }
        records[index].stop_attempted = true;
        if let Err(error) = persist(records) {
            return fail_with_rollback(controller, records, error);
        }
        if let Err(error) = controller.stop(LegacyService::for_family(records[index].family)) {
            return fail_with_rollback(controller, records, error);
        }
        records[index].stopped = true;
        if let Err(error) = persist(records) {
            return fail_with_rollback(controller, records, error);
        }
    }
    Ok(())
}

fn fail_with_rollback<C: ServiceController>(
    controller: &mut C,
    records: &mut [HandoffRecord],
    error: String,
) -> PackageResult<()> {
    match rollback(controller, records) {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(format!(
            "{error}; predecessor recovery also failed: {rollback_error}"
        )),
    }
}

/// Re-establishes exactly the captured service state. It is idempotent: only a
/// service this transaction stopped is restarted; absent services are ignored.
pub fn rollback<C: ServiceController>(
    controller: &mut C,
    records: &mut [HandoffRecord],
) -> PackageResult<()> {
    for record in records.iter_mut().rev() {
        if !record.stop_attempted && !record.stopped {
            continue;
        }
        let service = LegacyService::for_family(record.family);
        controller.set_start_mode(service, record.before.start_mode)?;
        if record.before.running {
            controller.start(service)?;
        }
        record.stop_attempted = false;
        record.stopped = false;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct SyntheticController {
        states: BTreeMap<LegacyFamily, ServiceSnapshot>,
        stops: usize,
        starts: usize,
    }
    impl ServiceController for SyntheticController {
        fn snapshot(&mut self, service: LegacyService) -> PackageResult<ServiceSnapshot> {
            Ok(self.states[&service.family()])
        }
        fn stop(&mut self, service: LegacyService) -> PackageResult<()> {
            self.stops += 1;
            self.states.get_mut(&service.family()).unwrap().running = false;
            Ok(())
        }
        fn start(&mut self, service: LegacyService) -> PackageResult<()> {
            self.starts += 1;
            self.states.get_mut(&service.family()).unwrap().running = true;
            Ok(())
        }
        fn set_start_mode(&mut self, service: LegacyService, mode: StartMode) -> PackageResult<()> {
            self.states.get_mut(&service.family()).unwrap().start_mode = mode;
            Ok(())
        }
    }
    fn controller() -> SyntheticController {
        SyntheticController {
            states: BTreeMap::from([(
                LegacyFamily::Sunshine,
                ServiceSnapshot {
                    exists: true,
                    running: true,
                    start_mode: StartMode::Automatic,
                },
            )]),
            stops: 0,
            starts: 0,
        }
    }
    #[test]
    fn stops_only_after_validation_and_rollback_restores_the_synthetic_service() {
        let mut controller = controller();
        let mut records = preflight(&mut controller, [LegacyFamily::Sunshine]).unwrap();
        assert!(stop_after_import_validation(&mut controller, &mut records, false).is_err());
        assert_eq!(controller.stops, 0);
        stop_after_import_validation(&mut controller, &mut records, true).unwrap();
        assert_eq!(controller.stops, 1);
        rollback(&mut controller, &mut records).unwrap();
        rollback(&mut controller, &mut records).unwrap();
        assert_eq!(controller.starts, 1);
        assert!(controller.states[&LegacyFamily::Sunshine].running);
    }

    #[test]
    fn persistence_failure_after_a_stop_restores_every_possibly_touched_service() {
        let mut controller = controller();
        let mut records = preflight(&mut controller, [LegacyFamily::Sunshine]).unwrap();
        let mut calls = 0;
        let error = stop_with_recovery_state(&mut controller, &mut records, true, &mut |_| {
            calls += 1;
            if calls == 2 {
                Err("injected state write failure".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(error.contains("injected state write failure"));
        assert_eq!(controller.stops, 1);
        assert_eq!(controller.starts, 1);
        assert!(controller.states[&LegacyFamily::Sunshine].running);
        assert!(!records[0].stop_attempted);
    }

    #[test]
    fn persistence_failure_before_stop_leaves_the_service_running() {
        let mut controller = controller();
        let mut records = preflight(&mut controller, [LegacyFamily::Sunshine]).unwrap();
        assert!(
            stop_with_recovery_state(&mut controller, &mut records, true, &mut |_| Err(
                "no disk".into()
            ))
            .is_err()
        );
        assert_eq!(controller.stops, 0);
        assert!(controller.states[&LegacyFamily::Sunshine].running);
    }

    struct SyntheticRegistry(RegistrationState);
    impl RegistrationReader for SyntheticRegistry {
        fn registration(&mut self, _: LegacyFamily) -> PackageResult<RegistrationState> {
            Ok(self.0.clone())
        }
    }
    #[test]
    fn captures_only_msi_product_codes_and_blocks_exe_uninstallers_before_stop() {
        let mut msi = SyntheticRegistry(RegistrationState::Msi(MsiRegistration {
            family: LegacyFamily::Sunshine,
            product_code: "{01234567-89AB-CDEF-0123-456789ABCDEF}".into(),
        }));
        assert_eq!(
            capture_msi_registrations(&mut msi, [LegacyFamily::Sunshine])
                .unwrap()
                .len(),
            1
        );
        let mut exe = SyntheticRegistry(RegistrationState::ExeOnly);
        assert!(capture_msi_registrations(&mut exe, [LegacyFamily::Sunshine]).is_err());
    }

    #[test]
    fn every_fixed_family_has_the_same_synthetic_discovery_and_handoff_contract() {
        for family in LegacyFamily::ALL {
            let mut registry = SyntheticRegistry(RegistrationState::Msi(MsiRegistration {
                family,
                product_code: "{01234567-89AB-CDEF-0123-456789ABCDEF}".into(),
            }));
            let registrations = capture_msi_registrations(&mut registry, [family]).unwrap();
            assert_eq!(registrations.len(), 1);

            let mut controller = SyntheticController {
                states: BTreeMap::from([(
                    family,
                    ServiceSnapshot {
                        exists: true,
                        running: true,
                        start_mode: StartMode::Automatic,
                    },
                )]),
                stops: 0,
                starts: 0,
            };
            let mut records = preflight(&mut controller, [family]).unwrap();
            stop_after_import_validation(&mut controller, &mut records, true).unwrap();
            rollback(&mut controller, &mut records).unwrap();
            assert_eq!(controller.stops, 1);
            assert_eq!(controller.starts, 1);
        }
    }
}
