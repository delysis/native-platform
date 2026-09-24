//! Bounded proposal admission, independent of KV/sampler ownership. Rejected
//! proposals never acquire permission to emit text; the worker owns reset.
use std::collections::BTreeSet;

use llama_native_types::{
    FIRST_WORD_CHOICE_MAX_ATTEMPTS, FIRST_WORD_CHOICE_MAX_PREFIX_TOKENS, FirstWordChoiceAttempt,
    FirstWordChoiceAttemptOutcome, FirstWordChoiceEvidence, FirstWordChoicePolicy,
    complete_first_word_key,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Admission {
    Pending,
    Accepted,
    Retry,
    Exhausted,
}

pub(super) struct FirstWordGate {
    pub evidence: FirstWordChoiceEvidence,
    base_seed: u32,
    initial_token_exclusions: Vec<i32>,
}

impl FirstWordGate {
    pub fn new(policy: FirstWordChoicePolicy, base_seed: u32) -> Self {
        Self {
            evidence: FirstWordChoiceEvidence {
                policy,
                attempts: Vec::new(),
                selected_attempt: None,
                total_attempted_tokens: 0,
                exhausted: false,
            },
            base_seed,
            initial_token_exclusions: Vec::new(),
        }
    }

    pub fn pending(&self) -> bool {
        self.evidence.selected_attempt.is_none()
            && !self.evidence.exhausted
            && !self
                .evidence
                .attempts
                .last()
                .is_some_and(|attempt| attempt.outcome == FirstWordChoiceAttemptOutcome::Cancelled)
    }

    pub fn seed(&self) -> u32 {
        self.evidence
            .policy
            .attempt_seed(self.base_seed, self.evidence.attempts.len() as u32)
    }

    pub fn record_nonterminal_token(&mut self) {
        self.evidence.total_attempted_tokens += 1;
    }

    pub fn begin_attempt(&mut self, exclusions: &BTreeSet<i32>) {
        self.initial_token_exclusions = exclusions.iter().copied().collect();
    }

    pub fn exhaust_initial_support(&mut self) {
        self.evidence.attempts.push(FirstWordChoiceAttempt {
            seed: self.seed(),
            initial_token_exclusions: std::mem::take(&mut self.initial_token_exclusions),
            token_ids: Vec::new(),
            terminal_token_id: None,
            word_key: None,
            outcome: FirstWordChoiceAttemptOutcome::InitialSupportExhausted,
        });
        self.evidence.exhausted = true;
    }

    pub fn observe(
        &mut self,
        text: &str,
        tokens: &[i32],
        terminal_token_id: Option<i32>,
        finished: bool,
        reserved: &mut BTreeSet<String>,
    ) -> Admission {
        let visual_prose = self.evidence.policy == FirstWordChoicePolicy::DistinctVisualProseV3;
        let disallowed_prefix = visual_prose
            && (completed_leading_html_tag(text)
                || has_leading_line_break(text)
                || completed_leading_punctuation_line(text));
        let pending_html_prefix = self.evidence.policy
            == FirstWordChoicePolicy::DistinctVisualProseV3
            && possible_incomplete_leading_html_tag(text);
        let word_key = (!disallowed_prefix && !pending_html_prefix)
            .then(|| complete_first_word_key(text, terminal_token_id.is_some()))
            .flatten();
        let numeric_only_word = visual_prose
            && word_key
                .as_deref()
                .is_some_and(|word| !word.chars().any(char::is_alphabetic));
        let outcome = if disallowed_prefix || numeric_only_word {
            FirstWordChoiceAttemptOutcome::DisallowedPrefix
        } else if let Some(word) = &word_key {
            if reserved.contains(word) {
                FirstWordChoiceAttemptOutcome::Duplicate
            } else {
                reserved.insert(word.clone());
                FirstWordChoiceAttemptOutcome::Accepted
            }
        } else if terminal_token_id.is_some() {
            FirstWordChoiceAttemptOutcome::EndOfGeneration
        } else if finished || tokens.len() >= FIRST_WORD_CHOICE_MAX_PREFIX_TOKENS as usize {
            FirstWordChoiceAttemptOutcome::PrefixLimit
        } else {
            return Admission::Pending;
        };
        let accepted = outcome == FirstWordChoiceAttemptOutcome::Accepted;
        self.evidence.attempts.push(FirstWordChoiceAttempt {
            seed: self.seed(),
            initial_token_exclusions: std::mem::take(&mut self.initial_token_exclusions),
            token_ids: tokens.to_vec(),
            terminal_token_id,
            word_key,
            outcome,
        });
        if accepted {
            self.evidence.selected_attempt = Some(self.evidence.attempts.len() as u32 - 1);
            Admission::Accepted
        } else if self.evidence.attempts.len() < FIRST_WORD_CHOICE_MAX_ATTEMPTS as usize {
            Admission::Retry
        } else {
            self.evidence.exhausted = true;
            Admission::Exhausted
        }
    }

    pub fn cancel(&mut self, tokens: &[i32]) {
        if !self.pending() {
            return;
        }
        self.evidence.attempts.push(FirstWordChoiceAttempt {
            seed: self.seed(),
            initial_token_exclusions: std::mem::take(&mut self.initial_token_exclusions),
            token_ids: tokens.to_vec(),
            terminal_token_id: None,
            word_key: None,
            outcome: FirstWordChoiceAttemptOutcome::Cancelled,
        });
    }
}

fn leading_non_whitespace(text: &str) -> &str {
    text.trim_start_matches(char::is_whitespace)
}

fn possible_html_tag_body(body: &str) -> bool {
    let body = body.strip_prefix('/').unwrap_or(body);
    body.chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && !body.contains('<')
}

fn completed_leading_html_tag(text: &str) -> bool {
    let Some(rest) = leading_non_whitespace(text).strip_prefix('<') else {
        return false;
    };
    let Some(end) = rest.find('>') else {
        return false;
    };
    possible_html_tag_body(&rest[..end])
}

fn completed_leading_punctuation_line(text: &str) -> bool {
    let Some((first_line, _)) = leading_non_whitespace(text).split_once('\n') else {
        return false;
    };
    let first_line = first_line.trim_end_matches('\r').trim();
    !first_line.is_empty()
        && !first_line
            .chars()
            .any(|character| character == '_' || character.is_alphanumeric())
}

fn has_leading_line_break(text: &str) -> bool {
    text.chars()
        .take_while(|character| character.is_whitespace())
        .any(|character| matches!(character, '\r' | '\n'))
}

fn possible_incomplete_leading_html_tag(text: &str) -> bool {
    let Some(rest) = leading_non_whitespace(text).strip_prefix('<') else {
        return false;
    };
    !rest.contains('>') && possible_html_tag_body(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_distinguishes_waiting_slot_from_started_proposal() {
        let mut waiting = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 1);
        waiting.cancel(&[]);
        assert!(
            waiting.evidence.attempts[0]
                .initial_token_exclusions
                .is_empty()
        );
        assert!(waiting.evidence.attempts[0].token_ids.is_empty());

        let mut started = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 1);
        started.begin_attempt(&BTreeSet::from([9, 0, 7]));
        started.record_nonterminal_token();
        started.cancel(&[3]);
        assert_eq!(
            started.evidence.attempts[0].initial_token_exclusions,
            [0, 7, 9]
        );
        assert_eq!(started.evidence.attempts[0].token_ids, [3]);
        assert_eq!(started.evidence.total_attempted_tokens, 1);
        assert!(!waiting.pending());
        assert!(!started.pending());
    }

    #[test]
    fn duplicate_prefix_is_rejected_before_any_emission_permission() {
        let mut words = BTreeSet::from(["the".to_string()]);
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 41);
        assert_eq!(
            gate.observe("Th", &[1], None, false, &mut words),
            Admission::Pending
        );
        assert_eq!(
            gate.observe("The ", &[1, 2], None, false, &mut words),
            Admission::Retry
        );
        assert_eq!(gate.evidence.selected_attempt, None);
        assert_eq!(gate.evidence.attempts[0].token_ids, [1, 2]);
        assert_eq!(
            gate.observe("Another ", &[3], None, false, &mut words),
            Admission::Accepted
        );
        assert_eq!(gate.evidence.selected_attempt, Some(1));
        assert_eq!(
            gate.evidence.attempts[1].seed,
            FirstWordChoicePolicy::DistinctV2.attempt_seed(41, 1)
        );
    }

    #[test]
    fn visual_prose_policy_withholds_fragmented_html_and_retries_a_complete_tag() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctVisualProseV3, 41);
        assert_eq!(
            gate.observe("<str", &[1], None, false, &mut words),
            Admission::Pending
        );
        assert_eq!(
            gate.observe("<strong>", &[1, 2], None, false, &mut words),
            Admission::Retry
        );
        assert_eq!(
            gate.evidence.attempts[0].outcome,
            FirstWordChoiceAttemptOutcome::DisallowedPrefix
        );
        assert_eq!(gate.evidence.attempts[0].word_key, None);
        assert_eq!(
            gate.observe(" Across ", &[3], None, false, &mut words),
            Admission::Accepted
        );
        assert_eq!(
            gate.evidence.attempts[1].word_key.as_deref(),
            Some("across")
        );
    }

    #[test]
    fn visual_prose_policy_retries_a_completed_punctuation_only_leading_line() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctVisualProseV3, 41);
        assert_eq!(
            gate.observe(".", &[1], None, false, &mut words),
            Admission::Pending
        );
        assert_eq!(
            gate.observe(
                ".\nLoom native smoke prose: The",
                &[1, 2],
                None,
                false,
                &mut words
            ),
            Admission::Retry
        );
        assert_eq!(
            gate.evidence.attempts[0].outcome,
            FirstWordChoiceAttemptOutcome::DisallowedPrefix
        );
        assert_eq!(
            gate.observe("Lantern light ", &[3], None, false, &mut words),
            Admission::Accepted
        );
    }

    #[test]
    fn visual_prose_policy_retries_leading_line_break_and_numeric_only_word() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctVisualProseV3, 41);
        assert_eq!(
            gate.observe("\nfloor. ", &[1], None, false, &mut words),
            Admission::Retry
        );
        assert_eq!(
            gate.observe("0805705.", &[2], None, false, &mut words),
            Admission::Retry
        );
        assert_eq!(
            gate.observe("100-year-old ", &[3], None, false, &mut words),
            Admission::Accepted
        );
        assert!(
            gate.evidence.attempts[..2].iter().all(|attempt| {
                attempt.outcome == FirstWordChoiceAttemptOutcome::DisallowedPrefix
            })
        );
    }

    #[test]
    fn visual_prose_policy_exhausts_bounded_markup_proposals_without_admission() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctVisualProseV3, 7);
        for attempt in 0..FIRST_WORD_CHOICE_MAX_ATTEMPTS {
            let result = gate.observe("<b>", &[attempt as i32], None, false, &mut words);
            assert_eq!(
                result,
                if attempt + 1 == FIRST_WORD_CHOICE_MAX_ATTEMPTS {
                    Admission::Exhausted
                } else {
                    Admission::Retry
                }
            );
        }
        assert!(gate.evidence.exhausted);
        assert_eq!(gate.evidence.selected_attempt, None);
        assert!(words.is_empty());
        assert!(gate.evidence.attempts.iter().all(|attempt| {
            attempt.outcome == FirstWordChoiceAttemptOutcome::DisallowedPrefix
                && attempt.word_key.is_none()
        }));
    }

    #[test]
    fn bounded_shortfall_never_admits_duplicate_or_partial_word() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 2);
        let tokens = vec![9; FIRST_WORD_CHOICE_MAX_PREFIX_TOKENS as usize];
        for attempt in 0..FIRST_WORD_CHOICE_MAX_ATTEMPTS {
            let result = gate.observe("unfinished", &tokens, None, false, &mut words);
            assert_eq!(
                result,
                if attempt + 1 == FIRST_WORD_CHOICE_MAX_ATTEMPTS {
                    Admission::Exhausted
                } else {
                    Admission::Retry
                }
            );
        }
        assert!(words.is_empty());
        assert!(gate.evidence.exhausted);
        assert_eq!(gate.evidence.selected_attempt, None);
    }

    #[test]
    fn eog_can_complete_a_word_but_cancellation_cannot() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 3);
        gate.cancel(&[8]);
        assert!(!gate.pending());
        assert_eq!(gate.evidence.selected_attempt, None);
        assert_eq!(
            gate.evidence.attempts[0].outcome,
            FirstWordChoiceAttemptOutcome::Cancelled
        );
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 3);
        assert_eq!(
            gate.observe("fin", &[8], Some(99), true, &mut words),
            Admission::Accepted
        );
        assert_eq!(gate.evidence.attempts[0].terminal_token_id, Some(99));
    }

    #[test]
    fn token_limit_is_not_a_word_boundary_and_rejected_work_remains_charged() {
        let mut words = BTreeSet::new();
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 17);
        gate.record_nonterminal_token();
        assert_eq!(
            gate.observe("part", &[7], None, true, &mut words),
            Admission::Retry
        );
        gate.record_nonterminal_token();
        assert_eq!(
            gate.observe("word ", &[8], None, false, &mut words),
            Admission::Accepted
        );
        gate.record_nonterminal_token(); // An admitted continuation token.
        assert_eq!(gate.evidence.total_attempted_tokens, 3);
        assert_eq!(
            gate.evidence.attempts[0].outcome,
            FirstWordChoiceAttemptOutcome::PrefixLimit
        );
        assert_eq!(gate.evidence.attempts[1].token_ids, [8]);
    }
}
