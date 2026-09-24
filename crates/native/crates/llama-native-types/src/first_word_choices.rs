use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_categories::UnicodeCategories;
use unicode_normalization::UnicodeNormalization;

pub const FIRST_WORD_CHOICE_MAX_ATTEMPTS: u32 = 32;
pub const FIRST_WORD_CHOICE_MAX_PREFIX_TOKENS: u32 = 16;
pub const FIRST_WORD_CHOICE_MAX_CASES: usize = 4;
pub const FIRST_WORD_CHOICE_MAX_EOG_TOKENS: usize = 128;
pub const FIRST_WORD_CHOICE_MAX_INITIAL_EXCLUSIONS: usize = FIRST_WORD_CHOICE_MAX_EOG_TOKENS
    + FIRST_WORD_CHOICE_MAX_CASES * FIRST_WORD_CHOICE_MAX_ATTEMPTS as usize;

/// First-token sampling without replacement followed by complete lexical-word
/// rejection. On every initial draw, all model EOG IDs and prior proposal initial
/// IDs are masked to negative infinity BEFORE the configured sampler chain.
/// Tails retain the configured sampler. Seeds and each actual mask are recorded.
/// Slots reserve words in request order. This is NOT sampling conditioned exactly
/// on different lexical words: shared whitespace/punctuation/subword initial IDs
/// exclude all their possible continuations. Exhaustion returns no duplicate word.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FirstWordChoicePolicy {
    DistinctV2,
    /// Distinct lexical words whose withheld prefix must not begin with a
    /// complete angle-bracket markup construct. Rejected proposal bytes remain
    /// in the attempt ledger and never acquire stream authority.
    DistinctPlainTextV3,
}

impl FirstWordChoicePolicy {
    #[must_use]
    pub const fn rejects_leading_markup(self) -> bool {
        matches!(self, Self::DistinctPlainTextV3)
    }

    /// Attempt zero preserves the explicit caller seed. Later seeds are the
    /// first little-endian u32 of SHA256(domain || base_le || attempt_le), reduced
    /// modulo u32::MAX to exclude llama.cpp's nondeterministic seed sentinel.
    #[must_use]
    pub fn attempt_seed(self, base_seed: u32, attempt: u32) -> u32 {
        if attempt == 0 {
            return base_seed;
        }
        let mut digest = Sha256::new();
        digest.update(match self {
            Self::DistinctV2 => b"llama-native:first-word-distinct-v2\0".as_slice(),
            Self::DistinctPlainTextV3 => {
                b"llama-native:first-word-distinct-plain-text-v3\0".as_slice()
            }
        });
        digest.update(base_seed.to_le_bytes());
        digest.update(attempt.to_le_bytes());
        let bytes = digest.finalize();
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % u32::MAX
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FirstWordChoiceAttemptOutcome {
    Accepted,
    Duplicate,
    DisallowedPrefix,
    PrefixLimit,
    EndOfGeneration,
    Cancelled,
    InitialSupportExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadingMarkupStatus {
    NotMarkup,
    Pending,
    Complete,
}

/// Classify a leading angle-bracket construct without guessing from an
/// incomplete UTF-8 or token fragment. Leading whitespace is evidence, not
/// discarded output.
#[must_use]
pub fn leading_markup_status(text: &str) -> LeadingMarkupStatus {
    let prefix = text.trim_start_matches(char::is_whitespace);
    let bytes = prefix.as_bytes();
    if bytes.first() != Some(&b'<') {
        return LeadingMarkupStatus::NotMarkup;
    }
    if prefix.starts_with("<!--") {
        return if prefix.contains("-->") {
            LeadingMarkupStatus::Complete
        } else {
            LeadingMarkupStatus::Pending
        };
    }

    let mut index = 1;
    if bytes.get(index) == Some(&b'/') {
        index += 1;
    }
    let Some(first) = bytes.get(index) else {
        return LeadingMarkupStatus::Pending;
    };
    if matches!(first, b'!' | b'?') {
        return if prefix[index + 1..].contains('>') {
            LeadingMarkupStatus::Complete
        } else {
            LeadingMarkupStatus::Pending
        };
    }
    if !first.is_ascii_alphabetic() {
        return LeadingMarkupStatus::NotMarkup;
    }
    index += 1;
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    {
        index += 1;
    }
    let Some(close) = prefix[index..].find('>') else {
        return LeadingMarkupStatus::Pending;
    };
    if prefix[index..index + close].contains('<') {
        LeadingMarkupStatus::NotMarkup
    } else {
        LeadingMarkupStatus::Complete
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FirstWordChoiceAttempt {
    pub seed: u32,
    /// Sorted token IDs masked before this proposal's initial sampler application.
    /// Empty for cancellation before a proposal was started.
    pub initial_token_exclusions: Vec<i32>,
    /// Exact sampled, non-EOG proposal prefix. No rejected bytes are streamed.
    pub token_ids: Vec<i32>,
    pub terminal_token_id: Option<i32>,
    pub word_key: Option<String>,
    pub outcome: FirstWordChoiceAttemptOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FirstWordChoiceEvidence {
    pub policy: FirstWordChoicePolicy,
    pub attempts: Vec<FirstWordChoiceAttempt>,
    /// Zero-based admitted attempt, if any.
    pub selected_attempt: Option<u32>,
    /// All sampled non-EOG tokens, including rejected prefixes and accepted tail.
    pub total_attempted_tokens: u64,
    pub exhausted: bool,
}

fn lexical(character: char) -> bool {
    character == '_' || character.is_letter() || character.is_number() || character.is_mark()
}

/// Stable key only once a first lexical word is complete. Prefix punctuation
/// and whitespace are ignored for comparison, never removed from output bytes.
/// NFC precedes Unicode lowercase, matching Loom's displayed choice identity.
/// An unresolved trailing apostrophe/hyphen is held until its next scalar.
#[must_use]
pub fn complete_first_word_key(text: &str, terminal: bool) -> Option<String> {
    let start = text.char_indices().find(|(_, c)| lexical(*c))?.0;
    let mut characters = text[start..].char_indices().peekable();
    let mut end = 0;
    while let Some((offset, character)) = characters.next() {
        if lexical(character) {
            end = offset + character.len_utf8();
            continue;
        }
        if matches!(character, '\'' | '’' | '-') {
            match characters.peek() {
                Some((_, next)) if lexical(*next) => continue,
                None if !terminal => return None,
                _ => {}
            }
        }
        return Some(
            text[start..start + end]
                .nfc()
                .collect::<String>()
                .to_lowercase(),
        );
    }
    terminal.then(|| {
        text[start..start + end]
            .nfc()
            .collect::<String>()
            .to_lowercase()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_word_requires_a_boundary_and_preserves_lexical_joiners() {
        assert_eq!(complete_first_word_key("  The", false), None);
        assert_eq!(
            complete_first_word_key("  The ", false).as_deref(),
            Some("the")
        );
        assert_eq!(complete_first_word_key("‘Don't", false), None);
        assert_eq!(
            complete_first_word_key("‘Don't’ ", false).as_deref(),
            Some("don't")
        );
        assert_eq!(complete_first_word_key("well-", false), None);
        assert_eq!(
            complete_first_word_key("well-known.", false).as_deref(),
            Some("well-known")
        );
        assert_eq!(
            complete_first_word_key("Élan!", false),
            complete_first_word_key("E\u{301}lan ", false)
        );
        assert_eq!(complete_first_word_key("…", true), None);
        assert_eq!(complete_first_word_key("fin", true).as_deref(), Some("fin"));
    }

    #[test]
    fn attempt_seeds_are_explicit_repeatable_and_never_random_sentinel() {
        let policy = FirstWordChoicePolicy::DistinctV2;
        assert_eq!(policy.attempt_seed(41, 0), 41);
        let seeds = (0..FIRST_WORD_CHOICE_MAX_ATTEMPTS)
            .map(|attempt| policy.attempt_seed(41, attempt))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(seeds.len(), FIRST_WORD_CHOICE_MAX_ATTEMPTS as usize);
        assert!(!seeds.contains(&u32::MAX));
        assert_eq!(policy.attempt_seed(41, 1), policy.attempt_seed(41, 1));
        assert_ne!(
            policy.attempt_seed(41, 1),
            FirstWordChoicePolicy::DistinctPlainTextV3.attempt_seed(41, 1)
        );
    }

    #[test]
    fn plain_text_policy_waits_for_complete_markup_and_ignores_lookalikes() {
        for pending in ["<", "<str", "  <strong", "\n\t<strong", "<!-- comment"] {
            assert_eq!(
                leading_markup_status(pending),
                LeadingMarkupStatus::Pending,
                "{pending:?}"
            );
        }
        for rejected in [
            "<strong>",
            "  <strong class=\"sale\">words",
            "\r\n<strong>words",
            "</em> prose",
            "<x-tag/> prose",
            "<!-- comment --> prose",
        ] {
            assert_eq!(
                leading_markup_status(rejected),
                LeadingMarkupStatus::Complete,
                "{rejected:?}"
            );
        }
        for prose in ["<3 forever", "1 < 2", "< word", "ordinary prose"] {
            assert_eq!(
                leading_markup_status(prose),
                LeadingMarkupStatus::NotMarkup,
                "{prose:?}"
            );
        }
    }

    #[test]
    fn policy_and_attempt_outcome_have_stable_versioned_wire_names() {
        assert_eq!(
            serde_json::to_string(&FirstWordChoicePolicy::DistinctV2).expect("serialize v2"),
            r#""distinct_v2""#
        );
        assert_eq!(
            serde_json::to_string(&FirstWordChoicePolicy::DistinctPlainTextV3)
                .expect("serialize v3"),
            r#""distinct_plain_text_v3""#
        );
        assert_eq!(
            serde_json::to_string(&FirstWordChoiceAttemptOutcome::DisallowedPrefix)
                .expect("serialize outcome"),
            r#""disallowed_prefix""#
        );
    }
}
