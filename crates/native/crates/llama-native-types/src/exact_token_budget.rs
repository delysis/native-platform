use thiserror::Error;

use crate::{CompletionPrompt, GenerationBatchRequest, GenerationInput};

/// Exact KV-cell accounting for one case in an exact-token generation batch.
///
/// `required_suffix_and_completion_cells` excludes the prefix cells reported
/// by [`ExactTokenBatchCellBudget`]. Cached prefixes occupy independent cells
/// per sequence; an uncached common prefix occupies one shared set of cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactTokenCaseCellBudget {
    input_index: usize,
    prompt_tokens: u64,
    reused_prefix_tokens: u64,
    cached_prefix_tokens: u64,
    unshared_prompt_tokens: u64,
    maximum_sampled_tokens: u64,
    maximum_resident_completion_tokens: u64,
    required_suffix_and_completion_cells: u64,
}

impl ExactTokenCaseCellBudget {
    #[must_use]
    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    #[must_use]
    pub const fn prompt_tokens(&self) -> u64 {
        self.prompt_tokens
    }

    #[must_use]
    pub const fn reused_prefix_tokens(&self) -> u64 {
        self.reused_prefix_tokens
    }

    #[must_use]
    pub const fn cached_prefix_tokens(&self) -> u64 {
        self.cached_prefix_tokens
    }

    #[must_use]
    pub const fn unshared_prompt_tokens(&self) -> u64 {
        self.unshared_prompt_tokens
    }

    #[must_use]
    pub const fn maximum_sampled_tokens(&self) -> u64 {
        self.maximum_sampled_tokens
    }

    /// Maximum generated tokens that can be decoded into this sequence's KV
    /// cells. The final sampled token terminates the loop at the token limit
    /// and is therefore returned without a subsequent decode.
    #[must_use]
    pub const fn maximum_resident_completion_tokens(&self) -> u64 {
        self.maximum_resident_completion_tokens
    }

    #[must_use]
    pub const fn required_suffix_and_completion_cells(&self) -> u64 {
        self.required_suffix_and_completion_cells
    }
}

/// Checked, authoritative KV-cell accounting for an exact-token batch.
///
/// The native engine derives this value before queueing an exact-token request
/// and uses the same prefix allocation during execution. Callers can therefore
/// reserve the returned `required_cells` without copying llama.cpp's batching
/// arithmetic. This is an estimator of cell occupancy, not inference authority,
/// and it deliberately has no deserialization contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactTokenBatchCellBudget {
    shared_uncached_prefix_tokens: u64,
    cached_prefix_tokens: u64,
    cases: Vec<ExactTokenCaseCellBudget>,
    required_cells: u64,
}

impl ExactTokenBatchCellBudget {
    #[must_use]
    pub const fn shared_uncached_prefix_tokens(&self) -> u64 {
        self.shared_uncached_prefix_tokens
    }

    #[must_use]
    pub const fn cached_prefix_tokens(&self) -> u64 {
        self.cached_prefix_tokens
    }

    #[must_use]
    pub fn cases(&self) -> &[ExactTokenCaseCellBudget] {
        &self.cases
    }

    #[must_use]
    pub const fn required_cells(&self) -> u64 {
        self.required_cells
    }

    #[must_use]
    pub const fn fits(&self, available_cells: u64) -> bool {
        self.required_cells <= available_cells
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExactTokenBatchBudgetError {
    #[error("exact-token batch is empty")]
    EmptyBatch,
    #[error("generation case {index} is not exactly one token-ID completion prompt")]
    NonExactTokenCase { index: usize },
    #[error("generation case {index} has an empty exact-token prompt")]
    EmptyPrompt { index: usize },
    #[error("generation case {index} contains a negative token ID")]
    NegativeTokenId { index: usize },
    #[error("generation case {index} has a zero completion-token budget")]
    ZeroCompletionBudget { index: usize },
    #[error("generation case {index} has an invalid cached-prefix witness")]
    InvalidCachedPrefix { index: usize },
    #[error("exact-token batch cell accounting overflowed")]
    ArithmeticOverflow,
}

/// Derive exact KV-cell occupancy for an ordered exact-token batch.
///
/// For uncached cases, a token-exact common prefix is stored once, while at
/// least the final prompt token remains sequence-local so each sequence obtains
/// its own logits. Cached prefixes already occupy separate sequence cells and
/// are therefore summed rather than shared. Every case then contributes its
/// unshared prompt suffix and the maximum number of generated tokens that can
/// subsequently be decoded into KV. At a positive token limit `N`, at most
/// `N - 1` generated tokens enter KV: the `N`th sample is returned and
/// terminates generation before another decode.
pub fn exact_token_batch_cell_budget(
    request: &GenerationBatchRequest,
) -> Result<ExactTokenBatchCellBudget, ExactTokenBatchBudgetError> {
    if request.cases.is_empty() {
        return Err(ExactTokenBatchBudgetError::EmptyBatch);
    }

    let mut prompts = Vec::<&[i32]>::with_capacity(request.cases.len());
    let mut cached_prefix_lengths = Vec::<Option<usize>>::with_capacity(request.cases.len());
    for (index, case) in request.cases.iter().enumerate() {
        let token_ids = match &case.input {
            GenerationInput::Completion { prompts } => match prompts.as_slice() {
                [CompletionPrompt::Tokens { token_ids }] => token_ids.as_slice(),
                _ => return Err(ExactTokenBatchBudgetError::NonExactTokenCase { index }),
            },
            GenerationInput::Chat { .. } | GenerationInput::FillInMiddle { .. } => {
                return Err(ExactTokenBatchBudgetError::NonExactTokenCase { index });
            }
        };
        if token_ids.is_empty() {
            return Err(ExactTokenBatchBudgetError::EmptyPrompt { index });
        }
        if token_ids.iter().any(|token_id| *token_id < 0) {
            return Err(ExactTokenBatchBudgetError::NegativeTokenId { index });
        }
        if case.sampling.max_tokens == 0 {
            return Err(ExactTokenBatchBudgetError::ZeroCompletionBudget { index });
        }
        let cached_prefix_length = case.cached_prefix.as_ref().map(|cached| {
            let valid = cached.token_count > 0
                && cached.token_count == cached.token_ids.len()
                && cached.token_count < token_ids.len()
                && token_ids
                    .iter()
                    .take(cached.token_count)
                    .copied()
                    .eq(cached.token_ids.iter().copied());
            valid
                .then_some(cached.token_count)
                .ok_or(ExactTokenBatchBudgetError::InvalidCachedPrefix { index })
        });
        prompts.push(token_ids);
        cached_prefix_lengths.push(cached_prefix_length.transpose()?);
    }

    let uncached_indices = cached_prefix_lengths
        .iter()
        .enumerate()
        .filter_map(|(index, cached)| cached.is_none().then_some(index))
        .collect::<Vec<_>>();
    let shared_uncached_prefix = if uncached_indices.len() > 1 {
        let first = prompts[uncached_indices[0]];
        let minimum = uncached_indices
            .iter()
            .map(|index| prompts[*index].len())
            .min()
            .ok_or(ExactTokenBatchBudgetError::ArithmeticOverflow)?;
        let common = (0..minimum)
            .take_while(|position| {
                uncached_indices
                    .iter()
                    .skip(1)
                    .all(|index| prompts[*index][*position] == first[*position])
            })
            .count();
        common.min(minimum.saturating_sub(1))
    } else {
        0
    };

    let mut cached_prefix_tokens = 0_u64;
    let mut cases = Vec::with_capacity(request.cases.len());
    let mut required_cells = usize_to_u64(shared_uncached_prefix)?;
    for (index, ((case, prompt), cached_prefix)) in request
        .cases
        .iter()
        .zip(prompts)
        .zip(cached_prefix_lengths)
        .enumerate()
    {
        let prompt_tokens = usize_to_u64(prompt.len())?;
        let cached_prefix_tokens_for_case = usize_to_u64(cached_prefix.unwrap_or(0))?;
        cached_prefix_tokens = checked_add(cached_prefix_tokens, cached_prefix_tokens_for_case)?;
        let reused_prefix_tokens = if cached_prefix.is_some() {
            cached_prefix_tokens_for_case
        } else {
            usize_to_u64(shared_uncached_prefix)?
        };
        let unshared_prompt_tokens = prompt_tokens
            .checked_sub(reused_prefix_tokens)
            .ok_or(ExactTokenBatchBudgetError::ArithmeticOverflow)?;
        let maximum_sampled_tokens = u64::from(case.sampling.max_tokens);
        let maximum_resident_completion_tokens = maximum_sampled_tokens
            .checked_sub(1)
            .ok_or(ExactTokenBatchBudgetError::ArithmeticOverflow)?;
        let required_suffix_and_completion_cells =
            checked_add(unshared_prompt_tokens, maximum_resident_completion_tokens)?;
        required_cells = checked_add(required_cells, required_suffix_and_completion_cells)?;
        cases.push(ExactTokenCaseCellBudget {
            input_index: index,
            prompt_tokens,
            reused_prefix_tokens,
            cached_prefix_tokens: cached_prefix_tokens_for_case,
            unshared_prompt_tokens,
            maximum_sampled_tokens,
            maximum_resident_completion_tokens,
            required_suffix_and_completion_cells,
        });
    }
    required_cells = checked_add(required_cells, cached_prefix_tokens)?;

    Ok(ExactTokenBatchCellBudget {
        shared_uncached_prefix_tokens: usize_to_u64(shared_uncached_prefix)?,
        cached_prefix_tokens,
        cases,
        required_cells,
    })
}

fn usize_to_u64(value: usize) -> Result<u64, ExactTokenBatchBudgetError> {
    u64::try_from(value).map_err(|_| ExactTokenBatchBudgetError::ArithmeticOverflow)
}

fn checked_add(left: u64, right: u64) -> Result<u64, ExactTokenBatchBudgetError> {
    left.checked_add(right)
        .ok_or(ExactTokenBatchBudgetError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GenerationCase, SamplingConfig, SequenceStateBlob};

    fn case(tokens: &[i32], max_tokens: u32) -> GenerationCase {
        GenerationCase {
            case_id: format!("case-{}-{max_tokens}", tokens.len()),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: tokens.to_vec(),
                }],
            },
            sampling: SamplingConfig {
                max_tokens,
                ..SamplingConfig::default()
            },
            cached_prefix: None,
        }
    }

    fn request(cases: Vec<GenerationCase>) -> GenerationBatchRequest {
        GenerationBatchRequest {
            request_id: "budget".to_string(),
            model_id: "model".to_string(),
            cases,
        }
    }

    #[test]
    fn single_case_charges_its_exact_peak_cells() {
        let budget = exact_token_batch_cell_budget(&request(vec![case(&[1, 2, 3], 5)]))
            .expect("single exact case");
        assert_eq!(budget.shared_uncached_prefix_tokens(), 0);
        assert_eq!(budget.cached_prefix_tokens(), 0);
        assert_eq!(budget.required_cells(), 7);
        assert_eq!(budget.cases()[0].unshared_prompt_tokens(), 3);
        assert_eq!(budget.cases()[0].maximum_sampled_tokens(), 5);
        assert_eq!(budget.cases()[0].maximum_resident_completion_tokens(), 4);
        assert_eq!(budget.cases()[0].required_suffix_and_completion_cells(), 7);
    }

    #[test]
    fn one_sample_limit_needs_no_generated_token_kv_cell() {
        let budget = exact_token_batch_cell_budget(&request(vec![case(&[1, 2, 3], 1)]))
            .expect("single exact case");
        assert_eq!(budget.required_cells(), 3);
        assert_eq!(budget.cases()[0].maximum_sampled_tokens(), 1);
        assert_eq!(budget.cases()[0].maximum_resident_completion_tokens(), 0);
    }

    #[test]
    fn identical_cases_share_all_but_the_final_prompt_token() {
        let budget = exact_token_batch_cell_budget(&request(vec![
            case(&[1, 2, 3, 4], 5),
            case(&[1, 2, 3, 4], 7),
        ]))
        .expect("identical exact cases");
        assert_eq!(budget.shared_uncached_prefix_tokens(), 3);
        assert_eq!(budget.required_cells(), 15);
        assert_eq!(budget.cases()[0].unshared_prompt_tokens(), 1);
        assert_eq!(budget.cases()[1].unshared_prompt_tokens(), 1);
    }

    #[test]
    fn shortest_prompt_keeps_one_sequence_local_logit_token() {
        let budget =
            exact_token_batch_cell_budget(&request(vec![case(&[1, 2], 1), case(&[1, 2, 3], 2)]))
                .expect("prefix-related exact cases");
        assert_eq!(budget.shared_uncached_prefix_tokens(), 1);
        assert_eq!(budget.cases()[0].unshared_prompt_tokens(), 1);
        assert_eq!(budget.cases()[1].unshared_prompt_tokens(), 2);
        assert_eq!(budget.required_cells(), 5);
    }

    #[test]
    fn cached_prefixes_are_charged_per_sequence_not_as_shared_cells() {
        let mut cached = case(&[1, 2, 3, 4], 3);
        cached.cached_prefix = Some(SequenceStateBlob {
            sequence_id: 0,
            token_count: 2,
            bytes: vec![1],
            token_ids: vec![1, 2],
        });
        let budget = exact_token_batch_cell_budget(&request(vec![
            cached,
            case(&[9, 8, 7], 2),
            case(&[9, 8, 6], 4),
        ]))
        .expect("mixed cached and shared exact cases");
        assert_eq!(budget.cached_prefix_tokens(), 2);
        assert_eq!(budget.shared_uncached_prefix_tokens(), 2);
        assert_eq!(budget.required_cells(), 14);
        assert_eq!(budget.cases()[0].reused_prefix_tokens(), 2);
        assert_eq!(budget.cases()[0].cached_prefix_tokens(), 2);
    }

    #[test]
    fn invalid_exact_inputs_fail_before_budget_publication() {
        let mut empty = case(&[], 1);
        assert!(matches!(
            exact_token_batch_cell_budget(&request(vec![empty.clone()])),
            Err(ExactTokenBatchBudgetError::EmptyPrompt { index: 0 })
        ));
        empty.input = GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Tokens {
                token_ids: vec![-1],
            }],
        };
        assert!(matches!(
            exact_token_batch_cell_budget(&request(vec![empty])),
            Err(ExactTokenBatchBudgetError::NegativeTokenId { index: 0 })
        ));

        let mut invalid_cache = case(&[1, 2], 1);
        invalid_cache.cached_prefix = Some(SequenceStateBlob {
            sequence_id: 0,
            token_count: 2,
            bytes: vec![1],
            token_ids: vec![1, 2],
        });
        assert!(matches!(
            exact_token_batch_cell_budget(&request(vec![invalid_cache])),
            Err(ExactTokenBatchBudgetError::InvalidCachedPrefix { index: 0 })
        ));
    }

    #[test]
    fn deterministic_small_space_matches_independent_cell_formula() {
        for case_count in 1..=4_usize {
            for sample in 0..256_usize {
                let mut cases = Vec::with_capacity(case_count);
                for index in 0..case_count {
                    let length = 1 + ((sample + index * 3) % 7);
                    let common = (sample / 7) % length;
                    let tokens = (0..length)
                        .map(|position| {
                            if position < common {
                                position as i32
                            } else {
                                (100 + index * 11 + position) as i32
                            }
                        })
                        .collect::<Vec<_>>();
                    cases.push(case(&tokens, 1 + ((sample + index) % 9) as u32));
                }
                let request = request(cases);
                let budget = exact_token_batch_cell_budget(&request).expect("generated request");

                let prompts = request
                    .cases
                    .iter()
                    .map(|case| match &case.input {
                        GenerationInput::Completion { prompts } => match prompts.as_slice() {
                            [CompletionPrompt::Tokens { token_ids }] => token_ids.as_slice(),
                            _ => unreachable!("test creates exact prompts"),
                        },
                        _ => unreachable!("test creates completion prompts"),
                    })
                    .collect::<Vec<_>>();
                let expected_shared = if prompts.len() > 1 {
                    let minimum = prompts.iter().map(|tokens| tokens.len()).min().unwrap_or(0);
                    (0..minimum)
                        .take_while(|position| {
                            prompts
                                .iter()
                                .skip(1)
                                .all(|tokens| tokens[*position] == prompts[0][*position])
                        })
                        .count()
                        .min(minimum.saturating_sub(1))
                } else {
                    0
                };
                let expected = expected_shared as u64
                    + request
                        .cases
                        .iter()
                        .zip(prompts)
                        .map(|(case, prompt)| {
                            (prompt.len() - expected_shared) as u64
                                + u64::from(case.sampling.max_tokens - 1)
                        })
                        .sum::<u64>();
                assert_eq!(
                    budget.shared_uncached_prefix_tokens(),
                    expected_shared as u64
                );
                assert_eq!(budget.required_cells(), expected);
                assert!(budget.fits(expected));
                assert!(!budget.fits(expected.saturating_sub(1)) || expected == 0);
            }
        }
    }
}
