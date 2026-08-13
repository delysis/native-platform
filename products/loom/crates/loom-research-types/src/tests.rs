use loom_types::{BlobId, CommandId, ProjectId, RevisionId};
use serde_json::{Value, json};

use crate::*;

const RAW_ONE: &[u8] = b"Alpha.\n\n<stop>";
const RAW_TWO: &[u8] = b"Beta! END";
const TOKENS_ONE: &[u32] = &[10, 11, 12];
const TOKENS_TWO: &[u32] = &[20, 21];

fn blob(label: &str) -> BlobId {
    BlobId::digest(label.as_bytes())
}

#[derive(Clone, Copy)]
struct PromotionFingerprintFields {
    project_id: ProjectId,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    subject: PromotionSubject,
    admission_record_id: BlobId,
    intended_result_blob_id: BlobId,
    intended_result_byte_len: u64,
    command_id: CommandId,
    requested_at_ms: i64,
}

impl PromotionFingerprintFields {
    fn request(self) -> PromotionCommandRequest {
        PromotionCommandRequest::new(
            self.project_id,
            self.source_revision_id,
            self.source_blob_id,
            self.subject,
            self.admission_record_id,
            self.intended_result_blob_id,
            self.intended_result_byte_len,
            self.command_id,
            self.requested_at_ms,
        )
        .unwrap()
    }
}

fn identity(prefix: &str) -> CallIdentity {
    CallIdentity::new(
        CallScope::new(
            CampaignId::new(),
            StageId::new(),
            StageAttemptId::new(),
            TrialCaseId::new(),
        ),
        blob(&format!("{prefix}/model")),
        blob(&format!("{prefix}/tokenizer")),
        blob(&format!("{prefix}/prompt")),
        blob(&format!("{prefix}/sampler")),
        blob(&format!("{prefix}/control")),
        42,
    )
}

fn call(
    id: ModelCallId,
    identity: CallIdentity,
    class: CallEvidenceClass,
    raw: &[u8],
    tokens: &[u32],
    receipt: &str,
) -> ModelCall {
    ModelCall::new(
        id,
        identity,
        class,
        CallTerminal::Completed(
            CompletedCall::new(raw, tokens, blob("events"), Some(blob(receipt))).unwrap(),
        ),
    )
    .unwrap()
}

fn span_record(
    prefix: &str,
    raw: &[u8],
    tokens: &[u32],
    displayed_end: u64,
    tail_end: u64,
) -> (ModelCall, GeneratedSpanOccurrenceRecord) {
    let model_call = call(
        ModelCallId::new(),
        identity(prefix),
        CallEvidenceClass::LiveBaseWriterClaim,
        raw,
        tokens,
        &format!("{prefix}/backend-receipt"),
    );
    let projection = OutputProjection::new(raw, displayed_end, tail_end).unwrap();
    let span = GeneratedSpanOccurrenceRecord::from_declared_call(
        GeneratedSpanOccurrenceId::new(),
        &model_call,
        raw,
        tokens,
        projection,
    )
    .unwrap();
    (model_call, span)
}

#[test]
fn persisted_live_label_is_only_a_claim_and_round_trips_as_a_record() {
    let (model_call, span) = span_record("claim", RAW_ONE, TOKENS_ONE, 6, 8);
    assert!(span.has_live_base_writer_claim());
    let restored: GeneratedSpanOccurrenceRecord =
        serde_json::from_slice(&serde_json::to_vec(&span).unwrap()).unwrap();
    assert_eq!(restored, span);
    restored
        .verify_exact(&ExactCallEvidence::new(&model_call, RAW_ONE, TOKENS_ONE))
        .unwrap();

    // The record API contains no admitted boolean or verifier lease. The most
    // favorable derived result is explicitly named a declaration.
    let evidence = [ExactCallEvidence::new(&model_call, RAW_ONE, TOKENS_ONE)];
    let assembly = CandidateAssemblyRecord::new(
        CandidateAssemblyId::new(),
        vec![AssemblyPartRecord::new(JoinBefore::None, restored)],
        &evidence,
    )
    .unwrap();
    assert_eq!(
        assembly.declared_pipeline_eligibility(),
        PipelineEligibility::DeclaredBaseWriterOnly
    );
}

#[test]
fn flat_assembly_record_reconstructs_exact_occurrences_and_round_trips() {
    let (call_one, span_one) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    let (call_two, span_two) = span_record("two", RAW_TWO, TOKENS_TWO, 5, 6);
    let evidence = [
        ExactCallEvidence::new(&call_one, RAW_ONE, TOKENS_ONE),
        ExactCallEvidence::new(&call_two, RAW_TWO, TOKENS_TWO),
    ];
    let assembly = CandidateAssemblyRecord::new(
        CandidateAssemblyId::new(),
        vec![
            AssemblyPartRecord::new(JoinBefore::None, span_one),
            AssemblyPartRecord::new(JoinBefore::ParagraphBreak, span_two),
        ],
        &evidence,
    )
    .unwrap();
    assert_eq!(assembly.reconstruct(&evidence).unwrap(), b"Alpha.\n\nBeta!");
    let restored: CandidateAssemblyRecord =
        serde_json::from_slice(&serde_json::to_vec(&assembly).unwrap()).unwrap();
    assert_eq!(restored, assembly);
    assert_eq!(restored.reconstruct(&evidence).unwrap(), b"Alpha.\n\nBeta!");
}

#[test]
fn assembly_operation_occurrences_are_stable_for_idempotent_reconstruction() {
    let (model_call, span) = span_record("stable", RAW_ONE, TOKENS_ONE, 6, 8);
    let evidence = [ExactCallEvidence::new(&model_call, RAW_ONE, TOKENS_ONE)];
    let assembly_id = CandidateAssemblyId::new();
    let first = CandidateAssemblyRecord::new(
        assembly_id,
        vec![AssemblyPartRecord::new(JoinBefore::None, span.clone())],
        &evidence,
    )
    .unwrap();
    let retry = CandidateAssemblyRecord::new(
        assembly_id,
        vec![AssemblyPartRecord::new(JoinBefore::None, span)],
        &evidence,
    )
    .unwrap();
    assert_eq!(first, retry);
}

#[test]
fn assembly_rejects_first_separator_duplicate_span_and_extra_evidence() {
    let (model_call, span) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    let evidence = [ExactCallEvidence::new(&model_call, RAW_ONE, TOKENS_ONE)];
    assert_eq!(
        CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![AssemblyPartRecord::new(JoinBefore::Space, span.clone())],
            &evidence,
        )
        .unwrap_err(),
        AssemblyError::FirstPartHasSeparator
    );
    assert!(matches!(
        CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![
                AssemblyPartRecord::new(JoinBefore::None, span.clone()),
                AssemblyPartRecord::new(JoinBefore::None, span),
            ],
            &evidence,
        ),
        Err(AssemblyError::DuplicateSpan(_))
    ));

    let (other_call, other_span) = span_record("two", RAW_TWO, TOKENS_TWO, 5, 6);
    let extra = [
        evidence[0],
        ExactCallEvidence::new(&other_call, RAW_TWO, TOKENS_TWO),
    ];
    assert_eq!(
        CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![AssemblyPartRecord::new(JoinBefore::None, other_span)],
            &extra,
        )
        .unwrap_err(),
        AssemblyError::ExactEvidenceCoverageMismatch
    );
}

#[test]
fn assembly_replay_rejects_excess_aggregate_token_evidence() {
    let token_ids = vec![7_u32; MAX_GENERATED_TOKENS as usize];
    let mut calls = Vec::new();
    let mut spans = Vec::new();
    for index in 0..5 {
        let model_call = call(
            ModelCallId::new(),
            identity(&format!("token-budget-{index}")),
            CallEvidenceClass::LiveBaseWriterClaim,
            b"x",
            &token_ids,
            &format!("token-budget-receipt-{index}"),
        );
        let span = GeneratedSpanOccurrenceRecord::from_declared_call(
            GeneratedSpanOccurrenceId::new(),
            &model_call,
            b"x",
            &token_ids,
            OutputProjection::new(b"x", 1, 1).unwrap(),
        )
        .unwrap();
        calls.push(model_call);
        spans.push(AssemblyPartRecord::new(
            if index == 0 {
                JoinBefore::None
            } else {
                JoinBefore::Space
            },
            span,
        ));
    }
    let evidence = calls
        .iter()
        .map(|model_call| ExactCallEvidence::new(model_call, b"x", &token_ids))
        .collect::<Vec<_>>();
    assert_eq!(
        CandidateAssemblyRecord::new(CandidateAssemblyId::new(), spans, &evidence).unwrap_err(),
        AssemblyError::EvidenceBudgetExceeded
    );
}

#[test]
fn reordered_parts_and_mismatched_witnesses_are_rejected() {
    let (call_one, span_one) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    let (call_two, span_two) = span_record("two", RAW_TWO, TOKENS_TWO, 5, 6);
    let evidence = [
        ExactCallEvidence::new(&call_one, RAW_ONE, TOKENS_ONE),
        ExactCallEvidence::new(&call_two, RAW_TWO, TOKENS_TWO),
    ];
    let assembly = CandidateAssemblyRecord::new(
        CandidateAssemblyId::new(),
        vec![
            AssemblyPartRecord::new(JoinBefore::None, span_one),
            AssemblyPartRecord::new(JoinBefore::Space, span_two),
        ],
        &evidence,
    )
    .unwrap();
    let mut value = serde_json::to_value(&assembly).unwrap();
    value["parts"].as_array_mut().unwrap().swap(0, 1);
    assert!(serde_json::from_value::<CandidateAssemblyRecord>(value).is_err());

    let mut value = serde_json::to_value(&assembly).unwrap();
    value["witness"]["assembled_blob_id"] = Value::String(blob("wrong").to_string());
    let restored: CandidateAssemblyRecord = serde_json::from_value(value).unwrap();
    assert_eq!(
        restored.reconstruct(&evidence).unwrap_err(),
        AssemblyError::ReconstructionWitnessMismatch
    );
}

#[test]
fn output_projection_is_nonempty_contiguous_and_utf8_aligned() {
    assert!(matches!(
        OutputProjection::new(RAW_ONE, 0, 8),
        Err(CallError::Range(RangeError::Empty))
    ));
    assert!(matches!(
        OutputProjection::new(RAW_ONE, 9, 8),
        Err(CallError::Range(RangeError::Reversed { .. }))
    ));
    let unicode = "éclair".as_bytes();
    assert!(matches!(
        OutputProjection::new(unicode, 1, unicode.len() as u64),
        Err(CallError::Range(RangeError::SplitsUtf8 { offset: 1 }))
    ));

    let invalid_partition = json!({
        "raw_output_byte_len": 10,
        "displayed": { "start": 0, "end": 4 },
        "endpoint_excluded_tail": { "start": 5, "end": 8 },
        "trimmed_stop_suffix": { "start": 8, "end": 10 }
    });
    assert!(serde_json::from_value::<OutputProjection>(invalid_partition).is_err());
}

#[test]
fn token_range_is_absent_unless_an_explicit_boundary_claim_is_recorded() {
    let (model_call, plain) = span_record("plain", RAW_ONE, TOKENS_ONE, 6, 8);
    assert_eq!(plain.token_range(), None);

    let mapping = DeclaredTokenMapping::new(
        NonEmptyTokenRange::new(0, 1).unwrap(),
        blob("token-boundaries"),
    )
    .unwrap();
    let mapped = GeneratedSpanOccurrenceRecord::from_declared_call_with_token_mapping(
        GeneratedSpanOccurrenceId::new(),
        &model_call,
        RAW_ONE,
        TOKENS_ONE,
        OutputProjection::new(RAW_ONE, 6, 8).unwrap(),
        Some(mapping),
    )
    .unwrap();
    assert_eq!(mapped.token_range(), Some(mapping.range()));

    let out_of_bounds = DeclaredTokenMapping::new(
        NonEmptyTokenRange::new(0, 4).unwrap(),
        blob("bad-boundaries"),
    )
    .unwrap();
    assert!(matches!(
        GeneratedSpanOccurrenceRecord::from_declared_call_with_token_mapping(
            GeneratedSpanOccurrenceId::new(),
            &model_call,
            RAW_ONE,
            TOKENS_ONE,
            OutputProjection::new(RAW_ONE, 6, 8).unwrap(),
            Some(out_of_bounds),
        ),
        Err(CallError::TokenRangeOutOfBounds { .. })
    ));
    assert_eq!(
        DeclaredTokenMapping::new(
            NonEmptyTokenRange::new(1, 2).unwrap(),
            blob("non-prefix-boundaries"),
        )
        .unwrap_err(),
        CallError::TokenMappingMustBeginAtOutputStart
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn extraction_receipt_detects_every_call_identity_swap() {
    let (model_call, span) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    let original = model_call.identity();
    let scope = original.scope();
    let variants = [
        CallIdentity::new(
            CallScope::new(
                CampaignId::new(),
                scope.stage_id(),
                scope.attempt_id(),
                scope.case_id(),
            ),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            CallScope::new(
                scope.campaign_id(),
                StageId::new(),
                scope.attempt_id(),
                scope.case_id(),
            ),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            CallScope::new(
                scope.campaign_id(),
                scope.stage_id(),
                StageAttemptId::new(),
                scope.case_id(),
            ),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            CallScope::new(
                scope.campaign_id(),
                scope.stage_id(),
                scope.attempt_id(),
                TrialCaseId::new(),
            ),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            original.scope(),
            blob("changed-model"),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            original.scope(),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed() ^ 1,
        ),
        CallIdentity::new(
            original.scope(),
            original.model_fingerprint(),
            blob("changed-tokenizer"),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            original.scope(),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            blob("changed-prompt"),
            original.sampler_fingerprint(),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            original.scope(),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            blob("changed-sampler"),
            original.control_program_fingerprint(),
            original.seed(),
        ),
        CallIdentity::new(
            original.scope(),
            original.model_fingerprint(),
            original.tokenizer_fingerprint(),
            original.prompt_fingerprint(),
            original.sampler_fingerprint(),
            blob("changed-control"),
            original.seed(),
        ),
    ];
    for identity in variants {
        let swapped = call(
            model_call.id(),
            identity,
            CallEvidenceClass::LiveBaseWriterClaim,
            RAW_ONE,
            TOKENS_ONE,
            "one/backend-receipt",
        );
        assert_eq!(
            span.verify_exact(&ExactCallEvidence::new(&swapped, RAW_ONE, TOKENS_ONE)),
            Err(CallError::CallBindingMismatch)
        );
    }
}

#[test]
fn extraction_receipt_detects_output_tokens_class_call_and_receipt_swaps() {
    let (model_call, span) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    assert!(matches!(
        span.verify_exact(&ExactCallEvidence::new(
            &model_call,
            b"Alphx.\n\n<stop>",
            TOKENS_ONE,
        )),
        Err(CallError::RawOutputFingerprintMismatch)
    ));
    assert_eq!(
        span.verify_exact(&ExactCallEvidence::new(&model_call, RAW_ONE, &[10, 99, 12],)),
        Err(CallError::TokenFingerprintMismatch)
    );

    let changed_class = call(
        model_call.id(),
        model_call.identity().clone(),
        CallEvidenceClass::LiveLocalCriticClaim,
        RAW_ONE,
        TOKENS_ONE,
        "one/backend-receipt",
    );
    assert_eq!(
        span.verify_exact(&ExactCallEvidence::new(&changed_class, RAW_ONE, TOKENS_ONE,)),
        Err(CallError::CallBindingMismatch)
    );

    let changed_receipt = call(
        model_call.id(),
        model_call.identity().clone(),
        CallEvidenceClass::LiveBaseWriterClaim,
        RAW_ONE,
        TOKENS_ONE,
        "swapped/backend-receipt",
    );
    assert_eq!(
        span.verify_exact(&ExactCallEvidence::new(
            &changed_receipt,
            RAW_ONE,
            TOKENS_ONE,
        )),
        Err(CallError::CallBindingMismatch)
    );

    let changed_id = call(
        ModelCallId::new(),
        model_call.identity().clone(),
        CallEvidenceClass::LiveBaseWriterClaim,
        RAW_ONE,
        TOKENS_ONE,
        "one/backend-receipt",
    );
    assert_eq!(
        span.verify_exact(&ExactCallEvidence::new(&changed_id, RAW_ONE, TOKENS_ONE)),
        Err(CallError::CallIdMismatch)
    );

    let changed_events = ModelCall::new(
        model_call.id(),
        model_call.identity().clone(),
        CallEvidenceClass::LiveBaseWriterClaim,
        CallTerminal::Completed(
            CompletedCall::new(
                RAW_ONE,
                TOKENS_ONE,
                blob("swapped-events"),
                Some(blob("one/backend-receipt")),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        span.verify_exact(&ExactCallEvidence::new(
            &changed_events,
            RAW_ONE,
            TOKENS_ONE,
        )),
        Err(CallError::CallBindingMismatch)
    );
}

#[test]
fn tampered_extraction_receipt_and_unknown_fields_are_rejected() {
    let (_, span) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    let mut value = serde_json::to_value(&span).unwrap();
    value["extraction_receipt"] = Value::String(blob("tampered").to_string());
    assert!(serde_json::from_value::<GeneratedSpanOccurrenceRecord>(value).is_err());

    let model_call = call(
        ModelCallId::new(),
        identity("unknown"),
        CallEvidenceClass::Fixture,
        RAW_ONE,
        TOKENS_ONE,
        "fixture",
    );
    let mut value = serde_json::to_value(model_call).unwrap();
    value["caller_asserted_live"] = Value::Bool(true);
    assert!(serde_json::from_value::<ModelCall>(value).is_err());

    assert!(
        serde_json::from_value::<NonEmptyByteRange>(json!({
            "start": 0,
            "end": 1,
            "ignored": true
        }))
        .is_err()
    );
}

#[test]
fn span_wire_rejects_internally_inconsistent_call_and_token_claims() {
    let (_, span) = span_record("wire-invariants", RAW_ONE, TOKENS_ONE, 6, 8);

    let mut changed_outer_blob = serde_json::to_value(&span).unwrap();
    changed_outer_blob["raw_output_blob_id"] = Value::String(blob("other-output").to_string());
    assert!(serde_json::from_value::<GeneratedSpanOccurrenceRecord>(changed_outer_blob).is_err());

    let mut changed_binding_len = serde_json::to_value(&span).unwrap();
    changed_binding_len["call_binding"]["raw_output_byte_len"] = json!(RAW_ONE.len() + 1);
    assert!(serde_json::from_value::<GeneratedSpanOccurrenceRecord>(changed_binding_len).is_err());

    let mut missing_live_receipt = serde_json::to_value(&span).unwrap();
    missing_live_receipt["call_binding"]["backend_receipt_blob_id"] = Value::Null;
    assert!(serde_json::from_value::<GeneratedSpanOccurrenceRecord>(missing_live_receipt).is_err());

    let mut range_without_boundaries = serde_json::to_value(&span).unwrap();
    range_without_boundaries["token_range"] = json!({ "start": 0, "end": 1 });
    assert!(
        serde_json::from_value::<GeneratedSpanOccurrenceRecord>(range_without_boundaries).is_err()
    );

    let mut boundaries_without_range = serde_json::to_value(&span).unwrap();
    boundaries_without_range["call_binding"]["token_boundaries_fingerprint"] =
        Value::String(blob("boundaries").to_string());
    assert!(
        serde_json::from_value::<GeneratedSpanOccurrenceRecord>(boundaries_without_range).is_err()
    );

    let mut non_prefix_range = serde_json::to_value(&span).unwrap();
    non_prefix_range["token_range"] = json!({ "start": 1, "end": 2 });
    non_prefix_range["call_binding"]["token_boundaries_fingerprint"] =
        Value::String(blob("boundaries").to_string());
    assert!(serde_json::from_value::<GeneratedSpanOccurrenceRecord>(non_prefix_range).is_err());
}

#[test]
fn serde_boundaries_reject_oversized_collections_text_hashes_and_ids() {
    assert!(serde_json::from_str::<BoundedVec<u8, 2>>("[1,2,3]").is_err());
    assert!(serde_json::from_str::<NonEmptyBoundedVec<u8, 2>>("[]").is_err());
    assert!(serde_json::from_str::<NonEmptyBoundedVec<u8, 2>>("[1,2,3]").is_err());
    assert!(serde_json::from_str::<BoundedText<4>>("\"abcde\"").is_err());
    assert!(serde_json::from_str::<BoundedText<4>>("\"a\\nb\"").is_err());
    assert!(serde_json::from_value::<ModelCallId>(Value::String("X".repeat(1_024))).is_err());

    let (model_call, _) = span_record("bounded-hash", RAW_ONE, TOKENS_ONE, 6, 8);
    let mut value = serde_json::to_value(model_call).unwrap();
    value["identity"]["model_fingerprint"] = Value::String("a".repeat(4_096));
    assert!(serde_json::from_value::<ModelCall>(value).is_err());

    let operation_id = PipelineOperationId::new();
    let mut operation = json!({
        "id": operation_id,
        "kind": {
            "operation": "literal_text",
            "content_blob_id": blob("literal")
        },
        "inputs": []
    });
    operation["inputs"] = Value::Array(
        (0..=MAX_OPERATION_INPUTS)
            .map(|_| Value::String(PipelineOperationId::new().to_string()))
            .collect(),
    );
    assert!(serde_json::from_value::<PipelineOperation>(operation).is_err());
}

#[test]
fn wire_ids_and_hashes_require_their_canonical_encoding() {
    let canonical_ulid = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    assert!(
        serde_json::from_value::<ModelCallId>(Value::String(canonical_ulid.to_owned())).is_ok()
    );
    assert!(
        serde_json::from_value::<ModelCallId>(Value::String(canonical_ulid.to_ascii_lowercase()))
            .is_err()
    );

    let (model_call, _) = span_record("canonical", RAW_ONE, TOKENS_ONE, 6, 8);
    let mut noncanonical_hash = serde_json::to_value(model_call).unwrap();
    noncanonical_hash["identity"]["model_fingerprint"] = Value::String("A".repeat(64));
    assert!(serde_json::from_value::<ModelCall>(noncanonical_hash).is_err());

    let request = PromotionCommandRequest::new(
        ProjectId::new(),
        RevisionId::new(),
        blob("source"),
        PromotionSubject::CandidateProjection {
            projection_id: CandidateProjectionId::new(),
        },
        blob("admission"),
        blob("result"),
        7,
        CommandId::new(),
        1,
    )
    .unwrap();
    let authority = PromotionAuthority::new(
        PromotionActor::new("reviewer").unwrap(),
        request,
        UserPresenceEvidence::new(
            UserPresenceKind::EditorGesture,
            blob("session"),
            blob("event"),
            1,
            1,
        )
        .unwrap(),
    )
    .unwrap();
    let mut noncanonical_revision = serde_json::to_value(&authority).unwrap();
    let revision = noncanonical_revision["request"]["source_revision_id"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    noncanonical_revision["request"]["source_revision_id"] = Value::String(revision);
    assert!(serde_json::from_value::<PromotionAuthority>(noncanonical_revision).is_err());

    let mut noncanonical_command = serde_json::to_value(authority).unwrap();
    let command = noncanonical_command["request"]["command_id"]
        .as_str()
        .unwrap()
        .to_ascii_lowercase();
    noncanonical_command["request"]["command_id"] = Value::String(command);
    assert!(serde_json::from_value::<PromotionAuthority>(noncanonical_command).is_err());
}

#[test]
fn operation_graph_rejects_excess_aggregate_edges() {
    let mut nodes = Vec::with_capacity(MAX_OPERATION_NODES);
    let root = PipelineOperationId::new();
    nodes.push(
        PipelineOperation::new(
            root,
            PipelineOperationKind::LiteralText {
                content_blob_id: blob("root"),
            },
            vec![],
        )
        .unwrap(),
    );
    let mut previous = root;
    let mut early = Vec::new();
    for index in 1..MAX_OPERATION_NODES {
        let id = PipelineOperationId::new();
        let mut inputs = vec![previous];
        if index >= 2 {
            inputs.push(root);
        }
        if index == MAX_OPERATION_NODES - 1 {
            inputs.extend(early.iter().take(4).copied());
        }
        nodes.push(
            PipelineOperation::new(
                id,
                PipelineOperationKind::HumanTransformation {
                    content_blob_id: blob(&format!("node-{index}")),
                },
                inputs,
            )
            .unwrap(),
        );
        if index < 6 {
            early.push(id);
        }
        previous = id;
    }
    assert!(matches!(
        OperationGraph::new(nodes, previous),
        Err(OperationGraphError::Bound(BoundError::TooMany {
            maximum: MAX_OPERATION_EDGES,
            ..
        }))
    ));
}

#[test]
fn diagnostic_classes_reconstruct_but_derive_ineligible_pipeline_claims() {
    let classes = [
        CallEvidenceClass::LiveInstructEditorClaim,
        CallEvidenceClass::LiveLocalCriticClaim,
        CallEvidenceClass::LiveCodexCriticClaim,
        CallEvidenceClass::Fixture,
        CallEvidenceClass::HistoricalReceipt,
        CallEvidenceClass::Mock,
    ];
    for class in classes {
        let model_call = call(
            ModelCallId::new(),
            identity("diagnostic"),
            class,
            RAW_ONE,
            TOKENS_ONE,
            "diagnostic-receipt",
        );
        let span = GeneratedSpanOccurrenceRecord::from_declared_call(
            GeneratedSpanOccurrenceId::new(),
            &model_call,
            RAW_ONE,
            TOKENS_ONE,
            OutputProjection::new(RAW_ONE, 6, 8).unwrap(),
        )
        .unwrap();
        let evidence = [ExactCallEvidence::new(&model_call, RAW_ONE, TOKENS_ONE)];
        let assembly = CandidateAssemblyRecord::new(
            CandidateAssemblyId::new(),
            vec![AssemblyPartRecord::new(JoinBefore::None, span)],
            &evidence,
        )
        .unwrap();
        assert!(matches!(
            assembly.declared_pipeline_eligibility(),
            PipelineEligibility::Ineligible { .. }
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn arbitrary_literal_human_codex_and_fixture_text_have_only_mixed_record_lane() {
    let literal_id = PipelineOperationId::new();
    let human_id = PipelineOperationId::new();
    let graph = OperationGraph::new(
        vec![
            PipelineOperation::new(
                literal_id,
                PipelineOperationKind::LiteralText {
                    content_blob_id: blob("literal"),
                },
                vec![],
            )
            .unwrap(),
            PipelineOperation::new(
                human_id,
                PipelineOperationKind::HumanTransformation {
                    content_blob_id: BlobId::digest(b"Human-authored prose."),
                },
                vec![literal_id],
            )
            .unwrap(),
        ],
        human_id,
    )
    .unwrap();
    assert_eq!(
        MixedAuthorshipAssemblyRecord::new(
            MixedAuthorshipAssemblyId::new(),
            b"Swapped prose.",
            graph.clone(),
        )
        .unwrap_err(),
        AssemblyError::MixedOutputGraphMismatch
    );
    let mixed = MixedAuthorshipAssemblyRecord::new(
        MixedAuthorshipAssemblyId::new(),
        b"Human-authored prose.",
        graph,
    )
    .unwrap();
    assert!(matches!(
        mixed.declared_pipeline_eligibility(),
        PipelineEligibility::Ineligible { .. }
    ));
    assert_eq!(
        diagnostic_reason_labels(&mixed.declared_pipeline_eligibility()),
        vec!["literal_text", "human_text"]
    );
    mixed.verify_output(b"Human-authored prose.").unwrap();
    assert!(mixed.verify_output(b"Agent substitution.").is_err());

    // There is no literal-text field in an assembly part record.
    assert!(
        serde_json::from_value::<AssemblyPartRecord>(json!({
            "join_before": "none",
            "span": "arbitrary prose"
        }))
        .is_err()
    );

    let instruct_call = call(
        ModelCallId::new(),
        identity("instruct-mixed"),
        CallEvidenceClass::LiveInstructEditorClaim,
        b"Edited prose.",
        &[91, 92],
        "instruct-receipt",
    );
    let source_id = PipelineOperationId::new();
    let edit_id = PipelineOperationId::new();
    let instruct_graph = OperationGraph::new(
        vec![
            PipelineOperation::new(
                source_id,
                PipelineOperationKind::LiteralText {
                    content_blob_id: blob("source-before-edit"),
                },
                vec![],
            )
            .unwrap(),
            PipelineOperation::new(
                edit_id,
                PipelineOperationKind::InstructEditorTransformation {
                    call_id: instruct_call.id(),
                    output_blob_id: BlobId::digest(b"Edited prose."),
                },
                vec![source_id],
            )
            .unwrap(),
        ],
        edit_id,
    )
    .unwrap();
    assert_eq!(
        MixedAuthorshipAssemblyRecord::new(
            MixedAuthorshipAssemblyId::new(),
            b"Edited prose.",
            instruct_graph.clone(),
        )
        .unwrap_err(),
        AssemblyError::MixedOutputRequiresCallEvidence
    );
    MixedAuthorshipAssemblyRecord::new_from_call_output(
        MixedAuthorshipAssemblyId::new(),
        b"Edited prose.",
        instruct_graph,
        &ExactCallEvidence::new(&instruct_call, b"Edited prose.", &[91, 92]),
    )
    .unwrap();
}

#[test]
fn projection_record_pins_source_without_copying_manuscript_bytes() {
    let (model_call, span) = span_record("one", RAW_ONE, TOKENS_ONE, 6, 8);
    let evidence = [ExactCallEvidence::new(&model_call, RAW_ONE, TOKENS_ONE)];
    let assembly = CandidateAssemblyRecord::new(
        CandidateAssemblyId::new(),
        vec![AssemblyPartRecord::new(JoinBefore::None, span)],
        &evidence,
    )
    .unwrap();
    let source = b"Before OLD after";
    let projection = CandidateProjectionRecord::new(
        CandidateProjectionId::new(),
        &assembly,
        RevisionId::new(),
        BlobId::digest(source),
        source,
        ByteRange::new(7, 10).unwrap(),
        &evidence,
    )
    .unwrap();
    assert_eq!(
        projection.apply(&assembly, source, &evidence).unwrap(),
        b"Before Alpha. after"
    );
    let serialized = serde_json::to_string(&projection).unwrap();
    assert!(!serialized.contains("Before OLD after"));
    let restored: CandidateProjectionRecord = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored, projection);
    let mut zero_length = serde_json::to_value(&projection).unwrap();
    zero_length["witness"]["resulting_byte_len"] = json!(0);
    assert!(serde_json::from_value::<CandidateProjectionRecord>(zero_length).is_err());
    assert!(
        projection
            .apply(&assembly, b"Before BAD after", &evidence)
            .is_err()
    );

    let mut tampered = serde_json::to_value(&projection).unwrap();
    let replacement = PipelineOperationId::new().to_string();
    let last = tampered["operation_graph"]["nodes"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    last["id"] = Value::String(replacement.clone());
    tampered["operation_graph"]["output"] = Value::String(replacement);
    assert!(serde_json::from_value::<CandidateProjectionRecord>(tampered).is_err());

    let mut changed_source_revision = serde_json::to_value(&projection).unwrap();
    changed_source_revision["source_revision_id"] = Value::String(RevisionId::new().to_string());
    assert!(serde_json::from_value::<CandidateProjectionRecord>(changed_source_revision).is_err());

    let mut changed_assembly = serde_json::to_value(&projection).unwrap();
    changed_assembly["assembly_id"] = Value::String(CandidateAssemblyId::new().to_string());
    assert!(serde_json::from_value::<CandidateProjectionRecord>(changed_assembly).is_err());
}

#[test]
fn promotion_authority_requires_concrete_user_presence() {
    assert_eq!(
        UserPresenceEvidence::new(
            UserPresenceKind::EditorGesture,
            blob("session"),
            blob("event"),
            0,
            1,
        )
        .unwrap_err(),
        AssemblyError::InvalidUserPresence
    );
    let source_revision = RevisionId::new();
    let source_blob = blob("source");
    let project = ProjectId::new();
    let projection = CandidateProjectionId::new();
    let admission = blob("admission");
    let result = blob("result");
    let command = CommandId::new();
    let presence = UserPresenceEvidence::new(
        UserPresenceKind::CliInteractiveConfirmation,
        blob("session"),
        blob("event"),
        7,
        1_900_000_000_000,
    )
    .unwrap();
    let promotion_request = PromotionCommandRequest::new(
        project,
        source_revision,
        source_blob,
        PromotionSubject::CandidateProjection {
            projection_id: projection,
        },
        admission,
        result,
        42,
        command,
        1_899_999_999_999,
    )
    .unwrap();
    let authority = PromotionAuthority::new(
        PromotionActor::new("george").unwrap(),
        promotion_request,
        presence,
    )
    .unwrap();
    assert_eq!(authority.project_id(), project);
    assert_eq!(authority.source_revision_id(), source_revision);
    assert_eq!(authority.source_blob_id(), source_blob);
    assert_eq!(
        authority.subject(),
        PromotionSubject::CandidateProjection {
            projection_id: projection
        }
    );
    assert_eq!(authority.admission_record_id(), admission);
    assert_eq!(authority.intended_result_blob_id(), result);
    assert_eq!(authority.intended_result_byte_len(), 42);
    assert_eq!(authority.command_id(), command);
    assert_eq!(
        authority.command_request_fingerprint(),
        BlobId::digest(authority.request().canonical_request_bytes())
    );
    assert_eq!(authority.command_requested_at_ms(), 1_899_999_999_999);
    assert_eq!(authority.actor().as_str(), "george");

    assert!(
        PromotionCommandRequest::new(
            project,
            source_revision,
            source_blob,
            PromotionSubject::CandidateProjection {
                projection_id: projection,
            },
            admission,
            result,
            0,
            command,
            1,
        )
        .is_err()
    );

    let mut substituted = serde_json::to_value(&authority).unwrap();
    substituted["request"]["command_request_fingerprint"] =
        Value::String(blob("substituted request fingerprint").to_string());
    assert!(serde_json::from_value::<PromotionAuthority>(substituted).is_err());

    let mut changed_field = serde_json::to_value(&authority).unwrap();
    changed_field["request"]["intended_result_byte_len"] = Value::from(43_u64);
    assert!(serde_json::from_value::<PromotionAuthority>(changed_field).is_err());
}

#[test]
fn promotion_request_fingerprint_changes_with_every_request_field() {
    let fields = PromotionFingerprintFields {
        project_id: ProjectId::new(),
        source_revision_id: RevisionId::new(),
        source_blob_id: blob("fingerprint source"),
        subject: PromotionSubject::CandidateProjection {
            projection_id: CandidateProjectionId::new(),
        },
        admission_record_id: blob("fingerprint admission"),
        intended_result_blob_id: blob("fingerprint result"),
        intended_result_byte_len: 42,
        command_id: CommandId::new(),
        requested_at_ms: 1_900_000_000_000,
    };
    let base = fields.request();
    let variants = [
        PromotionFingerprintFields {
            project_id: ProjectId::new(),
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            source_revision_id: RevisionId::new(),
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            source_blob_id: blob("changed source"),
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            subject: PromotionSubject::CandidateProjection {
                projection_id: CandidateProjectionId::new(),
            },
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            subject: PromotionSubject::MixedAuthorship {
                mixed_assembly_id: MixedAuthorshipAssemblyId::new(),
            },
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            admission_record_id: blob("changed admission"),
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            intended_result_blob_id: blob("changed result"),
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            intended_result_byte_len: 43,
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            command_id: CommandId::new(),
            ..fields
        }
        .request(),
        PromotionFingerprintFields {
            requested_at_ms: fields.requested_at_ms + 1,
            ..fields
        }
        .request(),
    ];
    for variant in variants {
        assert_ne!(
            variant.command_request_fingerprint(),
            base.command_request_fingerprint()
        );
        assert_ne!(
            variant.canonical_request_bytes(),
            base.canonical_request_bytes()
        );
    }
}

#[test]
fn model_call_has_exactly_one_validated_terminal() {
    let failed = ModelCall::new(
        ModelCallId::new(),
        identity("failed"),
        CallEvidenceClass::HistoricalReceipt,
        CallTerminal::Failed {
            message: TerminalMessage::new("out of memory").unwrap(),
        },
    )
    .unwrap();
    assert!(matches!(failed.terminal(), CallTerminal::Failed { .. }));
    assert_eq!(
        failed.completed().unwrap_err(),
        CallError::CallDidNotComplete
    );

    let serialized = serde_json::to_string(&failed).unwrap();
    let duplicate_terminal = serialized.replacen(
        "\"terminal\":",
        "\"terminal\":{\"status\":\"rejected\",\"detail\":{\"message\":\"x\"}},\"terminal\":",
        1,
    );
    assert!(serde_json::from_str::<ModelCall>(&duplicate_terminal).is_err());
}
