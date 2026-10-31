//! Serialized native effects run outside the host's input/media tick.
use super::{
    Event, Record, Work,
    compositor::{Compositor, Placement},
    connectors::ConnectorInventory,
    journal::Journal,
};
use crate::{
    linux_control::LinuxControlClient,
    model::{Specification, VirtualDisplayLayout},
    scale::ScaleSetting,
};
use std::sync::mpsc::{Receiver, SyncSender};

pub(super) trait Broker: Send {
    fn connected(&self, output: &str) -> Result<bool, String>;
    fn set_connected(&self, output: &str, connected: bool) -> Result<(), String>;
}
impl Broker for LinuxControlClient {
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
impl<B: Broker, C: Compositor, I: ConnectorInventory> Controller<B, C, I> {
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
    /// Exclusive placement leaves the outputs of other journaled Amaterasu
    /// streams enabled; only displays Amaterasu does not own are turned off.
    fn placement(&self, owned: &str, layout: VirtualDisplayLayout) -> Placement {
        match layout {
            VirtualDisplayLayout::Extended => Placement::Extended,
            VirtualDisplayLayout::Exclusive => Placement::Exclusive {
                keep: self
                    .journal
                    .records
                    .iter()
                    .map(|record| record.output.clone())
                    .filter(|output| output != owned)
                    .collect(),
            },
        }
    }
    fn remove(&mut self, key: &(String, String)) -> Result<(), String> {
        let Some(mut record) = self
            .journal
            .records
            .iter()
            .find(|record| &record.key() == key)
            .cloned()
        else {
            return Ok(());
        };
        if record.layout.is_none() {
            record.layout = self.compositor.snapshot(&record.output)?;
            self.journal.update(record.clone())?;
        }
        self.broker.set_connected(&record.output, false)?;
        self.compositor.restore(record.layout.as_ref())?;
        if self.broker.connected(&record.output)? {
            return Err("owned connector remains connected".into());
        }
        self.journal.remove(key)
    }
    fn recover(&mut self) -> Option<String> {
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
    }
    fn create_with_references(
        &mut self,
        owner: String,
        specification: Specification,
        scale: ScaleSetting,
        references: Option<std::collections::BTreeSet<String>>,
        layout: VirtualDisplayLayout,
    ) -> Result<Record, String> {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return Err("display manager stopping".into());
        }
        // Connector names follow the driver's current type ids, which a module
        // reload advances; never assume the first pool's Virtual-1..8 names.
        let mut selected = None;
        for output in self.connectors.connectors()? {
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
        let mut record = Record {
            owner,
            specification,
            output: selected.ok_or("no free virtual display connector")?,
            layout: None,
            references,
            transition: None,
        };
        record.layout = self.compositor.snapshot(&record.output)?;
        let placement = self.placement(&record.output, layout);
        self.journal.insert(record.clone())?;
        let result = (|| {
            self.broker.set_connected(&record.output, true)?;
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                return Err("display manager stopping".into());
            }
            self.compositor.configure(
                &record.output,
                &record.specification,
                record.layout.as_ref(),
                &placement,
                scale,
            )?;
            if !self.broker.connected(&record.output)? {
                return Err("owned connector disappeared".into());
            }
            // An extended stream's removal preserves the then-current layout,
            // including user changes made during it. An exclusive stream keeps
            // the pre-stream snapshot: removal re-enables the displays it
            // turned off, exactly as they were.
            if placement == Placement::Extended {
                record.layout = None;
                self.journal.update(record.clone())?;
            }
            Ok(record.clone())
        })();
        if result.is_err() {
            let _ = self.remove(&record.key());
        }
        result
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
) {
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
            error,
        })
        .is_err()
    {
        let _ = controller.recover();
        return;
    }
    while let Ok(work) = jobs.recv() {
        if stopped.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        let event = match work {
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
                Event::Created {
                    key,
                    result: controller.create_with_references(
                        owner,
                        specification,
                        scale,
                        references,
                        layout,
                    ),
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
                let result = controller.remove(&key);
                Event::Removed { key, result }
            }
        };
        if events.send(event).is_err() {
            break;
        }
    }
    // Only journaled Amaterasu connectors are eligible; the broker rechecks UID.
    let _ = controller.recover();
}

#[cfg(test)]
mod tests;
