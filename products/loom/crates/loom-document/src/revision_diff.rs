//! Read-only presentation diffs. These spans describe exact supplied UTF-8
//! snapshots, not authorship, intent, preference, or a patch to apply.
use std::ops::Range;

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

/// Combined input bytes. Presentation must not allocate an unbounded history.
pub const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOKENS: usize = 64 * 1024;
const MAX_DIFF_WORK: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    /// Preserve line/block context and refine changed blocks at Unicode words.
    Words,
    /// Useful for verse and inspecting Markdown structure; every newline stays.
    Lines,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Equal,
    Insert,
    Delete,
    Replace,
}

/// Ranges are UTF-8 byte offsets in the two original snapshots. All spans,
/// including unchanged context, partition both inputs in order without loss.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiffSpan {
    pub kind: ChangeKind,
    pub before: Range<usize>,
    pub after: Range<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionDiff {
    pub granularity: Granularity,
    /// A bounded fallback represents an entire changed block as a replacement.
    /// It is still exact, but does not claim a minimal word-level alignment.
    pub coarse_blocks: usize,
    pub spans: Vec<DiffSpan>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum DiffError {
    #[error("revision comparison exceeds the combined two MiB input limit")]
    InputLimit,
}

/// Aligns lines first, then words within replacements. No whitespace, Unicode,
/// Markdown, or line-ending normalization is performed. Reordered text remains
/// an insertion/deletion; the diff makes no speculative move or intent claim.
///
/// Work is bounded across both stages. When alignment would exceed the budget,
/// the affected block is returned intact and `coarse_blocks` is incremented.
pub fn compare(
    before: &str,
    after: &str,
    granularity: Granularity,
) -> Result<RevisionDiff, DiffError> {
    if before.len().saturating_add(after.len()) > MAX_INPUT_BYTES {
        return Err(DiffError::InputLimit);
    }
    let mut work = MAX_DIFF_WORK;
    let mut result = RevisionDiff {
        granularity,
        coarse_blocks: 0,
        spans: Vec::new(),
    };
    let blocks = align(before, after, Granularity::Lines, &mut work);
    for block in blocks.spans {
        if granularity == Granularity::Words && block.kind == ChangeKind::Replace {
            let words = align(
                &before[block.before.clone()],
                &after[block.after.clone()],
                Granularity::Words,
                &mut work,
            );
            result.coarse_blocks += usize::from(words.coarse);
            for mut word in words.spans {
                word.before.start += block.before.start;
                word.before.end += block.before.start;
                word.after.start += block.after.start;
                word.after.end += block.after.start;
                push_span(&mut result.spans, word);
            }
        } else {
            result.coarse_blocks += usize::from(blocks.coarse);
            push_span(&mut result.spans, block);
        }
    }
    Ok(result)
}

struct Alignment {
    coarse: bool,
    spans: Vec<DiffSpan>,
}

fn tokens(text: &str, granularity: Granularity) -> Option<Vec<&str>> {
    let parts: Vec<_> = match granularity {
        Granularity::Words => text.split_word_bounds().take(MAX_TOKENS + 1).collect(),
        Granularity::Lines => text.split_inclusive('\n').take(MAX_TOKENS + 1).collect(),
    };
    (parts.len() <= MAX_TOKENS).then_some(parts)
}

fn align(before: &str, after: &str, granularity: Granularity, work: &mut usize) -> Alignment {
    if before == after || before.is_empty() || after.is_empty() {
        return single(before, after, false);
    }
    let (Some(old), Some(new)) = (tokens(before, granularity), tokens(after, granularity)) else {
        return single(before, after, true);
    };
    // Strip equal tokens, never individual bytes or partial graphemes.
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    let cost = (old_end - prefix)
        .saturating_add(new_end - prefix)
        .saturating_pow(2);
    if cost > *work {
        return single(before, after, true);
    }
    *work -= cost;
    let old_offsets = offsets(&old);
    let new_offsets = offsets(&new);
    let mut spans = Vec::new();
    push_span(
        &mut spans,
        DiffSpan {
            kind: ChangeKind::Equal,
            before: 0..old_offsets[prefix],
            after: 0..new_offsets[prefix],
        },
    );
    for op in capture_diff_slices(
        Algorithm::Myers,
        &old[prefix..old_end],
        &new[prefix..new_end],
    ) {
        let (tag, old_range, new_range) = op.as_tag_tuple();
        push_span(
            &mut spans,
            DiffSpan {
                kind: match tag {
                    DiffTag::Equal => ChangeKind::Equal,
                    DiffTag::Insert => ChangeKind::Insert,
                    DiffTag::Delete => ChangeKind::Delete,
                    DiffTag::Replace => ChangeKind::Replace,
                },
                before: old_offsets[prefix + old_range.start]..old_offsets[prefix + old_range.end],
                after: new_offsets[prefix + new_range.start]..new_offsets[prefix + new_range.end],
            },
        );
    }
    push_span(
        &mut spans,
        DiffSpan {
            kind: ChangeKind::Equal,
            before: old_offsets[old_end]..before.len(),
            after: new_offsets[new_end]..after.len(),
        },
    );
    Alignment {
        coarse: false,
        spans,
    }
}

fn offsets(tokens: &[&str]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(tokens.len() + 1);
    offsets.push(0);
    let mut offset = 0;
    for token in tokens {
        offset += token.len();
        offsets.push(offset);
    }
    offsets
}

fn single(before: &str, after: &str, coarse: bool) -> Alignment {
    let kind = if before == after {
        ChangeKind::Equal
    } else if before.is_empty() {
        ChangeKind::Insert
    } else if after.is_empty() {
        ChangeKind::Delete
    } else {
        ChangeKind::Replace
    };
    let mut spans = Vec::new();
    push_span(
        &mut spans,
        DiffSpan {
            kind,
            before: 0..before.len(),
            after: 0..after.len(),
        },
    );
    Alignment { coarse, spans }
}

fn push_span(spans: &mut Vec<DiffSpan>, span: DiffSpan) {
    if span.before.is_empty() && span.after.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.kind == span.kind
        && last.before.end == span.before.start
        && last.after.end == span.after.start
    {
        last.before.end = span.before.end;
        last.after.end = span.after.end;
    } else {
        spans.push(span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reconstruct(before: &str, after: &str, diff: &RevisionDiff) {
        let (mut old, mut new) = (String::new(), String::new());
        let (mut old_end, mut new_end) = (0, 0);
        for span in &diff.spans {
            assert_eq!(span.before.start, old_end);
            assert_eq!(span.after.start, new_end);
            let a = &before[span.before.clone()];
            let b = &after[span.after.clone()];
            match span.kind {
                ChangeKind::Equal => assert_eq!(a, b),
                ChangeKind::Insert => assert!(a.is_empty() && !b.is_empty()),
                ChangeKind::Delete => assert!(!a.is_empty() && b.is_empty()),
                ChangeKind::Replace => assert!(!a.is_empty() && !b.is_empty() && a != b),
            }
            old.push_str(a);
            new.push_str(b);
            (old_end, new_end) = (span.before.end, span.after.end);
        }
        assert_eq!(old, before);
        assert_eq!(new, after);
    }

    #[test]
    fn exact_snapshots_survive_unicode_whitespace_reordering_and_markup() {
        let cases = [
            ("", ""),
            ("", "draft"),
            ("draft", ""),
            ("Café e\u{301} 👩🏽‍💻 writes.", "Café e\u{301} 👨🏽‍💻 revises."),
            ("我們寫作。", "我們修改。"),
            ("First.\n\nSecond.\n", "Second.\n\nFirst.\n"),
            ("  line\r\n\tlast \n", " line\n\tlast\n"),
            (
                "**fact** [source](https://a.test)",
                "**fact** [source](https://b.test)",
            ),
            ("Same. Same. Same.", "Same. Different. Same."),
        ];
        for (before, after) in cases {
            for mode in [Granularity::Words, Granularity::Lines] {
                let diff = compare(before, after, mode).unwrap();
                reconstruct(before, after, &diff);
                assert_eq!(diff, compare(before, after, mode).unwrap());
            }
        }
    }

    #[test]
    fn word_view_keeps_a_replacement_and_its_unchanged_context() {
        let before = "The pilot helped 12 people.\n";
        let after = "The pilot helped 21 people.\n";
        let diff = compare(before, after, Granularity::Words).unwrap();
        let changes: Vec<_> = diff
            .spans
            .iter()
            .filter(|s| s.kind != ChangeKind::Equal)
            .collect();
        assert_eq!(changes.len(), 1);
        assert_eq!(&before[changes[0].before.clone()], "12");
        assert_eq!(&after[changes[0].after.clone()], "21");
    }

    #[test]
    fn grapheme_clusters_are_not_split_by_word_highlights() {
        let before = "Go 👩🏽‍💻 now.";
        let after = "Go 👨🏽‍💻 now.";
        let diff = compare(before, after, Granularity::Words).unwrap();
        let change = diff
            .spans
            .iter()
            .find(|s| s.kind != ChangeKind::Equal)
            .unwrap();
        assert_eq!(&before[change.before.clone()], "👩🏽‍💻");
        assert_eq!(&after[change.after.clone()], "👨🏽‍💻");
    }

    #[test]
    fn pathological_alignment_falls_back_without_losing_text() {
        let before = "a b c d ".repeat(5000);
        let after = "e f g h ".repeat(5000);
        let diff = compare(&before, &after, Granularity::Words).unwrap();
        assert!(diff.coarse_blocks > 0);
        reconstruct(&before, &after, &diff);
        assert_eq!(
            compare(&"x".repeat(MAX_INPUT_BYTES + 1), "", Granularity::Words),
            Err(DiffError::InputLimit)
        );
    }

    #[test]
    fn unchanged_book_does_not_spend_quadratic_alignment_work() {
        let before = "A long unchanged paragraph.\n".repeat(20000);
        let after = format!("{before}A final note.\n");
        let diff = compare(&before, &after, Granularity::Words).unwrap();
        assert_eq!(diff.coarse_blocks, 0);
        reconstruct(&before, &after, &diff);
    }
}
