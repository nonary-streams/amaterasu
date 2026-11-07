//! Fair, bounded selection of pending input revocations.

use std::collections::BTreeSet;
use std::ops::Bound::{Excluded, Included, Unbounded};

const MAX_REVOCATIONS_PER_TURN: usize = 8;

/// Remembers only where to resume scanning; the coordinator retains each job.
#[derive(Default)]
pub(super) struct RevocationSchedule {
    last_token: Option<String>,
}

impl RevocationSchedule {
    /// Select each token at most once, continuing after the previous selection.
    ///
    /// The cursor remains meaningful when its token is removed. New tokens are
    /// visited on a later pass, including those inserted before the cursor.
    pub(super) fn select(&self, tokens: &BTreeSet<String>) -> Vec<String> {
        let mut selected = Vec::with_capacity(tokens.len().min(MAX_REVOCATIONS_PER_TURN));
        if let Some(last_token) = &self.last_token {
            selected.extend(
                tokens
                    .range::<String, _>((Excluded(last_token), Unbounded))
                    .take(MAX_REVOCATIONS_PER_TURN)
                    .cloned(),
            );
            if selected.len() < MAX_REVOCATIONS_PER_TURN {
                selected.extend(
                    tokens
                        .range::<String, _>((Unbounded, Included(last_token)))
                        .take(MAX_REVOCATIONS_PER_TURN - selected.len())
                        .cloned(),
                );
            }
        } else {
            selected.extend(tokens.iter().take(MAX_REVOCATIONS_PER_TURN).cloned());
        }
        selected
    }

    /// Advance before attempting each token, even when that attempt fails.
    pub(super) fn mark_attempt(&mut self, token: &str) {
        self.last_token = Some(token.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn pending_first_batch_does_not_starve_the_ninth_or_replace_jobs() {
        let mut schedule = RevocationSchedule::default();
        let mut tokens: BTreeSet<_> = (0..9).map(|number| format!("token-{number:02}")).collect();
        let mut jobs = BTreeMap::new();

        let first = schedule.select(&tokens);
        assert_eq!(first.len(), 8);
        for (request, token) in first.iter().enumerate() {
            schedule.mark_attempt(token);
            jobs.entry(token.clone()).or_insert(request + 100);
        }
        let original_jobs = jobs.clone();

        // All eight requests are still pending when the next turn is admitted.
        let second = schedule.select(&tokens);
        assert_eq!(second.len(), 8);
        assert_eq!(second[0], "token-08");
        for token in &second {
            schedule.mark_attempt(token);
            jobs.entry(token.clone()).or_insert(108);
        }
        for (token, request) in original_jobs {
            assert_eq!(jobs[&token], request);
        }
        assert_eq!(jobs["token-08"], 108);

        // The previous cursor is removed while other jobs remain pending.
        tokens.remove(second.last().unwrap());
        tokens.remove("token-08");
        tokens.insert("token-00a".into());
        let third = schedule.select(&tokens);
        assert_eq!(third.len(), tokens.len());
        assert_eq!(third.iter().collect::<BTreeSet<_>>().len(), third.len());
        assert!(third.contains(&"token-00a".into()));
        assert!(!third.contains(&"token-08".into()));
    }

    #[test]
    fn wraps_after_a_removed_cursor_and_visits_new_earlier_tokens() {
        let mut schedule = RevocationSchedule::default();
        let mut tokens: BTreeSet<_> = (0..10).map(|number| format!("{number:02}")).collect();
        let first = schedule.select(&tokens);
        assert_eq!(first[7], "07");
        for token in first {
            schedule.mark_attempt(&token);
        }
        tokens.remove("07");
        tokens.insert("00a".into());
        let next = schedule.select(&tokens);
        assert_eq!(&next[..3], &["08", "09", "00"]);
        assert!(next.contains(&"00a".into()));
        assert_eq!(next.iter().collect::<BTreeSet<_>>().len(), next.len());
    }

    #[test]
    fn empty_and_single_token_scans_are_safe() {
        let mut schedule = RevocationSchedule::default();
        let mut tokens = BTreeSet::new();
        assert!(schedule.select(&tokens).is_empty());
        tokens.insert("only".into());
        assert_eq!(schedule.select(&tokens), ["only"]);
        schedule.mark_attempt("only");
        assert_eq!(schedule.select(&tokens), ["only"]);
        tokens.clear();
        assert!(schedule.select(&tokens).is_empty());
        tokens.insert("another".into());
        assert_eq!(schedule.select(&tokens), ["another"]);
    }

    #[test]
    fn an_error_in_a_batch_does_not_skip_unattempted_tokens() {
        let mut schedule = RevocationSchedule::default();
        let tokens: BTreeSet<_> = (0..10).map(|number| format!("{number:02}")).collect();
        let batch = schedule.select(&tokens);
        schedule.mark_attempt(&batch[0]);
        // Simulate returning from the caller immediately after the first token.
        assert_eq!(schedule.select(&tokens)[0], "01");
    }
}
