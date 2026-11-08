//! Provider-owned adjustment leases. Every native effect follows a durable intent.
use amaterasu_module_sdk::display_settings::{
    Adjustment, Lease, MAX_LEASES, MAX_SCOPES, Operation, Scope, Target, valid_id,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub target: Target,
    pub source: String,
    pub settings: Adjustment,
}
pub trait Backend {
    fn enumerate(&mut self) -> Result<Vec<Observation>>;
    /// Apply only named properties and verify their actual values before success.
    fn apply(&mut self, target: &Target, adjustment: &Adjustment) -> Result<()>;
}
pub trait Store {
    fn save(&mut self, state: &State) -> Result<()>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeRecord {
    owner: String,
    consumers: BTreeSet<String>,
    scope: Scope,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseRecord {
    owner: String,
    lease: Lease,
    adjustment: Adjustment,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetRecord {
    observation: Observation,
    leases: BTreeMap<String, LeaseRecord>,
    pending: Option<Adjustment>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    version: u32,
    targets: BTreeMap<String, TargetRecord>,
    scopes: BTreeMap<String, ScopeRecord>,
    closed_scopes: BTreeMap<String, Target>,
    released_leases: BTreeSet<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            targets: BTreeMap::new(),
            scopes: BTreeMap::new(),
            closed_scopes: BTreeMap::new(),
            released_leases: BTreeSet::new(),
        }
    }
}
impl State {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.scopes.len() > MAX_SCOPES
            || self.targets.len() > MAX_SCOPES
            || self.closed_scopes.len() > 1024
            || self.released_leases.len() > 4096
            || self.targets.values().map(|t| t.leases.len()).sum::<usize>() > MAX_LEASES
        {
            return Err("invalid_adjustment_journal".into());
        }
        for (id, s) in &self.scopes {
            if !valid_id(&s.scope.id)
                || !valid_id(id)
                || !valid_id(&s.owner)
                || s.consumers.len() > 16
                || s.consumers.iter().any(|p| !valid_id(p))
                || !self
                    .targets
                    .get(&s.scope.target.id)
                    .is_some_and(|t| t.observation.target == s.scope.target)
            {
                return Err("invalid_adjustment_scope".into());
            }
        }
        for (id, t) in &self.targets {
            if id != &t.observation.target.id
                || !valid_id(id)
                || !valid_id(&t.observation.target.generation)
            {
                return Err("invalid_adjustment_target".into());
            }
            for (lid, l) in &t.leases {
                if lid != &l.lease.id
                    || l.lease.scope.target != t.observation.target
                    || !self
                        .scopes
                        .values()
                        .any(|s| s.scope == l.lease.scope && s.consumers.contains(&l.owner))
                {
                    return Err("invalid_adjustment_lease".into());
                }
                l.adjustment.validate()?;
            }
            merged(t)?;
        }
        Ok(())
    }
}
fn merged(t: &TargetRecord) -> Result<Adjustment> {
    let mut patch = Adjustment::default();
    for lease in t.leases.values() {
        patch.merge(&lease.adjustment)?;
    }
    Ok(patch)
}
fn identity(owner: &str, operation: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(format!("{owner}\0{operation}")))
}

pub struct Engine<B, S> {
    pub backend: B,
    pub store: S,
    state: State,
    registrars: BTreeSet<String>,
    consumers: BTreeSet<String>,
}
impl<B: Backend, S: Store> Engine<B, S> {
    pub fn new(
        backend: B,
        store: S,
        state: State,
        registrars: BTreeSet<String>,
        consumers: BTreeSet<String>,
    ) -> Result<Self> {
        state.validate()?;
        Ok(Self {
            backend,
            store,
            state,
            registrars,
            consumers,
        })
    }
    fn commit(&mut self, mut next: State) -> Result<()> {
        // Closed receipts are bounded. New scopes use fresh identities, so
        // evicting a receipt can never revive an old plugin's scope handle.
        while next.closed_scopes.len() > 1024 {
            next.closed_scopes.pop_first();
        }
        while next.released_leases.len() > 4096 {
            next.released_leases.pop_first();
        }
        next.targets.retain(|id, target| {
            target.pending.is_some()
                || !target.leases.is_empty()
                || next
                    .scopes
                    .values()
                    .any(|scope| scope.scope.target.id == *id)
        });
        next.validate()?;
        self.store.save(&next)?;
        self.state = next;
        Ok(())
    }
    fn observe(&mut self, target: &Target) -> Result<Observation> {
        self.backend
            .enumerate()?
            .into_iter()
            .find(|o| &o.target == target)
            .ok_or_else(|| "display_target_unavailable".into())
    }
    pub fn reconcile(&mut self) -> Result<()> {
        let targets: Vec<_> = self
            .state
            .targets
            .values()
            .map(|t| t.observation.target.clone())
            .collect();
        let mut failure = None;
        for target in targets {
            if let Err(error) = self.reconcile_target(&target) {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn reconcile_target(&mut self, target: &Target) -> Result<()> {
        let Some(record) = self.state.targets.get(&target.id) else {
            return Ok(());
        };
        if record.observation.target != *target {
            return Err("display_generation_conflict".into());
        }
        let desired = record.pending.clone().unwrap_or(merged(record)?);
        if desired == Adjustment::default() {
            return Ok(());
        }
        let current = self.observe(target)?;
        if !matches_patch(&current.settings, &desired) {
            self.backend.apply(target, &desired)?;
        }
        let verified = self.observe(target)?;
        if !matches_patch(&verified.settings, &desired) {
            return Err("display_adjustment_unverified".into());
        }
        if self.state.targets[&target.id].pending.is_some() {
            let mut next = self.state.clone();
            next.targets.get_mut(&target.id).unwrap().pending = None;
            self.commit(next)?;
        }
        Ok(())
    }
    pub fn execute(&mut self, caller: &str, operation: Operation) -> Result<Value> {
        if !self.registrars.contains(caller) && !self.consumers.contains(caller) {
            return Err("permission_denied".into());
        }
        match operation {
            Operation::Enumerate => Ok(
                json!({"ok":true,"displays":self.backend.enumerate()?.into_iter().map(|o|json!({"target":o.target,"settings":o.settings})).collect::<Vec<_>>()}),
            ),
            Operation::Read { target } => {
                Ok(json!({"ok":true,"target":target,"settings":self.observe(&target)?.settings}))
            }
            Operation::OpenScope {
                operation_id,
                source,
                consumers,
            } => self.open_scope(caller, operation_id, source, consumers),
            Operation::Acquire {
                operation_id,
                scope,
                adjustment,
            } => self.acquire(caller, operation_id, scope, adjustment),
            Operation::Release { lease } => self.release(caller, lease),
            Operation::CloseScope { operation_id } => self.close_scope(caller, operation_id),
            Operation::Status { scope } => {
                self.authorize(caller, &scope)?;
                self.reconcile_target(&scope.target)?;
                Ok(json!({"ok":true,"scope":scope,"enforced":true}))
            }
        }
    }
    fn authorize(&self, caller: &str, scope: &Scope) -> Result<()> {
        if !self
            .state
            .scopes
            .values()
            .any(|s| s.scope == *scope && (s.owner == caller || s.consumers.contains(caller)))
        {
            return Err("display_scope_denied".into());
        }
        Ok(())
    }
    fn open_scope(
        &mut self,
        caller: &str,
        operation_id: String,
        source: String,
        consumers: Vec<String>,
    ) -> Result<Value> {
        if !self.registrars.contains(caller)
            || !valid_id(&operation_id)
            || consumers.is_empty()
            || consumers.len() > 16
            || consumers.iter().any(|p| !self.consumers.contains(p))
        {
            return Err("display_scope_denied".into());
        }
        let id = identity(caller, &operation_id);
        let consumers: BTreeSet<_> = consumers.into_iter().collect();
        if self.state.closed_scopes.contains_key(&id) {
            return Err("display_scope_closed".into());
        }
        let observation = self
            .backend
            .enumerate()?
            .into_iter()
            .find(|o| o.source == source)
            .ok_or("display_source_unavailable")?;
        if let Some(existing) = self.state.scopes.get(&id) {
            if existing.consumers != consumers || existing.scope.target != observation.target {
                return Err("display_scope_conflict".into());
            }
            return Ok(json!({"ok":true,"scope":existing.scope}));
        }
        let scope = Scope {
            id: amaterasu_storage::random_identifier()?,
            target: observation.target.clone(),
        };
        let mut next = self.state.clone();
        if let Some(old) = next.targets.get(&scope.target.id)
            && old.observation.target != scope.target
        {
            return Err("display_generation_conflict".into());
        }
        next.targets
            .entry(scope.target.id.clone())
            .or_insert(TargetRecord {
                observation,
                leases: BTreeMap::new(),
                pending: None,
            });
        next.scopes.insert(
            id,
            ScopeRecord {
                owner: caller.into(),
                consumers,
                scope: scope.clone(),
            },
        );
        self.commit(next)?;
        Ok(json!({"ok":true,"scope":scope}))
    }
    fn acquire(
        &mut self,
        caller: &str,
        operation_id: String,
        scope: Scope,
        adjustment: Adjustment,
    ) -> Result<Value> {
        self.authorize(caller, &scope)?;
        adjustment.validate()?;
        if !valid_id(&operation_id) {
            return Err("invalid_operation_id".into());
        }
        self.reconcile_target(&scope.target)?;
        let id = identity(caller, &format!("{operation_id}:{}", scope.id));
        if self.state.released_leases.contains(&id) {
            return Err("display_lease_released".into());
        }
        let lease = Lease {
            id: id.clone(),
            scope: scope.clone(),
        };
        if let Some(old) = self.state.targets.values().find_map(|t| t.leases.get(&id)) {
            if old.owner != caller || old.lease != lease || old.adjustment != adjustment {
                return Err("display_adjustment_conflict".into());
            }
            return Ok(json!({"ok":true,"lease":lease,"enforced":true}));
        }
        let current = self.observe(&scope.target)?;
        let mut next = self.state.clone();
        let target = next.targets.get_mut(&scope.target.id).unwrap();
        let previous = merged(target)?;
        // Capture original values when each property first becomes leased, not
        // when a scope was registered earlier or another property was acquired.
        capture_baseline(
            &mut target.observation.settings,
            &previous,
            &adjustment,
            &current.settings,
        )?;
        target.leases.insert(
            id,
            LeaseRecord {
                owner: caller.into(),
                lease: lease.clone(),
                adjustment,
            },
        );
        target.pending = Some(merged(target)?);
        self.commit(next)?;
        self.reconcile_target(&scope.target)?;
        Ok(json!({"ok":true,"lease":lease,"enforced":true}))
    }
    fn release(&mut self, caller: &str, lease: Lease) -> Result<Value> {
        if self.state.released_leases.contains(&lease.id) {
            self.reconcile_target(&lease.scope.target)?;
            return Ok(json!({"ok":true,"released":true}));
        }
        self.authorize(caller, &lease.scope)?;
        let mut next = self.state.clone();
        let target = next
            .targets
            .get_mut(&lease.scope.target.id)
            .ok_or("display_target_unavailable")?;
        let old = target
            .leases
            .get(&lease.id)
            .ok_or("display_lease_unavailable")?;
        if old.owner != caller || old.lease != lease {
            return Err("display_lease_denied".into());
        }
        let previous = merged(target)?;
        target.leases.remove(&lease.id);
        let mut desired = merged(target)?;
        desired.restore_missing(&previous, &target.observation.settings);
        target.pending = Some(desired);
        next.released_leases.insert(lease.id.clone());
        self.commit(next)?;
        self.reconcile_target(&lease.scope.target)?;
        Ok(json!({"ok":true,"released":true}))
    }
    fn close_scope(&mut self, caller: &str, operation_id: String) -> Result<Value> {
        if !self.registrars.contains(caller) || !valid_id(&operation_id) {
            return Err("display_scope_denied".into());
        }
        let id = identity(caller, &operation_id);
        if let Some(target) = self.state.closed_scopes.get(&id).cloned() {
            match self.observe(&target) {
                Ok(_) => self.reconcile_target(&target)?,
                Err(error) if error == "display_target_unavailable" => {}
                Err(error) => return Err(error),
            }
            return Ok(json!({"ok":true,"closed":true}));
        }
        let Some(scope) = self.state.scopes.get(&id).cloned() else {
            return Ok(json!({"ok":true,"closed":true}));
        };
        if scope.owner != caller {
            return Err("display_scope_denied".into());
        }
        let present = match self.observe(&scope.scope.target) {
            Ok(_) => true,
            Err(error) if error == "display_target_unavailable" => false,
            Err(error) => return Err(error),
        };
        let mut next = self.state.clone();
        let target = next.targets.get_mut(&scope.scope.target.id).unwrap();
        let previous = merged(target)?;
        let removed: Vec<_> = target
            .leases
            .iter()
            .filter(|(_, l)| l.lease.scope == scope.scope)
            .map(|(id, _)| id.clone())
            .collect();
        for lease in removed {
            target.leases.remove(&lease);
            next.released_leases.insert(lease);
        }
        let mut desired = merged(target)?;
        desired.restore_missing(&previous, &target.observation.settings);
        target.pending = (present && desired != Adjustment::default()).then_some(desired);
        next.scopes.remove(&id);
        next.closed_scopes.insert(id, scope.scope.target.clone());
        self.commit(next)?;
        if present {
            self.reconcile_target(&scope.scope.target)?;
        }
        Ok(json!({"ok":true,"closed":true}))
    }
}
pub(crate) fn matches_patch(actual: &Adjustment, wanted: &Adjustment) -> bool {
    macro_rules! same {
        ($f:ident) => {
            wanted.$f.is_none_or(|v| actual.$f == Some(v))
        };
    }
    same!(hdr)
        && same!(wide_color_gamut)
        && same!(resolution)
        && same!(refresh_millihz)
        && same!(scale)
        && same!(position)
}
fn capture_baseline(
    baseline: &mut Adjustment,
    previous: &Adjustment,
    patch: &Adjustment,
    current: &Adjustment,
) -> Result<()> {
    macro_rules! capture {
        ($f:ident) => {
            if patch.$f.is_some() && previous.$f.is_none() {
                baseline.$f = Some(current.$f.ok_or("unsupported_display_setting")?);
            }
        };
    }
    capture!(hdr);
    capture!(wide_color_gamut);
    capture!(resolution);
    capture!(refresh_millihz);
    capture!(scale);
    capture!(position);
    Ok(())
}
#[cfg(test)]
mod tests;
