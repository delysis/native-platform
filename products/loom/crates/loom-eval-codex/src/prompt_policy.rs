use serde::Serialize;
use thiserror::Error;

pub(crate) const MAX_PARAGRAPH_BYTE_ANCHORS: usize = 8_192;

const SUSPICIOUS_ASCII_FRAGMENTS: &[&[u8]] = &[
    b"ignore previous",
    b"ignore all previous",
    b"ignore the rubric",
    b"ignore these instructions",
    b"system prompt",
    b"developer message",
    b"assistant message",
    b"winner_label",
    b"score_millionths",
    b"return only json",
    b"return exactly json",
    b"do not follow the instructions",
    b"you are chatgpt",
    b"you are codex",
    b"codex exec",
    b"tool call",
    b"<system",
    b"[system]",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptInjectionAssessment {
    NoKnownSuspicion,
    Suspected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct RawUtf8ParagraphAnchor {
    ordinal: u32,
    start: u64,
    end: u64,
}

impl RawUtf8ParagraphAnchor {
    #[cfg(test)]
    pub(crate) const fn start(self) -> u64 {
        self.start
    }

    #[cfg(test)]
    pub(crate) const fn end(self) -> u64 {
        self.end
    }
}

pub(crate) fn assess_prompt_injection(bytes: &[u8]) -> PromptInjectionAssessment {
    if SUSPICIOUS_ASCII_FRAGMENTS
        .iter()
        .any(|fragment| contains_ascii_case_insensitive(bytes, fragment))
    {
        PromptInjectionAssessment::Suspected
    } else {
        PromptInjectionAssessment::NoKnownSuspicion
    }
}

pub(crate) fn paragraph_byte_anchors(
    text: &str,
) -> Result<Vec<RawUtf8ParagraphAnchor>, PromptPolicyError> {
    let mut anchors = Vec::new();
    let mut paragraph_start = None;
    let mut paragraph_content_end = 0_usize;
    let mut line_start = 0_usize;

    for line in text.split_inclusive('\n') {
        let line_end = line_start
            .checked_add(line.len())
            .ok_or(PromptPolicyError::OffsetOverflow)?;
        let content = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line));
        if content.trim_matches([' ', '\t']).is_empty() {
            if let Some(start) = paragraph_start.take() {
                push_anchor(&mut anchors, start, paragraph_content_end)?;
            }
        } else {
            paragraph_start.get_or_insert(line_start);
            paragraph_content_end = line_start
                .checked_add(content.len())
                .ok_or(PromptPolicyError::OffsetOverflow)?;
        }
        line_start = line_end;
    }

    if let Some(start) = paragraph_start {
        push_anchor(&mut anchors, start, paragraph_content_end)?;
    }
    if anchors.is_empty() && !text.is_empty() {
        push_anchor(&mut anchors, 0, text.len())?;
    }
    Ok(anchors)
}

fn push_anchor(
    anchors: &mut Vec<RawUtf8ParagraphAnchor>,
    start: usize,
    end: usize,
) -> Result<(), PromptPolicyError> {
    if anchors.len() >= MAX_PARAGRAPH_BYTE_ANCHORS {
        return Err(PromptPolicyError::TooManyParagraphs(anchors.len() + 1));
    }
    let ordinal = u32::try_from(anchors.len()).map_err(|_| PromptPolicyError::OffsetOverflow)?;
    let start = u64::try_from(start).map_err(|_| PromptPolicyError::OffsetOverflow)?;
    let end = u64::try_from(end).map_err(|_| PromptPolicyError::OffsetOverflow)?;
    anchors.push(RawUtf8ParagraphAnchor {
        ordinal,
        start,
        end,
    });
    Ok(())
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.windows(needle.len()).any(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PromptPolicyError {
    #[error("manuscript contains more than the bounded paragraph-anchor count: {0}")]
    TooManyParagraphs(usize),
    #[error("manuscript byte-anchor arithmetic overflowed")]
    OffsetOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_raw_utf8_offsets_not_character_or_json_offsets() {
        let text = "Élan—first.\ncontinued.\n\n“Second,” she said.";
        let anchors = paragraph_byte_anchors(text).expect("anchors");
        assert_eq!(anchors.len(), 2);
        let first_start = usize::try_from(anchors[0].start()).expect("start fits");
        let first_end = usize::try_from(anchors[0].end()).expect("end fits");
        let second_start = usize::try_from(anchors[1].start()).expect("start fits");
        let second_end = usize::try_from(anchors[1].end()).expect("end fits");
        assert_eq!(
            &text.as_bytes()[first_start..first_end],
            b"\xC3\x89lan\xE2\x80\x94first.\ncontinued."
        );
        assert_eq!(
            &text.as_bytes()[second_start..second_end],
            "“Second,” she said.".as_bytes()
        );
    }

    #[test]
    fn meta_instruction_detection_is_ascii_case_insensitive_and_does_not_rewrite() {
        let bytes = b"A character whispers: IGNORE PREVIOUS orders.";
        assert_eq!(
            assess_prompt_injection(bytes),
            PromptInjectionAssessment::Suspected
        );
        assert_eq!(bytes, b"A character whispers: IGNORE PREVIOUS orders.");
        assert_eq!(
            assess_prompt_injection(b"She ignored the previous day's rain."),
            PromptInjectionAssessment::NoKnownSuspicion
        );
    }

    #[test]
    fn paragraph_map_rejects_more_than_its_explicit_bound() {
        let text = "x\n\n".repeat(MAX_PARAGRAPH_BYTE_ANCHORS + 1);
        assert!(matches!(
            paragraph_byte_anchors(&text),
            Err(PromptPolicyError::TooManyParagraphs(count))
                if count == MAX_PARAGRAPH_BYTE_ANCHORS + 1
        ));
    }
}
