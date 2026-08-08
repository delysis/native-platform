use std::fmt;
use std::ops::Range;

use loom_types::{ByteRange, DocumentKind};
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use thiserror::Error;

/// Default deterministic resource limits for a visible-text three-way merge.
pub const DEFAULT_MERGE_BUDGET: MergeBudget = MergeBudget {
    max_input_bytes: 16 * 1024 * 1024,
    max_changed_scalars: 64 * 1024,
    max_diff_work: 64 * 1024 * 1024,
    max_changes: 16 * 1024,
    max_output_bytes: 48 * 1024 * 1024,
};

/// Deterministic bounds checked before or immediately after each merge stage.
///
/// `max_diff_work` bounds the product of Unicode scalar counts in the changed
/// middle windows. Shared UTF-8 prefixes and suffixes do not consume that
/// budget. This intentionally rejects a pathological merge instead of making
/// the editor wait an unbounded amount of time.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MergeBudget {
    pub max_input_bytes: u64,
    pub max_changed_scalars: u64,
    pub max_diff_work: u64,
    pub max_changes: u64,
    pub max_output_bytes: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeBudgetMetric {
    InputBytes,
    ChangedScalars,
    DiffWork,
    ConflictComparisons,
    Changes,
    OutputBytes,
}

impl fmt::Display for MergeBudgetMetric {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InputBytes => "input bytes",
            Self::ChangedScalars => "changed Unicode scalars",
            Self::DiffWork => "diff work",
            Self::ConflictComparisons => "conflict comparisons",
            Self::Changes => "change count",
            Self::OutputBytes => "output bytes",
        })
    }
}

/// The result of a three-way merge. Conflicts never contain a partially
/// promoted merge; callers must hold the draft until the author resolves them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MergeOutcome {
    Merged { content: String },
    Conflict { conflicts: Vec<MergeConflict> },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeConflictKind {
    CompetingInsertions,
    OverlappingEdits,
}

/// A byte range and its exact UTF-8 text in one of the three inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MergeConflictSpan {
    pub range: ByteRange,
    pub text: String,
}

/// One pair of incompatible changes.
///
/// `base` is the union of the two base ranges. `app_base_range` and
/// `external_base_range` retain the exact base range claimed by each side.
/// `app` and `external` use byte ranges in their respective complete inputs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MergeConflict {
    pub kind: MergeConflictKind,
    pub base: MergeConflictSpan,
    pub app_base_range: ByteRange,
    pub app: MergeConflictSpan,
    pub external_base_range: ByteRange,
    pub external: MergeConflictSpan,
}

#[derive(Clone, Debug, Error, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeError {
    #[error("hybrid documents require block metadata before they can be merged")]
    HybridMetadataRequired,
    #[error("merge {metric} budget exceeded: {actual} exceeds {limit}")]
    BudgetExceeded {
        metric: MergeBudgetMetric,
        actual: u64,
        limit: u64,
    },
    #[error("merge byte range cannot be represented as a 64-bit value")]
    RangeTooLarge,
    #[error("the diff engine emitted an invalid or overlapping edit script")]
    InvalidEditScript,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Change {
    base: Range<usize>,
    side: Range<usize>,
    replacement: String,
}

/// Merges two visible UTF-8 document states derived from a common base using
/// [`DEFAULT_MERGE_BUDGET`].
pub fn three_way_merge(
    kind: DocumentKind,
    base: &str,
    app_draft: &str,
    external_visible: &str,
) -> Result<MergeOutcome, MergeError> {
    three_way_merge_with_budget(
        kind,
        base,
        app_draft,
        external_visible,
        DEFAULT_MERGE_BUDGET,
    )
}

/// Merges two visible UTF-8 document states derived from a common base with
/// explicit deterministic resource limits.
///
/// The function operates on Unicode scalar boundaries and reports all public
/// ranges as UTF-8 byte offsets. It does not normalize any input. In
/// particular, verse whitespace and CRLF bytes are preserved exactly. Prose
/// canonicalization, when desired, belongs at the document import boundary.
/// Hybrid documents are rejected because visible text alone cannot preserve
/// their prose/verse block semantics.
pub fn three_way_merge_with_budget(
    kind: DocumentKind,
    base: &str,
    app_draft: &str,
    external_visible: &str,
    budget: MergeBudget,
) -> Result<MergeOutcome, MergeError> {
    if kind == DocumentKind::Hybrid {
        return Err(MergeError::HybridMetadataRequired);
    }

    ensure_input_budget(base, app_draft, external_visible, budget)?;

    if app_draft == external_visible {
        ensure_output_budget(app_draft.len(), budget)?;
        return Ok(MergeOutcome::Merged {
            content: app_draft.to_owned(),
        });
    }
    if app_draft == base {
        ensure_output_budget(external_visible.len(), budget)?;
        return Ok(MergeOutcome::Merged {
            content: external_visible.to_owned(),
        });
    }
    if external_visible == base {
        ensure_output_budget(app_draft.len(), budget)?;
        return Ok(MergeOutcome::Merged {
            content: app_draft.to_owned(),
        });
    }

    let app_changes = diff_changes(base, app_draft, budget)?;
    let external_changes = diff_changes(base, external_visible, budget)?;
    let conflicts = find_conflicts(
        base,
        app_draft,
        external_visible,
        &app_changes,
        &external_changes,
        budget,
    )?;
    if !conflicts.is_empty() {
        return Ok(MergeOutcome::Conflict { conflicts });
    }

    let mut changes = app_changes;
    for external_change in external_changes {
        if !changes
            .iter()
            .any(|app_change| equivalent_change(app_change, &external_change))
        {
            changes.push(external_change);
        }
    }
    changes.sort_by(|left, right| {
        left.base
            .start
            .cmp(&right.base.start)
            .then_with(|| left.base.end.cmp(&right.base.end))
            .then_with(|| left.replacement.cmp(&right.replacement))
    });

    apply_changes(base, &changes, budget)
}

fn ensure_input_budget(
    base: &str,
    app_draft: &str,
    external_visible: &str,
    budget: MergeBudget,
) -> Result<(), MergeError> {
    let actual = [base.len(), app_draft.len(), external_visible.len()]
        .into_iter()
        .max()
        .unwrap_or(0);
    ensure_budget(
        MergeBudgetMetric::InputBytes,
        usize_to_u64(actual)?,
        budget.max_input_bytes,
    )
}

fn ensure_output_budget(actual: usize, budget: MergeBudget) -> Result<(), MergeError> {
    ensure_budget(
        MergeBudgetMetric::OutputBytes,
        usize_to_u64(actual)?,
        budget.max_output_bytes,
    )
}

fn ensure_budget(metric: MergeBudgetMetric, actual: u64, limit: u64) -> Result<(), MergeError> {
    if actual > limit {
        Err(MergeError::BudgetExceeded {
            metric,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn diff_changes(base: &str, side: &str, budget: MergeBudget) -> Result<Vec<Change>, MergeError> {
    if base == side {
        return Ok(Vec::new());
    }

    let (prefix_bytes, suffix_bytes) = common_utf8_edges(base, side);
    let base_middle_end = base
        .len()
        .checked_sub(suffix_bytes)
        .ok_or(MergeError::InvalidEditScript)?;
    let side_middle_end = side
        .len()
        .checked_sub(suffix_bytes)
        .ok_or(MergeError::InvalidEditScript)?;
    let base_middle = base
        .get(prefix_bytes..base_middle_end)
        .ok_or(MergeError::InvalidEditScript)?;
    let side_middle = side
        .get(prefix_bytes..side_middle_end)
        .ok_or(MergeError::InvalidEditScript)?;
    let base_scalar_count = usize_to_u64(base_middle.chars().count())?;
    let side_scalar_count = usize_to_u64(side_middle.chars().count())?;
    let changed_scalars = base_scalar_count.saturating_add(side_scalar_count);
    ensure_budget(
        MergeBudgetMetric::ChangedScalars,
        changed_scalars,
        budget.max_changed_scalars,
    )?;
    ensure_budget(
        MergeBudgetMetric::DiffWork,
        base_scalar_count.saturating_mul(side_scalar_count),
        budget.max_diff_work,
    )?;

    let base_characters: Vec<char> = base_middle.chars().collect();
    let side_characters: Vec<char> = side_middle.chars().collect();
    let base_offsets = utf8_offsets(base_middle);
    let side_offsets = utf8_offsets(side_middle);
    let operations = capture_diff_slices(Algorithm::Myers, &base_characters, &side_characters);
    let mut changes: Vec<Change> = Vec::new();

    for operation in operations {
        if operation.tag() == DiffTag::Equal {
            continue;
        }
        let old = operation.old_range();
        let new = operation.new_range();
        let base_range = offset_range(prefix_bytes, &base_offsets, old)?;
        let side_range = offset_range(prefix_bytes, &side_offsets, new)?;

        if let Some(previous) = changes.last_mut()
            && previous.base.end == base_range.start
            && previous.side.end == side_range.start
        {
            previous.base.end = base_range.end;
            previous.side.end = side_range.end;
            previous
                .replacement
                .push_str(side.get(side_range).ok_or(MergeError::InvalidEditScript)?);
        } else {
            changes.push(Change {
                base: base_range,
                side: side_range.clone(),
                replacement: side
                    .get(side_range)
                    .ok_or(MergeError::InvalidEditScript)?
                    .to_owned(),
            });
        }
    }

    ensure_budget(
        MergeBudgetMetric::Changes,
        usize_to_u64(changes.len())?,
        budget.max_changes,
    )?;
    Ok(changes)
}

fn common_utf8_edges(left: &str, right: &str) -> (usize, usize) {
    let prefix_bytes = left
        .chars()
        .zip(right.chars())
        .take_while(|(left_character, right_character)| left_character == right_character)
        .map(|(character, _)| character.len_utf8())
        .sum();
    let left_remaining = &left[prefix_bytes..];
    let right_remaining = &right[prefix_bytes..];
    let suffix_bytes = left_remaining
        .chars()
        .rev()
        .zip(right_remaining.chars().rev())
        .take_while(|(left_character, right_character)| left_character == right_character)
        .map(|(character, _)| character.len_utf8())
        .sum();
    (prefix_bytes, suffix_bytes)
}

fn utf8_offsets(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .collect()
}

fn offset_range(
    prefix_bytes: usize,
    offsets: &[usize],
    scalar_range: Range<usize>,
) -> Result<Range<usize>, MergeError> {
    let start = offsets
        .get(scalar_range.start)
        .and_then(|offset| prefix_bytes.checked_add(*offset))
        .ok_or(MergeError::InvalidEditScript)?;
    let end = offsets
        .get(scalar_range.end)
        .and_then(|offset| prefix_bytes.checked_add(*offset))
        .ok_or(MergeError::InvalidEditScript)?;
    Ok(start..end)
}

fn find_conflicts(
    base: &str,
    app_draft: &str,
    external_visible: &str,
    app_changes: &[Change],
    external_changes: &[Change],
    budget: MergeBudget,
) -> Result<Vec<MergeConflict>, MergeError> {
    let comparison_work =
        usize_to_u64(app_changes.len())?.saturating_mul(usize_to_u64(external_changes.len())?);
    ensure_budget(
        MergeBudgetMetric::ConflictComparisons,
        comparison_work,
        budget.max_diff_work,
    )?;

    let mut conflicts = Vec::new();
    let mut conflict_payload_bytes = 0_u64;
    for app_change in app_changes {
        for external_change in external_changes {
            if !changes_overlap(app_change, external_change)
                || equivalent_change(app_change, external_change)
            {
                continue;
            }
            let base_range = app_change.base.start.min(external_change.base.start)
                ..app_change.base.end.max(external_change.base.end);
            let next_conflict_count = usize_to_u64(conflicts.len())?.saturating_add(1);
            ensure_budget(
                MergeBudgetMetric::Changes,
                next_conflict_count,
                budget.max_changes,
            )?;
            conflict_payload_bytes = conflict_payload_bytes
                .saturating_add(usize_to_u64(base_range.len())?)
                .saturating_add(usize_to_u64(app_change.side.len())?)
                .saturating_add(usize_to_u64(external_change.side.len())?);
            ensure_budget(
                MergeBudgetMetric::OutputBytes,
                conflict_payload_bytes,
                budget.max_output_bytes,
            )?;
            conflicts.push(MergeConflict {
                kind: if app_change.base.is_empty() && external_change.base.is_empty() {
                    MergeConflictKind::CompetingInsertions
                } else {
                    MergeConflictKind::OverlappingEdits
                },
                base: conflict_span(base, base_range)?,
                app_base_range: public_range(app_change.base.clone())?,
                app: conflict_span(app_draft, app_change.side.clone())?,
                external_base_range: public_range(external_change.base.clone())?,
                external: conflict_span(external_visible, external_change.side.clone())?,
            });
        }
    }
    Ok(conflicts)
}

fn changes_overlap(left: &Change, right: &Change) -> bool {
    match (left.base.is_empty(), right.base.is_empty()) {
        (true, true) => left.base.start == right.base.start,
        (true, false) => left.base.start > right.base.start && left.base.start < right.base.end,
        (false, true) => right.base.start > left.base.start && right.base.start < left.base.end,
        (false, false) => left.base.start < right.base.end && right.base.start < left.base.end,
    }
}

fn equivalent_change(left: &Change, right: &Change) -> bool {
    left.base == right.base && left.replacement == right.replacement
}

fn conflict_span(text: &str, range: Range<usize>) -> Result<MergeConflictSpan, MergeError> {
    Ok(MergeConflictSpan {
        range: public_range(range.clone())?,
        text: text
            .get(range)
            .ok_or(MergeError::InvalidEditScript)?
            .to_owned(),
    })
}

fn public_range(range: Range<usize>) -> Result<ByteRange, MergeError> {
    Ok(ByteRange {
        start: usize_to_u64(range.start)?,
        end: usize_to_u64(range.end)?,
    })
}

fn usize_to_u64(value: usize) -> Result<u64, MergeError> {
    u64::try_from(value).map_err(|_| MergeError::RangeTooLarge)
}

fn apply_changes(
    base: &str,
    changes: &[Change],
    budget: MergeBudget,
) -> Result<MergeOutcome, MergeError> {
    let mut output_length = base.len();
    let mut cursor = 0;
    for change in changes {
        if change.base.start < cursor || change.base.end > base.len() {
            return Err(MergeError::InvalidEditScript);
        }
        output_length = output_length
            .checked_sub(change.base.len())
            .and_then(|length| length.checked_add(change.replacement.len()))
            .ok_or(MergeError::RangeTooLarge)?;
        cursor = change.base.end;
    }
    ensure_output_budget(output_length, budget)?;

    let mut content = String::with_capacity(output_length);
    cursor = 0;
    for change in changes {
        content.push_str(
            base.get(cursor..change.base.start)
                .ok_or(MergeError::InvalidEditScript)?,
        );
        content.push_str(&change.replacement);
        cursor = change.base.end;
    }
    content.push_str(base.get(cursor..).ok_or(MergeError::InvalidEditScript)?);
    if content.len() != output_length {
        return Err(MergeError::InvalidEditScript);
    }
    Ok(MergeOutcome::Merged { content })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(outcome: MergeOutcome) -> String {
        match outcome {
            MergeOutcome::Merged { content } => content,
            MergeOutcome::Conflict { conflicts } => {
                panic!("expected merge, got conflicts: {conflicts:?}")
            }
        }
    }

    #[test]
    fn composes_disjoint_edits() {
        let base = "The moon rose.\nBirds slept.\n";
        let app = "The sun rose.\nBirds slept.\n";
        let external = "The moon rose.\nOwls slept.\n";

        let outcome = three_way_merge(DocumentKind::Prose, base, app, external)
            .expect("merge disjoint changes");

        assert_eq!(merged(outcome), "The sun rose.\nOwls slept.\n");
    }

    #[test]
    fn identical_edits_are_applied_once() {
        let base = "old\n";
        let edited = "new\n";
        let outcome = three_way_merge_with_budget(
            DocumentKind::Prose,
            base,
            edited,
            edited,
            DEFAULT_MERGE_BUDGET,
        )
        .expect("merge identical changes");
        assert_eq!(merged(outcome), edited);
    }

    #[test]
    fn reports_overlapping_edits_with_all_three_spans() {
        let outcome = three_way_merge_with_budget(
            DocumentKind::Prose,
            "cat",
            "dog",
            "fox",
            DEFAULT_MERGE_BUDGET,
        )
        .expect("construct conflict");
        let MergeOutcome::Conflict { conflicts } = outcome else {
            panic!("overlapping edits must conflict");
        };
        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.kind, MergeConflictKind::OverlappingEdits);
        assert_eq!(conflict.base.range, ByteRange { start: 0, end: 3 });
        assert_eq!(conflict.base.text, "cat");
        assert_eq!(conflict.app.text, "dog");
        assert_eq!(conflict.external.text, "fox");
    }

    #[test]
    fn unicode_changes_merge_only_at_scalar_boundaries() {
        let base = "a🍎b café";
        let app = "a🍐b café";
        let external = "a🍎b cafè";
        let outcome = three_way_merge_with_budget(
            DocumentKind::Prose,
            base,
            app,
            external,
            DEFAULT_MERGE_BUDGET,
        )
        .expect("merge Unicode changes");
        let content = merged(outcome);
        assert_eq!(content, "a🍐b cafè");
        assert!(content.is_char_boundary(1));
        assert!(content.is_char_boundary("a🍐".len()));
    }

    #[test]
    fn unicode_conflict_ranges_never_split_scalars() {
        let base = "a🍎b";
        let app = "a🍐b";
        let external = "a🍊b";
        let outcome = three_way_merge(DocumentKind::Prose, base, app, external)
            .expect("construct Unicode conflict");
        let MergeOutcome::Conflict { conflicts } = outcome else {
            panic!("competing scalar replacements must conflict");
        };
        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.base.text, "🍎");
        assert_eq!(conflict.app.text, "🍐");
        assert_eq!(conflict.external.text, "🍊");
        for (text, span) in [
            (base, &conflict.base),
            (app, &conflict.app),
            (external, &conflict.external),
        ] {
            let start = usize::try_from(span.range.start).expect("range fits usize");
            let end = usize::try_from(span.range.end).expect("range fits usize");
            assert!(text.is_char_boundary(start));
            assert!(text.is_char_boundary(end));
        }
    }

    #[test]
    fn verse_merge_preserves_crlf_and_whitespace_exactly() {
        let base = "  one\r\n\r\ntwo  \r\n";
        let app = "  ONE\r\n\r\ntwo  \r\n";
        let external = "  one\r\n\r\ntwo!  \r\n";
        let outcome = three_way_merge_with_budget(
            DocumentKind::Verse,
            base,
            app,
            external,
            DEFAULT_MERGE_BUDGET,
        )
        .expect("merge verse changes");
        assert_eq!(merged(outcome).as_bytes(), b"  ONE\r\n\r\ntwo!  \r\n");
    }

    #[test]
    fn handles_empty_documents_and_conflicting_initial_insertions() {
        let one_sided = three_way_merge_with_budget(
            DocumentKind::Verse,
            "",
            "line\r\n",
            "",
            DEFAULT_MERGE_BUDGET,
        )
        .expect("merge one-sided insertion");
        assert_eq!(merged(one_sided), "line\r\n");

        let both_deleted = three_way_merge_with_budget(
            DocumentKind::Prose,
            "erase me",
            "",
            "",
            DEFAULT_MERGE_BUDGET,
        )
        .expect("merge identical deletions");
        assert_eq!(merged(both_deleted), "");

        let competing = three_way_merge_with_budget(
            DocumentKind::Verse,
            "",
            "app",
            "external",
            DEFAULT_MERGE_BUDGET,
        )
        .expect("report competing insertions");
        let MergeOutcome::Conflict { conflicts } = competing else {
            panic!("competing initial insertions must conflict");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, MergeConflictKind::CompetingInsertions);
        assert_eq!(conflicts[0].base.range, ByteRange { start: 0, end: 0 });
        assert_eq!(conflicts[0].app.text, "app");
        assert_eq!(conflicts[0].external.text, "external");
    }

    #[test]
    fn rejects_diff_windows_above_the_default_work_budget() {
        let base = "a".repeat(8_193);
        let app = "b".repeat(8_193);
        let external = "c".repeat(8_193);
        let error = three_way_merge_with_budget(
            DocumentKind::Prose,
            &base,
            &app,
            &external,
            DEFAULT_MERGE_BUDGET,
        )
        .expect_err("pathological diff should be held");
        assert!(matches!(
            error,
            MergeError::BudgetExceeded {
                metric: MergeBudgetMetric::DiffWork,
                ..
            }
        ));
    }

    #[test]
    fn hybrid_visible_text_is_held_without_block_metadata() {
        let error = three_way_merge_with_budget(
            DocumentKind::Hybrid,
            "base",
            "app",
            "external",
            DEFAULT_MERGE_BUDGET,
        )
        .expect_err("hybrid merge must require metadata");
        assert_eq!(error, MergeError::HybridMetadataRequired);
    }
}
