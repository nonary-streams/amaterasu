//! A session's two alternating timing segments. Both are private, contain
//! complete records, and are pruned with their session. They never enter the
//! human log snapshot or the support ZIP.
use super::{LogRecord, ROLLOVER_SIZE};
use cap_std::fs::{Dir, OpenOptions};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::time::Instant;

#[derive(Default)]
pub(super) struct TimingCapture {
    files: [Option<File>; 2],
    active: usize,
    bytes: u64,
    retry_at: Option<Instant>,
    next_warning: Option<Instant>,
}

impl TimingCapture {
    pub(super) fn record(
        &mut self,
        directory: &Dir,
        label: &str,
        source: &str,
        target: &str,
        record: &LogRecord,
        timestamp: u64,
    ) -> Option<String> {
        let now = Instant::now();
        if self.retry_at.is_some_and(|retry| now < retry) {
            return None;
        }
        let error = self
            .write(directory, label, source, target, record, timestamp)
            .err()?;
        let malformed = error.kind() == io::ErrorKind::InvalidData;
        if !malformed {
            self.retry_at = Some(now + super::RETRY_INTERVAL);
        }
        if self.next_warning.is_some_and(|next| now < next) {
            return None;
        }
        self.next_warning = Some(now + super::RETRY_INTERVAL);
        Some(if malformed {
            format!("Skipped an invalid timing sample from {source}.")
        } else {
            format!(
                "Could not write the timing capture ({}). Will retry.",
                error.kind()
            )
        })
    }

    pub(super) fn write(
        &mut self,
        directory: &Dir,
        label: &str,
        source: &str,
        target: &str,
        record: &LogRecord,
        timestamp: u64,
    ) -> io::Result<()> {
        let payload: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&record.message)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid timing sample"))?;
        let mut line = serde_json::to_vec(&serde_json::json!({
            "timestamp": timestamp, "source": source, "target": target, "payload": payload
        }))?;
        line.push(b'\n');
        if self.bytes + line.len() as u64 > ROLLOVER_SIZE {
            let next = 1 - self.active;
            if let Some(file) = &mut self.files[next] {
                file.rewind()?;
                file.set_len(0)?;
            }
            self.active = next;
            self.bytes = 0;
        }
        let slot = &mut self.files[self.active];
        if slot.is_none() {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let name = format!("{label}.log.timing.{}.jsonl", self.active);
            *slot = Some(directory.open_with(name, &options)?.into_std());
        }
        let file = slot.as_mut().expect("timing segment opened");
        // Rewrite from the last complete line after a partial write.
        file.seek(SeekFrom::Start(self.bytes))?;
        if let Err(error) = file.write_all(&line) {
            file.set_len(self.bytes)?;
            return Err(error);
        }
        self.bytes += line.len() as u64;
        Ok(())
    }
}
