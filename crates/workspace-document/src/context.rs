//! Deterministic, bounded selection of complete context units.
//!
//! The caller resolves immutable content and supplies the tokenizer. One unit
//! may contain several rendered messages (for example attribution + answer),
//! text attachments, citations, and media. Selection never splits that unit or
//! grants authority to resolve another source. Mandatory instructions and the
//! addressed input belong to `Complete`, not to either discardable history.

use std::ops::Range;

use crate::MAX_DOCUMENT_PARTS;

/// Bounds repeated native tokenization even for many zero-token history units.
/// Reaching the bound is an error, not permission to silently omit more input.
pub const MAX_CONTEXT_MEASUREMENTS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextBudget {
    pub source_tokens: usize,
    pub host_tokens: usize,
    pub context_tokens: usize,
    pub output_tokens: usize,
}

/// Index ranges always refer to the caller's original, ordered unit arrays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextMeasure {
    Source(Range<usize>),
    Host(Range<usize>),
    Complete {
        source: Range<usize>,
        host: Range<usize>,
    },
}

/// Selection evidence is not a native execution or tokenizer capability.
/// `measured_prompt_tokens` has exactly the domain of the supplied measurer.
/// A text-only tokenizer must not be described as counting image/audio tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSelection {
    source: Range<usize>,
    host: Range<usize>,
    measured_prompt_tokens: usize,
    measurements: usize,
}

impl ContextSelection {
    pub fn source(&self) -> Range<usize> { self.source.clone() }
    pub fn host(&self) -> Range<usize> { self.host.clone() }
    pub const fn omitted_source_units(&self) -> usize { self.source.start }
    pub const fn omitted_host_units(&self) -> usize { self.host.start }
    pub const fn measured_prompt_tokens(&self) -> usize { self.measured_prompt_tokens }
    pub const fn measurements(&self) -> usize { self.measurements }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextSelectionError<E> {
    InvalidBudget,
    UnitLimit,
    MeasurementLimit,
    Measurement(E),
    MandatoryInputTooLarge,
}

/// Prefer a contiguous recent suffix in each history. Then trim the oldest host
/// units before source units until the *complete framed input* fits. We make no
/// assumption that token counts are additive or monotonic. The greedy suffix
/// rule is deliberate; this is not a claim of globally optimal context packing.
/// Tokenization failures propagate and can never select a different assistant.
pub fn select_context<E>(
    budget: ContextBudget,
    source_units: usize,
    host_units: usize,
    mut measure: impl FnMut(ContextMeasure) -> Result<usize, E>,
) -> Result<ContextSelection, ContextSelectionError<E>> {
    if budget.context_tokens == 0 || budget.output_tokens >= budget.context_tokens {
        return Err(ContextSelectionError::InvalidBudget);
    }
    if source_units.checked_add(host_units)
        .is_none_or(|count| count > MAX_DOCUMENT_PARTS)
    {
        return Err(ContextSelectionError::UnitLimit);
    }
    let mut measurements = 0_usize;
    let mut bounded_measure = |request| {
        if measurements >= MAX_CONTEXT_MEASUREMENTS {
            return Err(ContextSelectionError::MeasurementLimit);
        }
        measurements += 1;
        measure(request).map_err(ContextSelectionError::Measurement)
    };
    let mut source_start = source_units;
    if budget.source_tokens != 0 {
        for start in (0..source_units).rev() {
            let tokens = bounded_measure(ContextMeasure::Source(start..source_units))?;
            if tokens > budget.source_tokens { break; }
            source_start = start;
        }
    }
    let mut host_start = host_units;
    if budget.host_tokens != 0 {
        for start in (0..host_units).rev() {
            let tokens = bounded_measure(ContextMeasure::Host(start..host_units))?;
            if tokens > budget.host_tokens { break; }
            host_start = start;
        }
    }
    loop {
        let tokens = bounded_measure(ContextMeasure::Complete {
            source: source_start..source_units,
            host: host_start..host_units,
        })?;
        if tokens.checked_add(budget.output_tokens)
            .is_some_and(|total| total <= budget.context_tokens)
        {
            return Ok(ContextSelection {
                source: source_start..source_units,
                host: host_start..host_units,
                measured_prompt_tokens: tokens,
                measurements,
            });
        }
        if host_start < host_units {
            host_start += 1;
        } else if source_start < source_units {
            source_start += 1;
        } else {
            return Err(ContextSelectionError::MandatoryInputTooLarge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> ContextBudget {
        ContextBudget { source_tokens: 50, host_tokens: 50, context_tokens: 100, output_tokens: 10 }
    }
    fn cost(request: ContextMeasure) -> Result<usize, &'static str> {
        Ok(match request {
            ContextMeasure::Source(range) | ContextMeasure::Host(range) => range.len() * 10,
            ContextMeasure::Complete { source, host } => 20 + (source.len() + host.len()) * 10,
        })
    }

    #[test]
    fn suffix_selection_retains_order_and_reports_omissions() {
        let selected = select_context(budget(), 8, 7, cost).expect("bounded context");
        assert_eq!(selected.source(), 3..8);
        assert_eq!(selected.host(), 5..7);
        assert_eq!(selected.omitted_source_units(), 3);
        assert_eq!(selected.omitted_host_units(), 5);
        assert_eq!(selected.measured_prompt_tokens(), 90);
    }

    #[test]
    fn zero_history_budgets_never_read_or_measure_disallowed_history() {
        let selected = select_context(ContextBudget { source_tokens: 0, host_tokens: 0, ..budget() },
            8, 7, |request| match request {
                ContextMeasure::Complete { source, host } => {
                    assert!(source.is_empty() && host.is_empty());
                    Ok::<_, &str>(20)
                }
                _ => panic!("disabled history must not be measured"),
            }).expect("mandatory input fits");
        assert_eq!(selected.source(), 8..8);
        assert_eq!(selected.host(), 7..7);
        assert_eq!(selected.measurements(), 1);
    }

    #[test]
    fn complete_measurement_counts_framing_not_a_sum_of_independent_counts() {
        let selected = select_context(budget(), 2, 2, |request| {
            Ok::<_, &str>(match request {
                ContextMeasure::Source(_) | ContextMeasure::Host(_) => 1,
                ContextMeasure::Complete { source, host } => 80 + 6 * (source.len() + host.len()),
            })
        }).expect("trim complete units");
        assert_eq!(selected.host(), 2..2);
        assert_eq!(selected.source(), 1..2);
        assert_eq!(selected.measured_prompt_tokens(), 86);
    }

    #[test]
    fn attribution_and_body_are_selected_as_one_unit_not_separate_messages() {
        let units = [["speaker metadata", "answer"], ["other metadata", "other answer"]];
        let selected = select_context(ContextBudget { source_tokens: 10, host_tokens: 0, ..budget() },
            units.len(), 0, |request| {
                Ok::<_, &str>(match request {
                    ContextMeasure::Source(range) => units[range].len() * 10,
                    ContextMeasure::Complete { source, .. } => units[source].len() * 10 + 20,
                    _ => panic!("no host units"),
                })
            }).expect("one complete source unit");
        assert_eq!(&units[selected.source()], &[["other metadata", "other answer"]]);
    }

    #[test]
    fn tokenization_errors_are_not_treated_as_empty_or_overlarge_history() {
        assert_eq!(select_context(budget(), 1, 0, |_| Err("tokenizer unavailable")),
            Err(ContextSelectionError::Measurement("tokenizer unavailable")));
    }

    #[test]
    fn mandatory_input_is_never_shortened() {
        assert_eq!(select_context(budget(), 0, 0, |_| Ok::<_, &str>(91)),
            Err(ContextSelectionError::MandatoryInputTooLarge));
        assert!(select_context(budget(), 0, 0, |_| Ok::<_, &str>(90)).is_ok());
    }

    #[test]
    fn overflowing_counts_fail_closed() {
        assert_eq!(select_context(budget(), 0, 0, |_| Ok::<_, &str>(usize::MAX)),
            Err(ContextSelectionError::MandatoryInputTooLarge));
        assert_eq!(select_context(budget(), usize::MAX, 1, cost),
            Err(ContextSelectionError::UnitLimit));
    }

    #[test]
    fn invalid_budgets_have_no_measurement_side_effect() {
        for invalid in [
            ContextBudget { context_tokens: 0, ..budget() },
            ContextBudget { output_tokens: 100, ..budget() },
            ContextBudget { output_tokens: usize::MAX, ..budget() },
        ] {
            assert_eq!(select_context(invalid, 0, 0,
                |_| -> Result<usize, &str> { panic!("invalid budget") }),
                Err(ContextSelectionError::InvalidBudget));
        }
    }

    #[test]
    fn zero_token_inputs_cannot_force_unbounded_measurement_work() {
        let mut calls = 0;
        assert_eq!(select_context(budget(), MAX_CONTEXT_MEASUREMENTS + 1, 0, |_| {
            calls += 1;
            Ok::<_, &str>(0)
        }), Err(ContextSelectionError::MeasurementLimit));
        assert_eq!(calls, MAX_CONTEXT_MEASUREMENTS);
    }
}
