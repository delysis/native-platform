//! Owner-thread execution of opt-in first-word selection. The ordinary batch
//! loop remains independent; text and media share this admission implementation.
use super::first_word_choices::{Admission, FirstWordGate};
use super::*;
use llama_cpp_2::token::data::LlamaTokenData;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use llama_native_types::FIRST_WORD_CHOICE_MAX_EOG_TOKENS;
use std::collections::BTreeSet;

fn initial_candidates(logits: &[f32], exclusions: &BTreeSet<i32>) -> Option<LlamaTokenDataArray> {
    if !logits
        .iter()
        .enumerate()
        .any(|(index, logit)| logit.is_finite() && !exclusions.contains(&(index as i32)))
    {
        return None;
    }
    Some(LlamaTokenDataArray::from_iter(
        logits.iter().enumerate().map(|(index, logit)| {
            let token_id = index as i32;
            let masked = if exclusions.contains(&token_id) {
                f32::NEG_INFINITY
            } else {
                *logit
            };
            LlamaTokenData::new(LlamaToken::new(token_id), masked, 0.0)
        }),
        false,
    ))
}

fn sample_saved_logits(
    sampler: &mut LlamaSampler,
    logits: &[f32],
    exclusions: &BTreeSet<i32>,
) -> NativeResult<Option<LlamaToken>> {
    let Some(mut candidates) = initial_candidates(logits, exclusions) else {
        return Ok(None);
    };
    sampler.apply(&mut candidates);
    let token = candidates.selected_token().ok_or_else(|| {
        NativeError::new(
            NativeErrorCode::DecodeFailed,
            "word-choice sampler selected no token",
        )
    })?;
    if exclusions.contains(&token.0)
        || !usize::try_from(token.0)
            .ok()
            .and_then(|index| logits.get(index))
            .is_some_and(|logit| logit.is_finite())
    {
        return Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "word-choice sampler selected an excluded or non-finite initial token",
        ));
    }
    // sample() normally owns accept. Here we use apply() against an immutable
    // snapshot, so exactly one explicit accept restores those same semantics.
    sampler.accept(token);
    Ok(Some(token))
}

fn discard_proposal(branch: &mut ActiveBranch<'_>) {
    branch.decoder = UTF_8.new_decoder();
    branch.decoder_finalized = false;
    branch.text.clear();
    branch.generated_token_ids.clear();
    branch.generated = 0;
    branch.terminal_sampled_token_id = None;
    branch.first_token_ms = None;
    branch.forced_tokens.clear();
}

fn reset_proposal(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    branch: &mut ActiveBranch<'_>,
    prompt_position: i32,
    prompt_tokens: &[i32],
    seed: u32,
    tracking: &mut SequenceTracking<'_>,
) -> NativeResult<()> {
    let removed = context
        .clear_kv_cache_seq(
            Some(branch.sequence_id as u32),
            Some(prompt_position as u32),
            None,
        )
        .map_err(|error| {
            native_decode_error("failed to discard rejected word proposal KV", error)
        })?;
    require_kv_removal(removed)?;
    discard_proposal(branch);
    branch.next_position = prompt_position;
    branch.state = GenerationState::Generating;
    branch.finish_reason.clear();
    let mut sampling = branch.request.sampling.clone();
    sampling.seed = seed;
    branch.sampler = build_sampler(model, &sampling);
    branch
        .sampler
        .accept_many(prompt_tokens.iter().copied().map(LlamaToken::new));
    tracking
        .token_counts
        .insert(branch.sequence_id, prompt_position as usize);
    tracking
        .token_ids
        .insert(branch.sequence_id, prompt_tokens.to_vec());
    Ok(())
}

fn require_kv_removal(removed: bool) -> NativeResult<()> {
    if removed {
        Ok(())
    } else {
        Err(NativeError::new(
            NativeErrorCode::DecodeFailed,
            "native memory refused word-choice sequence removal; retry state cannot be restored",
        ))
    }
}

fn emit_piece(
    request: &SharedPrefixBatchRequest,
    branch: &mut ActiveBranch<'_>,
    text: String,
    supervision: &mut BatchSupervision<'_>,
    started: Instant,
) {
    if text.is_empty() {
        return;
    }
    branch
        .first_token_ms
        .get_or_insert_with(|| started.elapsed().as_millis());
    supervision.emit(GenerationEvent {
        request_id: request.request_id.clone(),
        branch_id: branch.request.branch_id.clone(),
        sequence_id: branch.sequence_id,
        input_index: branch.sequence_id as usize,
        event_index: branch.event_index,
        event: GenerationEventKind::Delta { text },
    });
    branch.event_index += 1;
}

pub(super) struct WordChoiceExecution<'a, 'b> {
    pub request: &'a SharedPrefixBatchRequest,
    pub branches: &'a mut [ActiveBranch<'b>],
    pub policy: FirstWordChoicePolicy,
    pub started: Instant,
}

pub(super) fn generate(
    model: &LlamaModel,
    context: &mut LlamaContext<'_>,
    execution: WordChoiceExecution<'_, '_>,
    supervision: &mut BatchSupervision<'_>,
    tracking: &mut SequenceTracking<'_>,
) -> NativeResult<()> {
    let WordChoiceExecution {
        request,
        branches,
        policy,
        started,
    } = execution;
    // Capture every prompt row before any tail decode invalidates the context's
    // logits. Media cases all reference the same final prompt row (-1).
    let prompt_logits = branches
        .iter()
        .map(|branch| {
            if branch.logit_index == -1 {
                // mtmd eval_chunks(logits_last=true) produces one compact row
                // through native eval, bypassing the binding's initialized-row
                // bookkeeping. get_logits_ith(-1) would panic in that wrapper.
                context.get_logits().to_vec()
            } else {
                context.get_logits_ith(branch.logit_index).to_vec()
            }
        })
        .collect::<Vec<_>>();
    let prompt_positions = branches
        .iter()
        .map(|branch| branch.next_position)
        .collect::<Vec<_>>();
    let prompt_tokens = branches
        .iter()
        .map(|branch| {
            tracking
                .token_ids
                .get(&branch.sequence_id)
                .cloned()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let mut gates = branches
        .iter()
        .map(|branch| FirstWordGate::new(policy, branch.request.sampling.seed))
        .collect::<Vec<_>>();
    let mut reserved = BTreeSet::new();
    // Nonempty choices cannot begin with EOG. This explicit static mask avoids
    // zero-token retries without pretending that a previous EOG owns a word.
    let mut initial_exclusions = BTreeSet::new();
    for token_id in 0..model.n_vocab() {
        if model.is_eog_token(LlamaToken::new(token_id)) {
            initial_exclusions.insert(token_id);
            if initial_exclusions.len() > FIRST_WORD_CHOICE_MAX_EOG_TOKENS {
                return Err(NativeError::new(
                    NativeErrorCode::UnsupportedParameter,
                    "model EOG set exceeds the bounded first-word policy mask",
                ));
            }
        }
    }
    loop {
        // Request order decides duplicate ownership, never wall-clock timing or
        // word length. Admitted branches continue streaming while later slots retry.
        let admission_turn = branches.iter().zip(&gates).position(|(branch, gate)| {
            branch.state == GenerationState::Generating && gate.pending()
        });
        let mut next_tokens = Vec::new();
        for (index, branch) in branches.iter_mut().enumerate() {
            if branch.state != GenerationState::Generating {
                continue;
            }
            let gate = &mut gates[index];
            if supervision.cancellations[index].load(Ordering::Acquire) {
                if gate.pending() {
                    gate.cancel(&branch.generated_token_ids);
                    discard_proposal(branch);
                }
                branch.state = GenerationState::Cancelled;
                branch.finish_reason = "cancelled".to_string();
                let removed = context
                    .clear_kv_cache_seq(Some(index as u32), None, None)
                    .map_err(|error| {
                        native_decode_error("failed to cancel word-choice sequence", error)
                    })?;
                require_kv_removal(removed)?;
                continue;
            }
            if gate.pending() && admission_turn != Some(index) {
                continue;
            }
            if supervision.reasoning_forces[index].load(Ordering::Acquire) {
                return Err(NativeError::new(
                    NativeErrorCode::UnsupportedParameter,
                    "forced reasoning termination is unavailable for distinct first-word sampling",
                ));
            }
            let token = if gate.pending() && branch.generated == 0 {
                gate.begin_attempt(&initial_exclusions);
                let Some(token) = sample_saved_logits(
                    &mut branch.sampler,
                    &prompt_logits[index],
                    &initial_exclusions,
                )?
                else {
                    gate.exhaust_initial_support();
                    branch.state = GenerationState::Completed;
                    branch.finish_reason = "first_word_choices_exhausted".to_string();
                    continue;
                };
                // Admission is serial by slot, so reserving at the actual draw
                // is equivalent to reserving after rejection/admission, and also
                // truthfully covers a subsequently cancelled proposal.
                initial_exclusions.insert(token.0);
                token
            } else {
                branch.sampler.sample(context, branch.logit_index)
            };
            let terminal = model.is_eog_token(token);
            let was_pending = gate.pending();
            if terminal {
                branch.state = GenerationState::Completed;
                branch.finish_reason = "end_of_generation".to_string();
                branch.terminal_sampled_token_id = Some(token.0);
                if was_pending {
                    finalize_generated_text_once(
                        &mut branch.decoder,
                        &mut branch.text,
                        false,
                        &mut branch.decoder_finalized,
                    )?;
                }
            } else {
                gate.record_nonterminal_token();
                branch.generated_token_ids.push(token.0);
                let bytes = generated_token_piece(model, token).map_err(|error| {
                    NativeError::new(
                        NativeErrorCode::DecodeFailed,
                        format!("failed to decode word-choice token: {error}"),
                    )
                })?;
                let piece = decode_generated_utf8_piece(&mut branch.decoder, &bytes, false)?;
                append_generated_utf8_piece(&mut branch.text, &piece)?;
                branch.generated += 1;
                // Preserve existing stop projection for admitted tails. Initial
                // bytes remain withheld until both stop handling and word admission.
                if !was_pending {
                    emit_piece(request, branch, piece.clone(), supervision, started);
                }
                if apply_stop_sequences(&mut branch.text, &branch.request.sampling.stop) {
                    branch.state = GenerationState::Completed;
                    branch.finish_reason = "stop_sequence".to_string();
                } else if branch.generated >= branch.request.sampling.max_tokens as usize {
                    branch.state = GenerationState::Completed;
                    branch.finish_reason = "max_tokens".to_string();
                }
            }
            if was_pending {
                match gate.observe(
                    &branch.text,
                    &branch.generated_token_ids,
                    terminal.then_some(token.0),
                    branch.state != GenerationState::Generating,
                    &mut reserved,
                ) {
                    Admission::Pending => {}
                    Admission::Accepted => {
                        let admitted = branch.text.clone();
                        emit_piece(request, branch, admitted, supervision, started);
                    }
                    Admission::Retry => {
                        reset_proposal(
                            model,
                            context,
                            branch,
                            prompt_positions[index],
                            &prompt_tokens[index],
                            gate.seed(),
                            tracking,
                        )?;
                        continue;
                    }
                    Admission::Exhausted => {
                        // No proposal has ever been visible. Clear all output
                        // authority while retaining its exact bounded attempt ledger.
                        reset_proposal(
                            model,
                            context,
                            branch,
                            prompt_positions[index],
                            &prompt_tokens[index],
                            gate.seed(),
                            tracking,
                        )?;
                        branch.state = GenerationState::Completed;
                        branch.finish_reason = "first_word_choices_exhausted".to_string();
                        continue;
                    }
                }
            }
            if branch.state == GenerationState::Generating {
                next_tokens.push((index, token));
            }
        }
        if next_tokens.is_empty() {
            if branches
                .iter()
                .any(|branch| branch.state == GenerationState::Generating)
            {
                continue;
            }
            break;
        }
        let mut batch = LlamaBatch::new(next_tokens.len(), 1);
        for (logit_index, (index, token)) in next_tokens.iter().enumerate() {
            let branch = &mut branches[*index];
            batch
                .add(*token, branch.next_position, &[branch.sequence_id], true)
                .map_err(|error| {
                    native_decode_error("failed to build word-choice decode batch", error)
                })?;
            branch.logit_index = logit_index as i32;
            branch.next_position += 1;
            tracking
                .token_counts
                .insert(branch.sequence_id, branch.next_position as usize);
            tracking
                .token_ids
                .entry(branch.sequence_id)
                .or_default()
                .push(token.0);
        }
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("failed to decode word-choice batch", error))?;
    }
    for (branch, gate) in branches.iter_mut().zip(gates) {
        branch.first_word_choice = Some(gate.evidence);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use llama_native_types::FirstWordChoiceAttemptOutcome;

    #[test]
    fn initial_mask_precedes_sampling_and_lexical_guard_still_rejects_duplicates() {
        // The repeated uppercase spelling has a different token ID. Initial
        // sampling alone cannot enforce lexical identity across these proposals.
        let logits = [100.0, 10.0, 9.0, 8.0, 7.0, 6.0];
        let pieces = ["", "It ", "IT ", "A ", "Silver ", "Cloud "];
        let mut exclusions = BTreeSet::from([0]); // Static EOG mask.
        let mut words = BTreeSet::new();
        let mut selected = Vec::new();
        let mut drawn = Vec::new();
        for slot in 0..4 {
            let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, slot);
            loop {
                gate.begin_attempt(&exclusions);
                let before_draw = exclusions.iter().copied().collect::<Vec<_>>();
                let token = sample_saved_logits(&mut LlamaSampler::greedy(), &logits, &exclusions)
                    .expect("sample")
                    .expect("remaining support");
                assert!(!exclusions.contains(&token.0));
                exclusions.insert(token.0);
                drawn.push(token.0);
                gate.record_nonterminal_token();
                let piece = pieces[usize::try_from(token.0).expect("token index")];
                let admission = gate.observe(piece, &[token.0], None, false, &mut words);
                let attempt = gate.evidence.attempts.last().expect("recorded proposal");
                assert_eq!(attempt.initial_token_exclusions, before_draw);
                assert_eq!(attempt.token_ids, [token.0]);
                match admission {
                    Admission::Accepted => {
                        selected.push(piece);
                        break;
                    }
                    Admission::Retry => {
                        assert_eq!(piece, "IT ");
                        assert_eq!(attempt.outcome, FirstWordChoiceAttemptOutcome::Duplicate);
                    }
                    other => panic!("unexpected admission: {other:?}"),
                }
            }
            assert_eq!(
                gate.evidence.total_attempted_tokens,
                if slot == 1 { 2 } else { 1 }
            );
        }
        assert_eq!(selected, ["It ", "A ", "Silver ", "Cloud "]);
        assert_eq!(drawn, [1, 2, 3, 4, 5]);
        assert_eq!(logits, [100.0, 10.0, 9.0, 8.0, 7.0, 6.0]);
    }

    #[test]
    fn exhausted_initial_support_records_no_draw_or_completion_cost() {
        let exclusions = BTreeSet::from([0, 1]);
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 3);
        gate.begin_attempt(&exclusions);
        assert!(
            sample_saved_logits(
                &mut LlamaSampler::greedy(),
                &[9.0, 8.0, f32::NEG_INFINITY],
                &exclusions
            )
            .expect("empty support")
            .is_none()
        );
        gate.exhaust_initial_support();
        assert!(!gate.pending());
        assert!(gate.evidence.exhausted);
        assert_eq!(gate.evidence.selected_attempt, None);
        assert_eq!(gate.evidence.total_attempted_tokens, 0);
        let attempt = &gate.evidence.attempts[0];
        assert_eq!(attempt.initial_token_exclusions, [0, 1]);
        assert!(attempt.token_ids.is_empty());
        assert_eq!(attempt.terminal_token_id, None);
        assert_eq!(
            attempt.outcome,
            FirstWordChoiceAttemptOutcome::InitialSupportExhausted
        );
    }

    #[test]
    fn refused_native_removal_cannot_authorize_a_retry() {
        assert!(require_kv_removal(true).is_ok());
        assert_eq!(
            require_kv_removal(false).expect_err("backend refusal").code,
            NativeErrorCode::DecodeFailed
        );
    }

    #[test]
    fn first_word_eog_and_common_terminal_path_finish_decoder_once() {
        let mut decoder = UTF_8.new_decoder();
        let mut text = decode_generated_utf8_piece(&mut decoder, b"word", false).expect("word");
        let mut finalized = false;
        let mut gate = FirstWordGate::new(FirstWordChoicePolicy::DistinctV2, 41);
        let mut words = BTreeSet::new();
        assert!(
            finalize_generated_text_once(&mut decoder, &mut text, false, &mut finalized)
                .expect("EOG admission")
                .is_empty()
        );
        assert_eq!(
            gate.observe(&text, &[7], Some(99), true, &mut words),
            Admission::Accepted
        );
        assert!(
            finalize_generated_text_once(&mut decoder, &mut text, false, &mut finalized)
                .expect("common terminal event")
                .is_empty()
        );
        assert_eq!(text, "word");
        assert_eq!(gate.evidence.selected_attempt, Some(0));
        assert_eq!(gate.evidence.attempts[0].terminal_token_id, Some(99));
    }
}
