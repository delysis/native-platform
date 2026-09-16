//! Independent requests are not manuscript branches. Preserve their identities
//! in an explicit envelope and retain the native envelope's outputs unchanged.
use std::collections::BTreeSet;
use std::time::Duration;

use llama_native_engine::{GenerationTicket, TryWaitOutcome};
use llama_native_types::{
    CompletionPrompt, GenerationBatchRequest, GenerationCase, GenerationEvent, GenerationInput,
    GenerationOutput, ModelFingerprint, NativeError, NativeErrorCode, NativeTransport,
    PreparedPrompt, SamplingConfig, SpecialTokenPolicy,
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
    native_request: GenerationBatchRequest,
    model_fingerprint: ModelFingerprint,
}

/// Operational evidence from the concrete native runtime, not a strict
/// `VerifiedGenerationBatch` seal or hardware attestation. A cancelled case can
/// end between UTF-8 token pieces, so a whole-batch strict-text seal must not
/// become a prerequisite for retaining its successfully completed siblings.
#[derive(Debug)]
pub struct IndependentRawCompletion {
    requests: Vec<IndependentRawRequest>,
    prepared: Vec<PreparedPrompt>,
    native_request: GenerationBatchRequest,
    model_fingerprint: ModelFingerprint,
    outputs: Vec<GenerationOutput>,
}

impl IndependentRawBatch {
    pub fn start(
        runtime: &NativeHostRuntime,
        profile: &LocalModelProfile,
        requests: Vec<IndependentRawRequest>,
    ) -> Result<Self, NativeError> {
        validate_requests(&requests)?;
        if requests.len() > profile.max_parallel_cases as usize {
            return Err(invalid(
                "independent batch exceeds the selected sequence budget",
            ));
        }
        let handle = runtime.acquire_research_handle(profile)?;
        let model_fingerprint = handle
            .status()
            .fingerprint
            .ok_or_else(|| invalid("resident model fingerprint is unavailable"))?;
        let input = GenerationInput::Completion {
            prompts: requests
                .iter()
                .map(|request| CompletionPrompt::Text {
                    text: request.prompt.clone(),
                    special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
                })
                .collect(),
        };
        // Tokenization uses the same resident worker as generation. Supplying
        // exact tokens enables its aggregate cell-budget preflight and disables
        // implicit reuse of a previous request's resident prefix.
        let prepared = handle.prepare_input(input)?;
        if prepared.len() != requests.len() {
            return Err(invalid(
                "native preparation returned the wrong number of prompts",
            ));
        }
        for (index, (request, prompt)) in requests.iter().zip(&prepared).enumerate() {
            if prompt.input_index != index
                || prompt.source_sha256 != BlobId::digest(request.prompt.as_bytes()).to_string()
                || prompt.token_ids.is_empty()
            {
                return Err(invalid(
                    "native preparation is not bound to the exact raw input",
                ));
            }
        }
        let identity = serde_json::to_vec(&("loom_independent_raw_batch_v1", &requests))
            .map_err(|_| invalid("independent batch identity could not be encoded"))?;
        let native_request = GenerationBatchRequest {
            request_id: format!("peer-batch-{}", BlobId::digest(&identity)),
            model_id: profile.model_id.clone(),
            media: Vec::new(),
            cases: requests
                .iter()
                .zip(&prepared)
                .map(|(request, prompt)| GenerationCase {
                    case_id: request.case_id.clone(),
                    input: GenerationInput::Completion {
                        prompts: vec![CompletionPrompt::Tokens {
                            token_ids: prompt.token_ids.clone(),
                        }],
                    },
                    sampling: request.sampling.clone(),
                    cached_prefix: None,
                })
                .collect(),
        };
        // The native engine admits the entire exact-token cell budget before
        // mutating KV. This is one invocation, not several concurrent tickets.
        let ticket = handle.generate_batch(native_request.clone())?;
        Ok(Self {
            ticket: Some(ticket),
            requests,
            prepared,
            native_request,
            model_fingerprint,
        })
    }

    pub fn cancel_request(&self, request_id: &str) -> bool {
        let Some(request) = self
            .requests
            .iter()
            .find(|item| item.request_id == request_id)
        else {
            return false;
        };
        self.ticket
            .as_ref()
            .is_some_and(|ticket| ticket.cancel_branch(&request.case_id))
    }

    pub fn cancel_all(&self) {
        if let Some(ticket) = &self.ticket {
            let _ = ticket.cancel_all();
        }
    }

    /// Advisory events may be dropped under channel pressure. Receiving two
    /// case events does not itself qualify a shared native decode boundary.
    pub fn receive_event_timeout(&self, timeout: Duration) -> Option<GenerationEvent> {
        self.ticket
            .as_ref()
            .and_then(|ticket| ticket.events.recv_timeout(timeout).ok())
    }

    pub fn try_complete(&mut self) -> Result<Option<IndependentRawCompletion>, NativeError> {
        let ticket = self
            .ticket
            .take()
            .ok_or_else(|| invalid("independent batch already completed"))?;
        match ticket.try_wait()? {
            TryWaitOutcome::Pending(ticket) => {
                self.ticket = Some(ticket);
                Ok(None)
            }
            TryWaitOutcome::Ready(outputs) => {
                if outputs.len() != self.requests.len() {
                    return Err(invalid("native batch lost an independent request"));
                }
                for (index, (request, output)) in self.requests.iter().zip(&outputs).enumerate() {
                    if output.request_id != self.native_request.request_id
                        || output.input_index != index
                        || output.branch_id != request.case_id
                        || output.model_id != self.native_request.model_id
                        || !output.real_engine_invoked
                        || output.fake_fixture
                        || output.transport != NativeTransport::InProcess
                    {
                        return Err(invalid("native batch identity or execution class changed"));
                    }
                }
                Ok(Some(IndependentRawCompletion {
                    requests: std::mem::take(&mut self.requests),
                    prepared: std::mem::take(&mut self.prepared),
                    native_request: self.native_request.clone(),
                    model_fingerprint: self.model_fingerprint.clone(),
                    outputs,
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
        &self.outputs
    }

    pub fn requests(&self) -> &[IndependentRawRequest] {
        &self.requests
    }

    pub fn model_fingerprint(&self) -> &ModelFingerprint {
        &self.model_fingerprint
    }

    /// Host-private operational provenance, not a serialized strict seal.
    /// Outputs keep the actual native batch ID and input indexes; no receipt
    /// is rewritten to pretend that an independently owned request ran alone.
    pub fn receipt_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&serde_json::json!({
            "kind": "loom_independent_raw_execution_v1",
            "evidence_class": "operational_native",
            "requests": self.requests,
            "prepared_prompts": self.prepared,
            "native_request": self.native_request,
            "model_fingerprint": self.model_fingerprint,
            "native_outputs": self.outputs,
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
        if request.request_id.is_empty()
            || request.request_id.len() > 160
            || request.case_id.is_empty()
            || request.case_id.len() > 160
            || !ids.insert(&request.request_id)
            || !cases.insert(&request.case_id)
            || request.prompt.is_empty()
            || request.prompt.len() > MAX_RAW_PROMPT_BYTES
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
