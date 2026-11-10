//! Releases held controller input when the client stops sending control
//! traffic.
//!
//! Moonlight sends a periodic ping every 100 ms (older clients send loss
//! statistics every 50 ms), but a lost peer is only declared after the
//! transport's ten-second timeout. Controller samples are sent only when they
//! change, so until then the virtual controller keeps the client's last stick
//! and button state and a game keeps acting on it, e.g. turning the camera.
//! After a short silence the controllers are sent a neutral state instead.
//! The devices stay attached, and the client's next sample after traffic
//! resumes restores its real input.
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Five missed periodic pings: long enough to ride out jitter, short enough
/// that a held stick stops acting almost at once.
pub(super) const CLIENT_SILENCE_MS: u64 = 500;

#[derive(Debug, Default)]
pub(super) struct ClientSilence {
    last_heard: Option<u64>,
    /// Controllers whose last forwarded state was not neutral, by number, with
    /// the active mask that state carried.
    held: BTreeMap<u64, Value>,
    released: bool,
}

impl ClientSilence {
    /// Any control message, including a periodic ping, proves the client is
    /// still sending.
    pub(super) fn heard(&mut self, now: u64) {
        self.last_heard = Some(now);
        self.released = false;
    }

    /// Track the controller state just queued for the input provider.
    pub(super) fn forwarded(&mut self, event: &Value) {
        if event["kind"] != "controller" {
            return;
        }
        let Some(number) = event["number"].as_u64() else {
            return;
        };
        if is_neutral(event) {
            self.held.remove(&number);
        } else {
            self.held.insert(number, event["active_mask"].clone());
        }
    }

    /// Neutral states for every held controller, once per silence.
    pub(super) fn release_if_silent(&mut self, now: u64) -> Vec<Value> {
        let silent = self
            .last_heard
            .is_some_and(|heard| now.saturating_sub(heard) >= CLIENT_SILENCE_MS);
        if !silent || self.released {
            return Vec::new();
        }
        self.released = true;
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|(number, active_mask)| neutral_controller(number, active_mask))
            .collect()
    }
}

fn is_neutral(event: &Value) -> bool {
    [
        "buttons",
        "left_trigger",
        "right_trigger",
        "left_x",
        "left_y",
        "right_x",
        "right_y",
    ]
    .iter()
    .all(|field| event[*field].as_i64().unwrap_or(0) == 0)
}

fn neutral_controller(number: u64, active_mask: Value) -> Value {
    json!({
        "kind":"controller",
        "number":number,
        "active_mask":active_mask,
        "buttons":0,
        "left_trigger":0,
        "right_trigger":0,
        "left_x":0,
        "left_y":0,
        "right_x":0,
        "right_y":0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(number: u64, right_x: i64) -> Value {
        json!({"kind":"controller","number":number,"active_mask":3,"buttons":0,
            "left_trigger":0,"right_trigger":0,"left_x":0,"left_y":0,"right_x":right_x,"right_y":0})
    }

    #[test]
    fn a_held_stick_is_released_once_after_client_silence() {
        let mut silence = ClientSilence::default();
        silence.heard(1_000);
        silence.forwarded(&sample(0, 20_000));
        silence.forwarded(&sample(1, 0));
        assert!(
            silence
                .release_if_silent(1_000 + CLIENT_SILENCE_MS - 1)
                .is_empty()
        );
        let released = silence.release_if_silent(1_000 + CLIENT_SILENCE_MS);
        assert_eq!(released, [neutral_controller(0, json!(3))]);
        assert!(is_neutral(&released[0]));
        assert!(
            silence.release_if_silent(10_000).is_empty(),
            "released once per silence"
        );
    }

    #[test]
    fn traffic_rearms_and_neutral_or_idle_controllers_send_nothing() {
        let mut silence = ClientSilence::default();
        assert!(
            silence.release_if_silent(60_000).is_empty(),
            "nothing heard yet"
        );
        silence.heard(0);
        silence.forwarded(&sample(0, 20_000));
        silence.forwarded(&sample(0, 0));
        assert!(
            silence.release_if_silent(CLIENT_SILENCE_MS).is_empty(),
            "already neutral"
        );

        silence.heard(1_000);
        silence.forwarded(&sample(2, -5));
        assert_eq!(
            silence.release_if_silent(1_000 + CLIENT_SILENCE_MS).len(),
            1
        );
        silence.heard(2_000);
        silence.forwarded(&sample(2, -5));
        assert_eq!(
            silence.release_if_silent(2_000 + CLIENT_SILENCE_MS).len(),
            1,
            "a later silence releases again"
        );
    }

    #[test]
    fn non_controller_events_are_ignored() {
        let mut silence = ClientSilence::default();
        silence.heard(0);
        silence.forwarded(&json!({"kind":"key","code":30,"down":true}));
        assert!(silence.release_if_silent(CLIENT_SILENCE_MS).is_empty());
    }
}
