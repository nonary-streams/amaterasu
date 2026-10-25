//! Serialized native effects run outside the host's input/media tick.
use super::{
    Event, Record, Work, compositor::Compositor, connectors::ConnectorInventory, journal::Journal,
};
use crate::{linux_control::LinuxControlClient, model::Specification};
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
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl<B: Broker, C: Compositor, I: ConnectorInventory> Controller<B, C, I> {
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
    fn create(&mut self, owner: String, specification: Specification) -> Result<Record, String> {
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
        };
        record.layout = self.compositor.snapshot(&record.output)?;
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
            )?;
            if !self.broker.connected(&record.output)? {
                return Err("owned connector disappeared".into());
            }
            // Once configured, future removal preserves the current layout,
            // including user changes made during this active stream.
            record.layout = None;
            self.journal.update(record.clone())?;
            Ok(record.clone())
        })();
        if result.is_err() {
            let _ = self.remove(&record.key());
        }
        result
    }
}
pub(super) fn run<B: Broker, C: Compositor, I: ConnectorInventory>(
    journal: Journal,
    broker: B,
    compositor: C,
    connectors: I,
    jobs: Receiver<Work>,
    events: SyncSender<Event>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let mut controller = Controller {
        journal,
        broker,
        compositor,
        connectors,
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
            Work::Create {
                owner,
                specification,
            } => {
                let key = (owner.clone(), specification.identity.clone());
                Event::Created {
                    key,
                    result: controller.create(owner, specification),
                }
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
