//! Independent requests are not manuscript branches. Preserve their identities
//! in an explicit envelope and retain the native envelope's outputs unchanged.
use std::collections::BTreeSet;
use std::time::Duration;

use llama_native_engine::{GenerationTicket, TryWaitOutcome, VerifiedGenerationBatch};
use llama_native_types::{
    CompletionPrompt, GenerationBatchRequest, GenerationCase, GenerationEvent, GenerationInput,
    GenerationOutput, NativeError, NativeErrorCode, PreparedPrompt, SamplingConfig,
    SpecialTokenPolicy,
};
use loom_types::BlobId;
use serde::{Deserialize, Serialize};

use crate::{LocalModelProfile, NativeHostRuntime};

pub const MAX_INDEPENDENT_RAW_CASES: usize = 4;
const MAX_RAW_PROMPT_BYTES: usize = 64 * 1024;
const MAX_RAW_OUTPUT_TOKENS: u32 = 2048;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndependentRawRequest {
    /// Original immutable request identity, not the transient batch identity.
    pub request_id: String,
    /// Unique native case identity; cancellation uses this mapping.
    pub case_id: String,
    pub prompt: String,
    pub sampling: SamplingConfig,
}

/// Owns the per-call completion ticket, not the resident model. Dropping this
/// owner cancels and waits; it cannot make foreground code race detached work.
#[derive(Debug)]
pub struct IndependentRawBatch {
    ticket: Option<GenerationTicket>,
    requests: Vec<IndependentRawRequest>,
    prepared: Vec<PreparedPrompt>,
}

/// An in-memory native seal, never reconstructed from saved JSON. The saved
/// snapshot is evidence of this observation, not transferable execution authority.
#[derive(Debug)]
pub struct IndependentRawCompletion {
    requests: Vec<IndependentRawRequest>,
    prepared: Vec<PreparedPrompt>,
    seal: VerifiedGenerationBatch,
}

impl IndependentRawBatch {
    pub fn start(
        runtime: &NativeHostRuntime,
        profile: &LocalModelProfile,
        requests: Vec<IndependentRawRequest>,
    ) -> Result<Self, NativeError> {
        validate_requests(&requests)?;
        if requests.len() > profile.max_parallel_cases as usize {
            return Err(invalid("independent batch exceeds the selected sequence budget"));
        }
        let handle = runtime.acquire_research_handle(profile)?;
        let input = GenerationInput::Completion {
            prompts: requests.iter().map(|request| CompletionPrompt::Text {
                text: request.prompt.clone(),
                special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
            }).collect(),
        };
        // Tokenization uses the same resident worker as generation. Supplying
        // exact tokens enables its aggregate cell-budget preflight and disables
        // implicit reuse of a previous request's resident prefix.
        let prepared = handle.prepare_input(input)?;
        if prepared.len() != requests.len() {
            return Err(invalid("native preparation returned the wrong number of prompts"));
        }
        for (index, (request, prompt)) in requests.iter().zip(&prepared).enumerate() {
            if prompt.input_index != index
                || prompt.source_sha256 != BlobId::digest(request.prompt.as_bytes()).to_string()
                || prompt.token_ids.is_empty()
            {
                return Err(invalid("native preparation is not bound to the exact raw input"));
            }
        }
        let identity = serde_json::to_vec(&("loom_independent_raw_batch_v1", &requests))
            .map_err(|_| invalid("independent batch identity could not be encoded"))?;
        let request = GenerationBatchRequest {
            request_id: format!("peer-batch-{}", BlobId::digest(&identity)),
            model_id: profile.model_id.clone(),
            media: Vec::new(),
            cases: requests.iter().zip(&prepared).map(|(request, prompt)| GenerationCase {
                case_id: request.case_id.clone(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens { token_ids: prompt.token_ids.clone() }],
                },
                sampling: request.sampling.clone(),
                cached_prefix: None,
            }).collect(),
        };
        // The native engine admits the entire exact-token cell budget before
        // mutating KV. This is one invocation, not several concurrent tickets.
        let ticket = handle.generate_batch(request)?;
        Ok(Self { ticket: Some(ticket), requests, prepared })
    }

    pub fn cancel_request(&self, request_id: &str) -> bool {
        let Some(request) = self.requests.iter().find(|item| item.request_id == request_id) else {
            return false;
        };
        self.ticket.as_ref().is_some_and(|ticket| ticket.cancel_branch(&request.case_id))
    }

    pub fn cancel_all(&self) {
        if let Some(ticket) = &self.ticket {
            for request in &self.requests {
                let _ = ticket.cancel_branch(&request.case_id);
            }
        }
    }

    /// Advisory events may be dropped under channel pressure. Qualification
    /// must use the completed native seal rather than treating these as proof.
    pub fn receive_event_timeout(&self, timeout: Duration) -> Option<GenerationEvent> {
        self.ticket.as_ref().and_then(|ticket| ticket.events.recv_timeout(timeout).ok())
    }

    pub fn try_complete(&mut self) -> Result<Option<IndependentRawCompletion>, NativeError> {
        let ticket = self.ticket.take().ok_or_else(|| invalid("independent batch already completed"))?;
        match ticket.try_wait_verified()? {
            TryWaitOutcome::Pending(ticket) => {
                self.ticket = Some(ticket);
                Ok(None)
            }
            TryWaitOutcome::Ready(seal) => {
                if seal.outputs().len() != self.requests.len() {
                    return Err(invalid("sealed batch lost an independent request"));
                }
                for (index, (request, output)) in self.requests.iter().zip(seal.outputs()).enumerate() {
                    if output.input_index != index || output.branch_id != request.case_id {
                        return Err(invalid("sealed batch case mapping changed"));
                    }
                }
                Ok(Some(IndependentRawCompletion {
                    requests: std::mem::take(&mut self.requests),
                    prepared: std::mem::take(&mut self.prepared),
                    seal,
                }))
            }
        }
    }
}

impl Drop for IndependentRawBatch {
    fn drop(&mut self) {
        self.cancel_all();
        if let Some(ticket) = self.ticket.take() {
            // A dropped observer is not native completion. This wait deliberately
            // outlives a request deadline until the owning worker has returned.
            let _ = ticket.wait();
        }
    }
}

impl IndependentRawCompletion {
    pub fn outputs(&self) -> &[GenerationOutput] {
        self.seal.outputs()
    }

    pub fn requests(&self) -> &[IndependentRawRequest] {
        &self.requests
    }

    pub fn native_batch(&self) -> &VerifiedGenerationBatch {
        &self.seal
    }

    /// Host-private full provenance. Outputs keep the actual native batch ID
    /// and input indexes; no receipt is rewritten to pretend it ran separately.
    pub fn receipt_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let traces = self.seal.token_piece_traces().iter().map(|trace| {
            (trace.raw_piece_bytes(), trace.cumulative_boundaries())
        }).collect::<Vec<_>>();
        serde_json::to_vec(&serde_json::json!({
            "kind": "loom_independent_raw_execution_v1",
            "requests": self.requests,
            "prepared_prompts": self.prepared,
            "native_request": self.seal.request(),
            "model_fingerprint": self.seal.model_fingerprint(),
            "native_outputs": self.seal.outputs(),
            "native_events": self.seal.events(),
            "terminal_sampled_token_ids": self.seal.terminal_sampled_token_ids(),
            "token_piece_traces": traces,
        }))
    }
}

fn validate_requests(requests: &[IndependentRawRequest]) -> Result<(), NativeError> {
    if requests.is_empty() || requests.len() > MAX_INDEPENDENT_RAW_CASES {
        return Err(invalid("independent batch requires one to four requests"));
    }
    let mut ids = BTreeSet::new();
    let mut cases = BTreeSet::new();
    for request in requests {
        if request.request_id.is_empty() || request.request_id.len() > 160
            || request.case_id.is_empty() || request.case_id.len() > 160
            || !ids.insert(&request.request_id) || !cases.insert(&request.case_id)
            || request.prompt.is_empty() || request.prompt.len() > MAX_RAW_PROMPT_BYTES
            || !(1..=MAX_RAW_OUTPUT_TOKENS).contains(&request.sampling.max_tokens)
            || request.sampling.seed == u32::MAX
        {
            return Err(invalid("invalid independent raw request identity or limits"));
        }
    }
    Ok(())
}

fn invalid(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

#[cfg(test)]
#[path = "independent_tests.rs"]
mod tests;
