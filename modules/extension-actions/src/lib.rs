//! Bounded action receipt bookkeeping for extension action admission.
//! Execution requires the lifecycle coordinator's authority and is deliberately
//! not inferred from payload identities.
pub mod dispatch;
pub mod ledger;

pub mod validation;

mod service;
amaterasu_module_sdk::export_module!(service::ActionsModule);

pub(crate) mod worker;
