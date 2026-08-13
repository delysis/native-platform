use loom_types::{BlobId, RevisionId};
use serde_json::{Value, json};

use crate::*;

fn blob(label: &str) -> BlobId {
    BlobId::digest(label.as_bytes())
}

fn source_range(bytes: &[u8], revision_id: RevisionId, start: u64, end: u64) -> PromptSourceRange {
    PromptSourceRange::new(
        revision_id,
        BlobId::digest(bytes),
        NonEmptyByteRange::new(start, end).expect("nonempty test range"),
    )
    .expect("bounded test source")
}

fn field(
    field_id: u32,
    subject: TypedPlaceholder,
    arguments: Vec<TypedPlaceholder>,
    evidence: PromptSourceRange,
) -> GroundedBacktranslationField {
    GroundedBacktranslationField::new(
        field_id,
        subject,
        SemanticTermCode::new(100 + field_id).expect("nonzero term"),
        arguments,
        vec![evidence],
    )
    .expect("valid grounded field")
}

fn proposal_fixture() -> (BacktranslationProposal, RevisionId, Vec<u8>) {
    let bytes = b"Mara lifted the key. She knew the door was open.".to_vec();
    let revision_id = RevisionId::new();
    let source = source_range(&bytes, revision_id, 0, bytes.len() as u64);
    let role = TypedPlaceholder::role(1);
    let object = TypedPlaceholder::object(1);
    let roles = vec![
        BacktranslationRole::new(
            role,
            NarrativeRole::Viewpoint,
            vec![source_range(&bytes, revision_id, 0, 4)],
        )
        .expect("role"),
    ];
    let objects = vec![
        BacktranslationObject::new(
            object,
            SemanticTermCode::new(7).expect("term"),
            vec![source_range(&bytes, revision_id, 16, 19)],
        )
        .expect("object"),
    ];
    let grounded = |id| {
        BacktranslationSection::grounded(vec![field(id, role, vec![object], source)])
            .expect("section")
    };
    let sections = BacktranslationSections {
        causal_events: grounded(1),
        knowledge_changes: grounded(2),
        objects: grounded(3),
        physical_positions: grounded(4),
        dialogue_tactics: BacktranslationSection::abstained(
            BacktranslationAbstentionReason::NoObservableEvidence,
        ),
        resulting_state: grounded(5),
    };
    let proposal = BacktranslationProposal::new(
        source,
        blob("source work"),
        blob("controller model"),
        blob("controller prompt"),
        blob("controller call"),
        blob("ontology"),
        roles,
        objects,
        sections,
    )
    .expect("proposal");
    (proposal, revision_id, bytes)
}

#[test]
fn backtranslation_binds_exact_source_without_surface_names_in_fields() {
    let (proposal, revision_id, bytes) = proposal_fixture();
    proposal
        .verify_source(revision_id, &bytes)
        .expect("exact source verifies");
    assert_eq!(proposal.sections().dialogue_tactics.fields(), &[]);
    assert_eq!(
        proposal.sections().dialogue_tactics.abstention_reason(),
        Some(BacktranslationAbstentionReason::NoObservableEvidence)
    );
    assert!(matches!(
        proposal.verify_source(revision_id, b"substituted bytes"),
        Err(BacktranslationError::SourceBlobMismatch { .. })
    ));

    let serialized = serde_json::to_string(&proposal).expect("serialize");
    assert!(!serialized.contains("Mara"));
    assert!(!serialized.contains("key"));
}

#[test]
fn backtranslation_rejects_undeclared_references_cross_source_evidence_and_forgery() {
    let (proposal, _, _) = proposal_fixture();
    let mut value = serde_json::to_value(&proposal).expect("serialize");
    value["sections"]["causal_events"]["fields"][0]["subject"]["ordinal"] = json!(99);
    assert!(serde_json::from_value::<BacktranslationProposal>(value).is_err());

    let mut value = serde_json::to_value(&proposal).expect("serialize");
    value["unexpected"] = json!(true);
    assert!(serde_json::from_value::<BacktranslationProposal>(value).is_err());

    let mut value = serde_json::to_value(&proposal).expect("serialize");
    value["fingerprint"] = json!(blob("forged"));
    assert!(serde_json::from_value::<BacktranslationProposal>(value).is_err());

    let other_bytes = b"Other source.";
    let other = source_range(other_bytes, RevisionId::new(), 0, other_bytes.len() as u64);
    let invalid_role = BacktranslationRole::new(
        TypedPlaceholder::role(1),
        NarrativeRole::Viewpoint,
        vec![other],
    )
    .expect("role is locally valid");
    let original = proposal.source();
    let result = BacktranslationProposal::new(
        original,
        blob("source work"),
        blob("controller model"),
        blob("controller prompt"),
        blob("controller call"),
        blob("ontology"),
        vec![invalid_role],
        vec![],
        BacktranslationSections {
            causal_events: BacktranslationSection::abstained(
                BacktranslationAbstentionReason::ControllerUnsupported,
            ),
            knowledge_changes: BacktranslationSection::abstained(
                BacktranslationAbstentionReason::ControllerUnsupported,
            ),
            objects: BacktranslationSection::abstained(
                BacktranslationAbstentionReason::ControllerUnsupported,
            ),
            physical_positions: BacktranslationSection::abstained(
                BacktranslationAbstentionReason::ControllerUnsupported,
            ),
            dialogue_tactics: BacktranslationSection::abstained(
                BacktranslationAbstentionReason::ControllerUnsupported,
            ),
            resulting_state: BacktranslationSection::abstained(
                BacktranslationAbstentionReason::ControllerUnsupported,
            ),
        },
    );
    assert_eq!(result, Err(BacktranslationError::EvidenceSourceMismatch));
}

fn audition_case(
    case_label: &str,
    work_label: &str,
    improvement: CausalTransferDecision,
    leakage: LeakageDecision,
) -> BacktranslationAuditionCase {
    audition_case_with(
        case_label,
        work_label,
        improvement,
        leakage,
        AuditionIdentityOverrides::default(),
    )
}

#[derive(Clone, Copy, Default)]
struct AuditionIdentityOverrides {
    writer_model: Option<BlobId>,
    writer_tokenizer: Option<BlobId>,
    prompt: Option<BlobId>,
    call: Option<BlobId>,
    baseline_call: Option<BlobId>,
    evaluator_receipt: Option<BlobId>,
}

fn audition_case_with(
    case_label: &str,
    work_label: &str,
    improvement: CausalTransferDecision,
    leakage: LeakageDecision,
    overrides: AuditionIdentityOverrides,
) -> BacktranslationAuditionCase {
    let source_bytes = format!("Fresh source for {case_label}.");
    let revision_id = RevisionId::new();
    BacktranslationAuditionCase::new(
        TrialCaseId::new(),
        blob(work_label),
        source_range(
            source_bytes.as_bytes(),
            revision_id,
            0,
            source_bytes.len() as u64,
        ),
        overrides
            .writer_model
            .unwrap_or_else(|| blob("writer model")),
        overrides
            .writer_tokenizer
            .unwrap_or_else(|| blob("writer tokenizer")),
        overrides
            .prompt
            .unwrap_or_else(|| blob(&format!("prompt {case_label}"))),
        overrides
            .call
            .unwrap_or_else(|| blob(&format!("call {case_label}"))),
        blob(&format!("raw {case_label}")),
        blob(&format!("selected {case_label}")),
        overrides
            .baseline_call
            .unwrap_or_else(|| blob(&format!("baseline call {case_label}"))),
        blob(&format!("baseline output {case_label}")),
        overrides
            .evaluator_receipt
            .unwrap_or_else(|| blob(&format!("evaluation {case_label}"))),
        improvement,
        leakage,
    )
}

fn rebind_audition_case(
    original: &BacktranslationAuditionCase,
    work_fingerprint: BlobId,
    source: PromptSourceRange,
) -> BacktranslationAuditionCase {
    BacktranslationAuditionCase::new(
        TrialCaseId::new(),
        work_fingerprint,
        source,
        original.writer_model_fingerprint(),
        original.writer_tokenizer_fingerprint(),
        original.prompt_fingerprint(),
        original.call_fingerprint(),
        original.raw_output_blob_id(),
        original.selected_output_blob_id(),
        original.baseline_call_fingerprint(),
        original.baseline_output_blob_id(),
        original.evaluator_receipt_fingerprint(),
        original.improvement(),
        original.leakage(),
    )
}

#[test]
fn audition_requires_work_disjoint_improvement_and_no_leakage() {
    let (proposal, _, _) = proposal_fixture();
    let receipt = BacktranslationAuditionReceipt::new(
        proposal.fingerprint(),
        proposal.source_work_fingerprint(),
        proposal.controller_call_fingerprint(),
        vec![
            audition_case(
                "one",
                "work one",
                CausalTransferDecision::Improved,
                LeakageDecision::Clear,
            ),
            audition_case(
                "two",
                "work two",
                CausalTransferDecision::Improved,
                LeakageDecision::Clear,
            ),
        ],
    )
    .expect("disjoint receipt");
    let receipt_json = serde_json::to_string(&receipt).expect("serialize receipt");
    let replayed: BacktranslationAuditionReceipt =
        serde_json::from_str(&receipt_json).expect("strict receipt replay");
    let auditioned = proposal.audition(replayed).expect("passing audition");
    assert_ne!(
        auditioned.fingerprint(),
        auditioned.proposal().fingerprint()
    );

    let (proposal, _, _) = proposal_fixture();
    let leaky = BacktranslationAuditionReceipt::new(
        proposal.fingerprint(),
        proposal.source_work_fingerprint(),
        proposal.controller_call_fingerprint(),
        vec![
            audition_case(
                "three",
                "work three",
                CausalTransferDecision::Improved,
                LeakageDecision::Detected,
            ),
            audition_case(
                "four",
                "work four",
                CausalTransferDecision::Improved,
                LeakageDecision::Clear,
            ),
        ],
    )
    .expect("structural receipt");
    assert!(matches!(
        proposal.audition(leaky),
        Err(BacktranslationError::AuditionDidNotPass)
    ));
}

#[test]
fn audition_rejects_source_work_and_sibling_work_reuse() {
    let (proposal, _, _) = proposal_fixture();
    let same_source = audition_case(
        "source",
        "source work",
        CausalTransferDecision::Improved,
        LeakageDecision::Clear,
    );
    let other = audition_case(
        "other",
        "work other",
        CausalTransferDecision::Improved,
        LeakageDecision::Clear,
    );
    assert_eq!(
        BacktranslationAuditionReceipt::new(
            proposal.fingerprint(),
            proposal.source_work_fingerprint(),
            proposal.controller_call_fingerprint(),
            vec![same_source, other],
        ),
        Err(BacktranslationError::AuditionReusesSourceWork)
    );

    let first = audition_case(
        "first",
        "repeated work",
        CausalTransferDecision::Improved,
        LeakageDecision::Clear,
    );
    let second = audition_case(
        "second",
        "repeated work",
        CausalTransferDecision::Improved,
        LeakageDecision::Clear,
    );
    assert!(matches!(
        BacktranslationAuditionReceipt::new(
            proposal.fingerprint(),
            proposal.source_work_fingerprint(),
            proposal.controller_call_fingerprint(),
            vec![first, second],
        ),
        Err(BacktranslationError::AuditionReusesWork(_))
    ));

    let first = audition_case(
        "same source one",
        "distinct work one",
        CausalTransferDecision::Improved,
        LeakageDecision::Clear,
    );
    let second = rebind_audition_case(&first, blob("distinct work two"), first.source());
    assert_eq!(
        BacktranslationAuditionReceipt::new(
            proposal.fingerprint(),
            proposal.source_work_fingerprint(),
            proposal.controller_call_fingerprint(),
            vec![first, second],
        ),
        Err(BacktranslationError::AuditionReusesExactSource)
    );

    let source_reuse = rebind_audition_case(
        &audition_case(
            "proposal source",
            "fresh label",
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
        ),
        blob("fresh label"),
        proposal.source(),
    );
    let fresh = audition_case(
        "genuinely fresh",
        "fresh work",
        CausalTransferDecision::Improved,
        LeakageDecision::Clear,
    );
    let receipt = BacktranslationAuditionReceipt::new(
        proposal.fingerprint(),
        proposal.source_work_fingerprint(),
        proposal.controller_call_fingerprint(),
        vec![source_reuse, fresh],
    )
    .expect("receipt structure is otherwise valid");
    assert!(matches!(
        proposal.audition(receipt),
        Err(BacktranslationError::AuditionReusesProposalSource)
    ));
}

#[test]
fn audition_rejects_confounding_or_reused_generation_and_evaluation_evidence() {
    let (proposal, _, _) = proposal_fixture();
    let receipt = |second| {
        BacktranslationAuditionReceipt::new(
            proposal.fingerprint(),
            proposal.source_work_fingerprint(),
            proposal.controller_call_fingerprint(),
            vec![
                audition_case(
                    "control",
                    "control work",
                    CausalTransferDecision::Improved,
                    LeakageDecision::Clear,
                ),
                second,
            ],
        )
    };

    assert_eq!(
        receipt(audition_case_with(
            "binding",
            "binding work",
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
            AuditionIdentityOverrides {
                writer_model: Some(blob("other writer")),
                ..AuditionIdentityOverrides::default()
            },
        )),
        Err(BacktranslationError::AuditionWriterBindingMismatch)
    );
    assert_eq!(
        receipt(audition_case_with(
            "prompt",
            "prompt work",
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
            AuditionIdentityOverrides {
                prompt: Some(blob("prompt control")),
                ..AuditionIdentityOverrides::default()
            },
        )),
        Err(BacktranslationError::AuditionReusesPrompt)
    );
    assert_eq!(
        receipt(audition_case_with(
            "self",
            "self work",
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
            AuditionIdentityOverrides {
                call: Some(blob("same call")),
                baseline_call: Some(blob("same call")),
                ..AuditionIdentityOverrides::default()
            },
        )),
        Err(BacktranslationError::AuditionSelfComparison)
    );
    assert_eq!(
        receipt(audition_case_with(
            "call",
            "call work",
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
            AuditionIdentityOverrides {
                call: Some(blob("call control")),
                ..AuditionIdentityOverrides::default()
            },
        )),
        Err(BacktranslationError::AuditionReusesCall)
    );
    assert_eq!(
        receipt(audition_case_with(
            "judge",
            "judge work",
            CausalTransferDecision::Improved,
            LeakageDecision::Clear,
            AuditionIdentityOverrides {
                evaluator_receipt: Some(blob("evaluation control")),
                ..AuditionIdentityOverrides::default()
            },
        )),
        Err(BacktranslationError::AuditionReusesEvaluatorReceipt)
    );
}

#[test]
fn entity_and_suffix_masks_apply_only_exact_source_bound_transformations() {
    let bytes = b"Mara took the key. Then she waited.";
    let revision_id = RevisionId::new();
    let source = source_range(bytes, revision_id, 0, bytes.len() as u64);
    let entity_plan = SurfacePromptMaskPlan::new(
        source,
        SurfaceMaskKind::Entity,
        vec![SurfaceMaskSpan::new(
            NonEmptyByteRange::new(0, 4).expect("range"),
            SurfaceMaskReplacement::Placeholder {
                placeholder: TypedPlaceholder::role(1),
            },
        )],
    )
    .expect("entity mask");
    let applied = entity_plan
        .apply(revision_id, bytes)
        .expect("exact application");
    assert_eq!(
        applied.rendered_text(),
        "<loom-role:1> took the key. Then she waited."
    );
    assert_eq!(
        applied.rendered_blob_id(),
        BlobId::digest(applied.rendered_bytes())
    );

    let suffix_start = bytes
        .windows(b" Then".len())
        .position(|window| window == b" Then")
        .expect("suffix") as u64;
    let suffix_plan = SurfacePromptMaskPlan::new(
        source,
        SurfaceMaskKind::Suffix,
        vec![SurfaceMaskSpan::new(
            NonEmptyByteRange::new(suffix_start, bytes.len() as u64).expect("range"),
            SurfaceMaskReplacement::Omit,
        )],
    )
    .expect("suffix mask");
    assert_eq!(
        suffix_plan
            .apply(revision_id, bytes)
            .expect("suffix apply")
            .rendered_text(),
        "Mara took the key."
    );
}

#[test]
fn surface_masks_reject_overlap_wrong_replacement_source_swap_and_forgery() {
    let bytes = b"abcdef";
    let revision_id = RevisionId::new();
    let source = source_range(bytes, revision_id, 0, bytes.len() as u64);
    let overlap = SurfacePromptMaskPlan::new(
        source,
        SurfaceMaskKind::Beat,
        vec![
            SurfaceMaskSpan::new(
                NonEmptyByteRange::new(1, 4).expect("range"),
                SurfaceMaskReplacement::Omit,
            ),
            SurfaceMaskSpan::new(
                NonEmptyByteRange::new(3, 5).expect("range"),
                SurfaceMaskReplacement::KindMarker,
            ),
        ],
    );
    assert_eq!(overlap, Err(PromptMaskError::MaskSpansOverlapOrUnordered));

    assert_eq!(
        SurfacePromptMaskPlan::new(
            source,
            SurfaceMaskKind::State,
            vec![SurfaceMaskSpan::new(
                NonEmptyByteRange::new(1, 2).expect("range"),
                SurfaceMaskReplacement::Placeholder {
                    placeholder: TypedPlaceholder::role(1),
                },
            )],
        ),
        Err(PromptMaskError::PlaceholderRequiresEntityMask)
    );

    let plan = SurfacePromptMaskPlan::new(
        source,
        SurfaceMaskKind::ContentStyle,
        vec![SurfaceMaskSpan::new(
            NonEmptyByteRange::new(1, 2).expect("range"),
            SurfaceMaskReplacement::KindMarker,
        )],
    )
    .expect("plan");
    assert!(matches!(
        plan.clone().apply(revision_id, b"ABCDEF"),
        Err(PromptMaskError::SourceBlobMismatch { .. })
    ));
    let mut value = serde_json::to_value(plan).expect("serialize");
    value["fingerprint"] = json!(blob("forged mask"));
    assert!(serde_json::from_value::<SurfacePromptMaskPlan>(value).is_err());
}

#[test]
fn fim_requires_exact_model_tokenizer_and_capability_binding() {
    let bytes = b"prefix missing suffix";
    let revision_id = RevisionId::new();
    let source = source_range(bytes, revision_id, 0, bytes.len() as u64);
    let model = blob("model");
    let tokenizer = blob("tokenizer");
    let capability = blob("fim capability");
    let plan = ModelSpecificFimMaskPlan::new(
        source,
        NonEmptyByteRange::new(7, 14).expect("missing"),
        model,
        tokenizer,
        capability,
    )
    .expect("fim plan");
    let receipt = FimCapabilityReceipt::new(
        model,
        tokenizer,
        capability,
        vec![1],
        vec![2],
        vec![3],
        blob("backend receipt"),
    )
    .expect("receipt");
    let bound = plan.clone().bind_capability(receipt).expect("exact bind");
    assert_eq!(bound.plan().model_fingerprint(), model);
    bound
        .verify_source(revision_id, bytes)
        .expect("exact FIM source");
    assert!(matches!(
        bound.verify_source(revision_id, b"substituted source"),
        Err(PromptMaskError::SourceBlobMismatch { .. })
    ));

    let wrong = FimCapabilityReceipt::new(
        model,
        blob("other tokenizer"),
        capability,
        vec![1],
        vec![2],
        vec![3],
        blob("backend receipt"),
    )
    .expect("receipt");
    assert!(matches!(
        plan.bind_capability(wrong),
        Err(PromptMaskError::FimCapabilityMismatch)
    ));
}

#[test]
fn paragraph_endpoints_are_exact_prefixes_with_replayable_boundaries() {
    let raw = b"First paragraph.\n\nSecond paragraph.\n";
    let extraction = ParagraphEndpointExtraction::extract(raw, raw.len() as u64, None)
        .expect("endpoint extraction");
    assert_eq!(extraction.candidates().len(), 2);
    assert_eq!(
        extraction.candidates()[0].raw_prefix_range(),
        NonEmptyByteRange::new(0, 16).expect("range")
    );
    assert!(matches!(
        extraction.candidates()[0].boundary(),
        ParagraphBoundaryWitness::BlankLine { .. }
    ));
    assert!(matches!(
        extraction.candidates()[1].boundary(),
        ParagraphBoundaryWitness::TerminalLineBreak { .. }
    ));
    extraction
        .replay_against_raw(raw)
        .expect("strict replay succeeds");
    let selected = extraction.select(0).expect("selection");
    assert_eq!(
        selected.selected_bytes(raw).expect("selected bytes"),
        b"First paragraph."
    );
}

#[test]
fn endpoint_extraction_preserves_stop_suffix_and_never_invents_a_sentinel() {
    let raw = b"First.\n\nSTOP";
    let stop = TrimmedStopSuffixWitness::from_raw(
        raw,
        NonEmptyByteRange::new(8, raw.len() as u64).expect("stop range"),
        blob("stop rule"),
    )
    .expect("stop witness");
    let extraction =
        ParagraphEndpointExtraction::extract(raw, 64, Some(stop)).expect("endpoint extraction");
    assert_eq!(extraction.candidates().len(), 1);
    assert_eq!(extraction.trimmed_stop_suffix(), Some(stop));
    assert_eq!(
        extraction
            .clone()
            .select(0)
            .expect("selection")
            .selected_bytes(raw)
            .expect("bytes"),
        b"First."
    );

    let no_boundary = ParagraphEndpointExtraction::extract(b"unfinished", 64, None)
        .expect("valid empty candidate set");
    assert!(no_boundary.candidates().is_empty());
}

#[test]
fn endpoint_replay_rejects_altered_raw_json_forgery_and_unknown_fields() {
    let raw = b"One.\n\nTwo.\n";
    let extraction =
        ParagraphEndpointExtraction::extract(raw, 64, None).expect("endpoint extraction");
    assert!(matches!(
        extraction.replay_against_raw(b"Altered.\n\nTwo.\n"),
        Err(EndpointError::ReplayMismatch)
    ));

    let mut value = serde_json::to_value(&extraction).expect("serialize");
    value["fingerprint"] = json!(blob("forged endpoint"));
    assert!(serde_json::from_value::<ParagraphEndpointExtraction>(value).is_err());

    let mut value = serde_json::to_value(&extraction).expect("serialize");
    value["unexpected"] = Value::Bool(true);
    assert!(serde_json::from_value::<ParagraphEndpointExtraction>(value).is_err());
}
