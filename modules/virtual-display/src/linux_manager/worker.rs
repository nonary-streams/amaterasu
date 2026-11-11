//! Serialized native effects run outside the host's input/media tick.
use super::{
    CreationFailure, Event, Record, Work,
    compositor::{Compositor, Placement},
    connectors::ConnectorInventory,
    journal::Journal,
    retirement::{Retirement, UNPLUGGED_ATTEMPTS},
};
use crate::{
    linux_control::LinuxControlClient,
    model::{Specification, VirtualDisplayLayout},
    scale::ScaleSetting,
};
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, SyncSender},
    time::Instant,
};

pub(super) trait Broker: Send {
    fn connected(&self, output: &str) -> Result<bool, String>;
    fn set_connected(&self, output: &str, connected: bool) -> Result<(), String>;
    fn request_mode(&self, output: &str, specification: &Specification) -> Result<(), String>;
}
impl Broker for LinuxControlClient {
    fn request_mode(&self, output: &str, specification: &Specification) -> Result<(), String> {
        LinuxControlClient::request_mode(
            self,
            output,
            specification.width,
            specification.height,
            specification.refresh_millihz,
        )
    }
    fn connected(&self, output: &str) -> Result<bool, String> {
        self.query_connector(output).map_err(|error| error.detail)
    }
    fn set_connected(&self, output: &str, connected: bool) -> Result<(), String> {
        match self.set_connector(output, connected) {
            Ok(actual) if actual == connected => Ok(()),
            Ok(_) => Err("broker connector readback differs".into()),
            Err(error) => Err(error.detail),
        }
    }
}
struct Controller<B: Broker, C: Compositor, I: ConnectorInventory> {
    journal: Journal,
    broker: B,
    compositor: C,
    connectors: I,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "default layout is used by test helpers")
    )]
    layout: VirtualDisplayLayout,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
struct PreparedCreation {
    record: Record,
    parked_key: Option<(String, String)>,
    placement: Placement,
}
/// What one retirement poll achieved.
struct ParkedRetirement {
    /// Owners whose parked output is gone from the journal.
    retired: Vec<(String, String)>,
    /// Nothing is left that the provider can usefully retry on its own.
    finished: bool,
}

/// Records why a saved desktop could not be confirmed. The provider's caller
/// only learns that its output was parked or removed.
fn report_unconfirmed_restore(output: &str, stage: &str, error: &str) {
    let detail: String = error.chars().take(480).collect();
    eprintln!(
        "amaterasu virtual display desktop restoration unconfirmed: output={output} stage={stage} error={detail:?}"
    );
}

impl<B: Broker, C: Compositor, I: ConnectorInventory> Controller<B, C, I> {
    fn retained_mode(
        &mut self,
        key: &(String, String),
        mut change: crate::retained_mode::Change,
        step: crate::retained_mode::Step,
    ) -> Result<Record, String> {
        let mut record = self
            .journal
            .records
            .iter()
            .find(|record| &record.key() == key)
            .cloned()
            .ok_or("missing owned display record")?;
        if record.parked
            || !change.references_match(record.references.as_ref())
            || !self.broker.connected(&record.output)?
        {
            return Err("retained mode ownership changed".into());
        }
        if step != crate::retained_mode::Step::Prepare {
            if record.retained_mode.as_ref() != Some(&change)
                || !change.valid(&record.specification)
            {
                return Err("retained mode transaction changed".into());
            }
            let expected = if step == crate::retained_mode::Step::Commit {
                &change.desired
            } else {
                &change.old
            };
            self.compositor
                .verify_retained_mode(&record.output, expected)?;
            if step == crate::retained_mode::Step::Commit {
                if change.aborted {
                    return Err("retained mode was aborted".into());
                }
                change.completed = true;
                record.specification = change.desired.clone();
            } else {
                if change.completed {
                    return Err("retained mode already committed".into());
                }
                change.aborted = true;
                record.specification = change.old.clone();
            }
        } else {
            if record.specification != change.old
                || record
                    .transition
                    .as_ref()
                    .is_some_and(|transition| !transition.completed)
                || record
                    .retained_mode
                    .as_ref()
                    .is_some_and(|previous| previous.is_pending() && previous != &change)
            {
                return Err("retained mode precondition changed".into());
            }
            self.compositor
                .verify_retained_mode(&record.output, &change.old)?;
            // A completed earlier transaction no longer owns a mutation. The
            // new durable intent records its own exact observed old/new modes.
            record.transition = None;
        }
        record.retained_mode = Some(change);
        if !record.valid() {
            return Err("invalid retained mode intent".into());
        }
        self.journal.update(record.clone())?;
        Ok(record)
    }

    fn park(&mut self, mut record: Record, reason: &str) -> Result<bool, String> {
        if !record
            .references
            .as_ref()
            .is_some_and(|refs| refs.is_empty())
            || record
                .transition
                .as_ref()
                .is_some_and(|transition| !transition.completed)
            || record
                .retained_mode
                .as_ref()
                .is_some_and(|change| change.is_pending())
        {
            return Err("cannot park an unresolved display owner".into());
        }
        // A retried retirement parks the same output again; record it once.
        if !record.parked || record.park_reason.as_deref() != Some(reason) {
            eprintln!(
                "amaterasu virtual display parked: output={} reason={reason}",
                record.output
            );
        }
        record.transition = None;
        record.parked = true;
        record.park_reason = Some(reason.into());
        self.journal.update(record)?;
        Ok(true)
    }
    /// The provider's own attempt to finish releasing outputs it parked, made
    /// only while no stream owns a display: an active stream's layout must not
    /// be replaced by an earlier desktop, and its creation reuses the parked
    /// output anyway. Returns the owners whose parked output is now retired.
    fn retire_parked(&mut self, retirement: &mut Retirement, now: Instant) -> ParkedRetirement {
        let parked: Vec<Record> = self
            .journal
            .records
            .iter()
            .filter(|record| record.parked)
            .cloned()
            .collect();
        let mut outcome = ParkedRetirement {
            retired: Vec::new(),
            finished: true,
        };
        if parked.is_empty() || parked.len() != self.journal.records.len() {
            // The active stream's own removal starts the pacing again.
            return outcome;
        }
        let displays = self.connectors.connected_displays().ok();
        let unplugged: Vec<bool> = parked
            .iter()
            .map(|record| {
                displays
                    .as_ref()
                    .is_some_and(|displays| !displays.virtual_outputs.contains(&record.output))
            })
            .collect();
        // An unplugged output is left to the next stream or restart once its
        // saved layout has been tried often enough.
        if retirement.attempts() >= UNPLUGGED_ATTEMPTS && unplugged.iter().all(|gone| *gone) {
            return outcome;
        }
        outcome.finished = false;
        if !retirement.due(now, displays.as_ref()) {
            return outcome;
        }
        let saved_layout_exhausted = retirement.attempts() >= UNPLUGGED_ATTEMPTS;
        for (record, unplugged) in parked.iter().zip(unplugged) {
            if unplugged && saved_layout_exhausted {
                continue;
            }
            let attempt = retirement.attempts() + 1;
            let result = match self.remove(&record.key()) {
                Ok(false) => {
                    outcome.retired.push(record.key());
                    "retired".to_owned()
                }
                Ok(true) => "still parked".to_owned(),
                Err(error) => format!("failed: {error}"),
            };
            if retirement.outcome_changed(&format!("{} {result}", record.output)) {
                eprintln!(
                    "amaterasu virtual display parked output retirement: output={} attempt={attempt} result={:?}",
                    record.output,
                    result.chars().take(480).collect::<String>()
                );
            }
        }
        retirement.attempted(now);
        outcome.finished = self.journal.records.iter().all(|record| !record.parked);
        outcome
    }
    /// Starts pacing retirement when an output is parked and stops when none is.
    fn pace_retirement(&self, retirement: Option<Retirement>, now: Instant) -> Option<Retirement> {
        if self.journal.records.iter().any(|record| record.parked) {
            Some(retirement.unwrap_or_else(|| Retirement::begin(now)))
        } else {
            None
        }
    }
    fn reconfigure(
        &mut self,
        key: &(String, String),
        mut transition: super::Transition,
        scale: ScaleSetting,
        layout: VirtualDisplayLayout,
    ) -> Result<Record, String> {
        let mut record = self
            .journal
            .records
            .iter()
            .find(|record| &record.key() == key)
            .cloned()
            .ok_or("missing owned display record")?;
        if record
            .transition
            .as_ref()
            .is_some_and(|previous| !previous.completed)
            || record.specification != transition.old
            || !self.broker.connected(&record.output)?
        {
            return Err("reconfigure precondition changed".into());
        }
        if transition.mode_only {
            transition.preservation_baseline = Some(self.compositor.mode_only_baseline(
                &record.output,
                &transition.old,
                &transition.desired,
            )?);
        }
        // The complete old/new intent remains available for cleanup even if the
        // compositor command partially executes or the process disappears.
        record.transition = Some(transition.clone());
        if !record.valid() {
            return Err("invalid reconfiguration journal record".into());
        }
        self.journal.update(record.clone())?;
        if transition.mode_only {
            self.compositor.apply_mode_only(
                &record.output,
                &transition.desired,
                transition
                    .preservation_baseline
                    .as_ref()
                    .ok_or("missing preservation baseline")?,
            )?;
        } else {
            let placement = self.placement(&record.output, layout);
            self.compositor.configure(
                &record.output,
                &transition.desired,
                None,
                &placement,
                scale,
                false,
            )?;
        }
        if !self.broker.connected(&record.output)? {
            return Err("owned connector disappeared during reconfiguration".into());
        }
        record.specification = transition.desired;
        record
            .transition
            .as_mut()
            .expect("persisted transition")
            .completed = true;
        self.journal.update(record.clone())?;
        Ok(record)
    }
    /// A peer holds an independent output; like the reference host, an
    /// exclusive request then extends the current desktop without disabling it.
    fn placement(&self, owned: &str, layout: VirtualDisplayLayout) -> Placement {
        match layout {
            VirtualDisplayLayout::Extended => Placement::Extended,
            VirtualDisplayLayout::ExtendedPrimary => Placement::ExtendedPrimary,
            VirtualDisplayLayout::ExtendedIsolated => Placement::ExtendedIsolated,
            VirtualDisplayLayout::ExtendedPrimaryIsolated => Placement::ExtendedPrimaryIsolated,
            VirtualDisplayLayout::Exclusive => {
                let keep = self
                    .journal
                    .records
                    .iter()
                    .map(|record| record.output.clone())
                    .filter(|output| output != owned)
                    .collect::<Vec<_>>();
                if keep.is_empty() {
                    Placement::Exclusive { keep }
                } else {
                    Placement::Extended
                }
            }
        }
    }
    /// Returns true when the connector remains parked as a desktop anchor.
    fn remove(&mut self, key: &(String, String)) -> Result<bool, String> {
        let Some(mut record) = self
            .journal
            .records
            .iter()
            .find(|record| &record.key() == key)
            .cloned()
        else {
            return Ok(false);
        };
        // A recreated pool assigns new DRM connector names. The broker cannot
        // query an old name once its native connector has disappeared.
        if !self.connectors.connectors()?.contains(&record.output) {
            if record
                .references
                .as_ref()
                .is_some_and(|refs| !refs.is_empty())
            {
                return Err("reference_retirement_required".into());
            }
            while let Some(layout) = record.setup_layouts.last() {
                self.compositor.restore(Some(layout))?;
                record.setup_layouts.pop();
                self.journal.update(record.clone())?;
            }
            self.compositor.restore(record.layout.as_ref())?;
            if !self.compositor.verify_removal_guard(&record.output)? {
                return Err("no surviving desktop output".into());
            }
            if self.connectors.connectors()?.contains(&record.output) {
                return Err("virtual connector reappeared during recovery".into());
            }
            self.journal.remove(key)?;
            return Ok(false);
        }
        let connected = self.broker.connected(&record.output)?;
        if record.layout.is_none() {
            record.layout = self.compositor.snapshot(&record.output)?;
            self.journal.update(record.clone())?;
        }
        if connected {
            // Status reports connection state, not ownership. Confirm the UID
            // lease after persisting the snapshot and before changing layout.
            self.broker.set_connected(&record.output, true)?;
        }
        while let Some(layout) = record.setup_layouts.last() {
            if let Err(error) = self.compositor.restore(Some(layout)) {
                report_unconfirmed_restore(&record.output, "setup_before_unplug", &error);
                // Reused outputs can owe an intermediate setup snapshot as
                // well as the original desktop. A readback mismatch in either
                // must not keep an idle stream display lit beside a survivor.
                // Preserve both snapshots until restoration is confirmed.
                if self.compositor.verify_removal_guard(&record.output) == Ok(true) {
                    if connected {
                        self.broker.set_connected(&record.output, false)?;
                    }
                    return self.park(record, "restore_failed");
                }
                if connected && self.compositor.verify_parked(&record.output).is_ok() {
                    return self.park(record, "restore_failed");
                }
                return Err(error);
            }
            record.setup_layouts.pop();
            self.journal.update(record.clone())?;
        }
        if connected {
            // Unplugging depends on an independently observed surviving
            // desktop, not on an exact readback of the saved layout. A monitor
            // that is on can take the desktop while one of its properties still
            // differs, and the layout is applied again once the stream output
            // is gone. Leaving the stream output lit beside that monitor is
            // what the user sees as a screen that never went away. Without a
            // survivor the stream output stays as the desktop's only screen.
            let restored = self.compositor.restore(record.layout.as_ref());
            if let Err(error) = &restored {
                report_unconfirmed_restore(&record.output, "before_unplug", error);
            }
            match (
                restored,
                self.compositor.verify_removal_guard(&record.output),
            ) {
                (_, Ok(true)) => {}
                (Ok(()), Ok(false)) => return self.park(record, "no_survivor"),
                (Ok(()), Err(error)) => return Err(error),
                (Err(_), _) => {
                    self.compositor.verify_parked(&record.output)?;
                    return self.park(record, "restore_failed");
                }
            }
            self.broker.set_connected(&record.output, false)?;
        }
        if let Err(error) = self.compositor.restore(record.layout.as_ref()) {
            report_unconfirmed_restore(&record.output, "after_unplug", &error);
            if !self.broker.connected(&record.output)?
                && self.compositor.verify_removal_guard(&record.output)?
            {
                return self.park(record, "restore_failed");
            }
            return Err(error);
        }
        if self.broker.connected(&record.output)? {
            return Err("owned connector remains connected".into());
        }
        self.journal.remove(key)?;
        Ok(false)
    }
    fn recover(&mut self) -> Option<String> {
        // Provider restart invalidates its borrows, not its consumers' durable
        // references. A saved layout can affect every output, so preserve the
        // entire desktop until each owning caller explicitly releases capture.
        if self.journal.records.iter().any(|record| {
            record
                .references
                .as_ref()
                .is_some_and(|references| !references.is_empty())
        }) {
            return Some("reference_retirement_required".into());
        }
        let keys: Vec<_> = self.journal.records.iter().map(Record::key).collect();
        let mut failure = None;
        for key in keys {
            if let Err(error) = self.remove(&key) {
                failure.get_or_insert(error);
            }
        }
        failure
    }
    #[cfg(test)]
    fn create(
        &mut self,
        owner: String,
        specification: Specification,
        scale: ScaleSetting,
    ) -> Result<Record, String> {
        self.create_with_references(owner, specification, scale, None, self.layout)
            .map_err(|failure| failure.error)
    }
    fn prepare_creation(
        &mut self,
        owner: String,
        specification: Specification,
        references: Option<std::collections::BTreeSet<String>>,
        layout: VirtualDisplayLayout,
    ) -> Result<PreparedCreation, String> {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return Err("display manager stopping".into());
        }
        // Connector names follow the driver's current type ids, which a module
        // reload advances; never assume the first pool's Virtual-1..8 names.
        let parked = self
            .journal
            .records
            .iter()
            .find(|record| record.parked)
            .cloned();
        if parked
            .as_ref()
            .is_some_and(|record| record.setup_layouts.len() >= 4)
        {
            return Err("too many unresolved setup layouts".into());
        }
        let available = self.connectors.connectors()?;
        let parked_output_available = parked
            .as_ref()
            .is_some_and(|record| available.contains(&record.output));
        let mut selected = None;
        if !parked_output_available {
            for output in available {
                if !self
                    .journal
                    .records
                    .iter()
                    .any(|record| record.output == output)
                    && !self.broker.connected(&output)?
                {
                    selected = Some(output);
                    break;
                }
            }
        }
        let mut record = Record {
            owner,
            specification,
            output: parked
                .as_ref()
                .filter(|_| parked_output_available)
                .map(|record| record.output.clone())
                .or(selected)
                .ok_or("no free virtual display connector")?,
            layout: parked.as_ref().and_then(|record| record.layout.clone()),
            setup_layouts: parked
                .as_ref()
                .map(|record| record.setup_layouts.clone())
                .unwrap_or_default(),
            references,
            transition: None,
            retained_mode: None,
            parked: false,
            park_reason: None,
        };
        if parked.is_none() {
            record.layout = self.compositor.snapshot(&record.output)?;
        } else {
            let current = self
                .compositor
                .snapshot(&record.output)?
                .ok_or("current desktop snapshot unavailable")?;
            record.setup_layouts.push(current);
        }
        let placement = self.placement(&record.output, layout);
        Ok(PreparedCreation {
            record,
            parked_key: parked.as_ref().map(Record::key),
            placement,
        })
    }

    fn create_with_references(
        &mut self,
        owner: String,
        specification: Specification,
        scale: ScaleSetting,
        references: Option<std::collections::BTreeSet<String>>,
        layout: VirtualDisplayLayout,
    ) -> Result<Record, CreationFailure> {
        // Selection and snapshotting do not mutate the journal or connector.
        // Failures here leave only the caller's in-memory admission to release.
        let PreparedCreation {
            mut record,
            parked_key,
            placement,
        } = self
            .prepare_creation(owner, specification, references, layout)
            .map_err(|error| CreationFailure {
                error,
                cleanup_complete: true,
            })?;
        let reused_parked = parked_key.is_some();
        let persisted = if let Some(previous) = parked_key {
            self.journal.replace_parked(&previous, record.clone())
        } else {
            self.journal.insert(record.clone())
        };
        persisted.map_err(|error| CreationFailure {
            error,
            // Atomic replacement may have happened before a sync error. Keep
            // this ownership obligation unresolved rather than infer absence.
            cleanup_complete: false,
        })?;
        let result = (|| {
            let newly_connected = !self.broker.connected(&record.output)?;
            // Even an already connected parked output must be checked by the
            // broker: status alone does not prove that this UID still owns it.
            self.broker.set_connected(&record.output, true)?;
            self.broker
                .request_mode(&record.output, &record.specification)?;
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                return Err("display manager stopping".into());
            }
            self.compositor.configure(
                &record.output,
                &record.specification,
                if reused_parked {
                    // Preserve the current desktop without reapplying the
                    // earlier failed restoration during the new setup.
                    record.setup_layouts.last()
                } else {
                    record.layout.as_ref()
                },
                &placement,
                scale,
                newly_connected,
            )?;
            if !self.broker.connected(&record.output)? {
                return Err("owned connector disappeared".into());
            }
            // An extended stream's removal preserves the then-current layout,
            // including user changes made during it. An exclusive stream keeps
            // the pre-stream snapshot: removal re-enables the displays it
            // turned off, exactly as they were.
            // A parked output can carry an unresolved earlier restoration;
            // retain that baseline across reuse even for an extended stream.
            if reused_parked {
                record.setup_layouts.pop();
                self.journal.update(record.clone())?;
            } else if matches!(placement, Placement::Extended | Placement::ExtendedIsolated) {
                record.layout = None;
                self.journal.update(record.clone())?;
            }
            Ok(record.clone())
        })();
        result.map_err(|error| CreationFailure {
            error,
            // A failed or parked cleanup retains its ownership record. Only
            // exact native retirement plus successful journal deletion proves
            // this failed creation no longer owns a resource.
            cleanup_complete: matches!(self.remove(&record.key()), Ok(false)),
        })
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) fn run<B: Broker, C: Compositor, I: ConnectorInventory>(
    journal: Journal,
    broker: B,
    compositor: C,
    connectors: I,
    layout: VirtualDisplayLayout,
    jobs: Receiver<Work>,
    events: SyncSender<Event>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    let mut controller = Controller {
        journal,
        broker,
        compositor,
        connectors,
        layout,
        stopped: stopped.clone(),
    };
    let error = controller.recover();
    if events
        .send(Event::Recovered {
            records: controller.journal.records.clone(),
            error: error.clone(),
        })
        .is_err()
    {
        return error.map_or(Ok(()), Err);
    }
    let mut retirement = controller.pace_retirement(None, Instant::now());
    'jobs: loop {
        let received = match retirement.as_mut() {
            None => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(pacing) => jobs.recv_timeout(pacing.wait(Instant::now())),
        };
        let work = match received {
            Ok(work) => work,
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if stopped.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let Some(pacing) = retirement.as_mut() else {
                    continue;
                };
                let outcome = controller.retire_parked(pacing, Instant::now());
                for key in outcome.retired {
                    if events.send(Event::ParkedRetired { key }).is_err() {
                        break 'jobs;
                    }
                }
                if outcome.finished {
                    retirement = None;
                }
                continue;
            }
        };
        if stopped.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        let event = match work {
            Work::RetainedMode { key, change, step } => {
                let result = controller.retained_mode(&key, change, step);
                Event::RetainedMode { key, result }
            }
            Work::VerifyModeOnly {
                key,
                output,
                desired,
                baseline,
            } => {
                let result = controller
                    .compositor
                    .verify_mode_only(&output, &desired, &baseline);
                Event::ModeOnlyVerified {
                    key,
                    result,
                    observed_at: std::time::Instant::now(),
                }
            }
            Work::Reconfigure {
                key,
                transition,
                scale,
                layout,
            } => {
                let result = controller.reconfigure(&key, transition, scale, layout);
                Event::Reconfigured { key, result }
            }
            Work::Create {
                owner,
                specification,
                scale,
                references,
                layout,
            } => {
                let key = (owner.clone(), specification.identity.clone());
                let result = controller.create_with_references(
                    owner,
                    specification,
                    scale,
                    references,
                    layout,
                );
                let parked = controller
                    .journal
                    .records
                    .iter()
                    .filter(|record| record.parked)
                    .map(|record| (record.key(), record.output.clone()))
                    .collect();
                Event::Created {
                    key,
                    result,
                    parked,
                }
            }
            Work::References {
                key,
                references,
                reply,
            } => {
                let result = controller
                    .journal
                    .records
                    .iter()
                    .find(|record| record.key() == key)
                    .cloned()
                    .ok_or_else(|| "missing owned display record".to_owned())
                    .and_then(|mut record| {
                        record.references = Some(references);
                        controller.journal.update(record)
                    });
                let _ = reply.send(result);
                continue;
            }
            Work::Remove { key } => {
                let result = controller.remove(&key).and_then(|parked| {
                    if !parked {
                        return Ok(None);
                    }
                    // A creation may have failed before its output was returned
                    // to the manager. Report parking from the durable owner,
                    // independent of that incomplete admission metadata.
                    controller
                        .journal
                        .records
                        .iter()
                        .find(|record| record.key() == key && record.parked)
                        .map(|record| Some(record.output.clone()))
                        .ok_or_else(|| "parked display ownership record missing".into())
                });
                if let Err(error) = &result {
                    let detail: String = error.chars().take(480).collect();
                    eprintln!("amaterasu virtual display removal failed: {detail:?}");
                }
                Event::Removed { key, result }
            }
        };
        if events.send(event).is_err() {
            break;
        }
        retirement = controller.pace_retirement(retirement, Instant::now());
    }
    // Only journaled Amaterasu connectors are eligible; the broker rechecks UID.
    controller.recover().map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests;
