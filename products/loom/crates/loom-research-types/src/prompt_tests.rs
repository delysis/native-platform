use loom_types::{BlobId, ProjectId, RevisionId};
use serde_json::json;

use crate::*;

fn scope() -> CallScope {
    CallScope::new(
        CampaignId::new(),
        StageId::new(),
        StageAttemptId::new(),
        TrialCaseId::new(),
    )
}

fn source_range(revision_id: RevisionId, source: &[u8], start: u64, end: u64) -> PromptSourceRange {
    PromptSourceRange::new(
        revision_id,
        BlobId::digest(source),
        NonEmptyByteRange::new(start, end).expect("nonempty source range"),
    )
    .expect("bounded source range")
}

fn exact_block(
    role: CompletionPromptBlockRole,
    revision_id: RevisionId,
    source: &[u8],
    start: u64,
    end: u64,
) -> FrozenCompletionPromptBlock {
    let range = source_range(revision_id, source, start, end);
    let bytes = range
        .range()
        .checked_str(source)
        .expect("valid source excerpt")
        .as_bytes()
        .to_vec();
    FrozenCompletionPromptBlock::new(
        role,
        ExactPromptBlockBytes::new(bytes).expect("nonempty block"),
        PromptBlockWitness::exact_source(range),
    )
    .expect("valid exact-source block")
}

fn tail(
    origin: CompletionTailOrigin,
    revision_id: RevisionId,
    source: &[u8],
    start: u64,
) -> CompletionPromptTail {
    let range =
        NonEmptyByteRange::new(start, source.len() as u64).expect("nonempty tail byte range");
    match origin {
        CompletionTailOrigin::LiveManuscript => {
            CompletionPromptTail::live_manuscript(revision_id, BlobId::digest(source), range)
        }
        CompletionTailOrigin::AdmittedAssembly { assembly_id } => {
            CompletionPromptTail::admitted_assembly(assembly_id, BlobId::digest(source), range)
        }
    }
    .expect("bounded tail")
}

struct FingerprintFixture<'a> {
    revision: RevisionId,
    source: &'a [u8],
    tail_start: u64,
}

impl FingerprintFixture<'_> {
    fn compile(
        &self,
        project_id: ProjectId,
        call_scope: CallScope,
        recipe: BlobId,
        blocks: Vec<FrozenCompletionPromptBlock>,
        origin: CompletionTailOrigin,
    ) -> BlobId {
        let mut bytes = Vec::new();
        for block in &blocks {
            bytes.extend_from_slice(block.bytes().as_bytes());
        }
        bytes.extend_from_slice(
            NonEmptyByteRange::new(self.tail_start, self.source.len() as u64)
                .expect("nonempty tail")
                .as_range()
                .checked_slice(self.source)
                .expect("valid tail"),
        );
        let exact_sources = match origin {
            CompletionTailOrigin::LiveManuscript => {
                vec![ExactPromptSource::new(self.revision, self.source)]
            }
            CompletionTailOrigin::AdmittedAssembly { assembly_id } => vec![
                ExactPromptSource::new(self.revision, self.source),
                ExactPromptSource::admitted_assembly(assembly_id, self.source),
            ],
        };
        FrozenBaseCompletionPrompt::new(
            project_id,
            call_scope,
            recipe,
            blocks,
            tail(origin, self.revision, self.source, self.tail_start),
        )
        .expect("specification")
        .compile(bytes, &exact_sources)
        .expect("compiled prompt")
        .fingerprint()
    }
}

#[test]
fn raw_completion_compiles_exact_ordered_blocks_and_final_tail() {
    let project_id = ProjectId::new();
    let call_scope = scope();
    let recipe = BlobId::digest(b"natural-bookfront-plus-project-anchor-v1");
    let book_revision = RevisionId::new();
    let bookfront = "THE GLASS OBSERVATORY\r\n\r\n".as_bytes();
    let anchor_revision = RevisionId::new();
    let anchor = "Her sentences stayed close to touch and consequence.\n\n".as_bytes();
    let manuscript_revision = RevisionId::new();
    let manuscript = "Earlier paragraph.\n\nMara kept her hand on the latch.".as_bytes();

    let blocks = vec![
        exact_block(
            CompletionPromptBlockRole::Bookfront,
            book_revision,
            bookfront,
            0,
            bookfront.len() as u64,
        ),
        exact_block(
            CompletionPromptBlockRole::ProjectAnchor,
            anchor_revision,
            anchor,
            0,
            anchor.len() as u64,
        ),
    ];
    let prompt_tail = tail(
        CompletionTailOrigin::LiveManuscript,
        manuscript_revision,
        manuscript,
        "Earlier paragraph.\n\n".len() as u64,
    );
    let specification =
        FrozenBaseCompletionPrompt::new(project_id, call_scope, recipe, blocks, prompt_tail)
            .expect("valid frozen prompt specification");

    let tail_bytes = prompt_tail
        .range()
        .as_range()
        .checked_slice(manuscript)
        .expect("exact live tail");
    let mut expected = Vec::new();
    expected.extend_from_slice(bookfront);
    expected.extend_from_slice(anchor);
    expected.extend_from_slice(tail_bytes);
    let exact_sources = [
        ExactPromptSource::new(book_revision, bookfront),
        ExactPromptSource::new(anchor_revision, anchor),
        ExactPromptSource::new(manuscript_revision, manuscript),
    ];

    let serialized = serde_json::to_vec(&specification).expect("serialize specification");
    let restored: FrozenBaseCompletionPrompt =
        serde_json::from_slice(&serialized).expect("validated specification round trip");
    assert_eq!(restored, specification);
    assert_eq!(restored.preceding_blocks()[0].bytes().as_bytes(), bookfront);

    let compiled = specification
        .clone()
        .compile(expected.clone(), &exact_sources)
        .expect("exact raw completion prompt");
    let recompiled = restored
        .compile(expected.clone(), &exact_sources)
        .expect("same frozen prompt compiles identically");
    assert_eq!(compiled.project_id(), project_id);
    assert_eq!(compiled.scope(), call_scope);
    assert_eq!(compiled.treatment_recipe_fingerprint(), recipe);
    assert_compiled_prompt_contract(&compiled, recompiled, &specification, &expected, tail_bytes);
}

fn assert_compiled_prompt_contract(
    compiled: &CompiledBaseCompletionPrompt,
    recompiled: CompiledBaseCompletionPrompt,
    specification: &FrozenBaseCompletionPrompt,
    expected: &[u8],
    tail_bytes: &[u8],
) {
    assert_eq!(compiled.exact_bytes(), expected);
    assert_eq!(compiled.exact_text().as_bytes(), expected);
    assert_eq!(compiled.fingerprint(), recompiled.fingerprint());
    assert_eq!(
        compiled.content_fingerprint(),
        recompiled.content_fingerprint()
    );
    compiled
        .specification()
        .verify_compiled_content_evidence(
            compiled.exact_bytes(),
            compiled.tail_prompt_range(),
            compiled.content_fingerprint(),
        )
        .expect("retry-stable content evidence replays");
    assert_eq!(
        compiled.specification().verify_compiled_content_evidence(
            compiled.exact_bytes(),
            compiled.tail_prompt_range(),
            compiled.fingerprint(),
        ),
        Err(PromptCompileError::CompiledEvidenceFingerprintMismatch)
    );
    assert_eq!(
        compiled
            .tail_prompt_range()
            .as_range()
            .checked_slice(compiled.exact_bytes())
            .expect("compiled tail range"),
        tail_bytes
    );
    assert!(compiled.exact_bytes().ends_with(tail_bytes));
    assert_eq!(
        compiled.specification().preceding_blocks()[0].role(),
        CompletionPromptBlockRole::Bookfront
    );
    assert_eq!(
        compiled.specification().preceding_blocks()[1].role(),
        CompletionPromptBlockRole::ProjectAnchor
    );
    let recompiled_fingerprint = recompiled.fingerprint();
    let recompiled_content_fingerprint = recompiled.content_fingerprint();
    let (
        preserved_specification,
        preserved_bytes,
        preserved_tail,
        preserved_content_fingerprint,
        preserved_fingerprint,
    ) = recompiled.into_parts();
    assert_eq!(&preserved_specification, specification);
    assert_eq!(preserved_bytes, expected);
    assert_eq!(preserved_tail, compiled.tail_prompt_range());
    assert_eq!(
        preserved_content_fingerprint,
        recompiled_content_fingerprint
    );
    assert_eq!(preserved_fingerprint, recompiled_fingerprint);
}

#[test]
fn prompt_content_identity_excludes_only_the_attempt() {
    let project_id = ProjectId::new();
    let campaign_id = CampaignId::new();
    let stage_id = StageId::new();
    let case_id = TrialCaseId::new();
    let recipe = BlobId::digest(b"retry-stable treatment");
    let revision = RevisionId::new();
    let source = b"The latch lifted under Mara's hand.";
    let tail = CompletionPromptTail::live_manuscript(
        revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(0, source.len() as u64).expect("nonempty tail"),
    )
    .expect("bounded tail");
    let compile = |attempt_id, case_id| {
        FrozenBaseCompletionPrompt::new(
            project_id,
            CallScope::new(campaign_id, stage_id, attempt_id, case_id),
            recipe,
            Vec::new(),
            tail,
        )
        .expect("retry prompt specification")
        .compile(source.to_vec(), &[ExactPromptSource::new(revision, source)])
        .expect("source-bound retry prompt")
    };

    let first = compile(StageAttemptId::new(), case_id);
    let retry = compile(StageAttemptId::new(), case_id);
    let wrong_case = compile(StageAttemptId::new(), TrialCaseId::new());
    assert_ne!(first.scope().attempt_id(), retry.scope().attempt_id());
    assert_ne!(first.fingerprint(), retry.fingerprint());
    assert_eq!(first.content_fingerprint(), retry.content_fingerprint());
    assert_ne!(
        first.content_fingerprint(),
        wrong_case.content_fingerprint()
    );
}

#[test]
fn admitted_assembly_tail_is_bound_by_assembly_without_a_revision_claim() {
    let project_id = ProjectId::new();
    let assembly_id = CandidateAssemblyId::new();
    let assembled = b"A model-authored movement.\n\nThe next movement begins here.";
    let range = NonEmptyByteRange::new(27, assembled.len() as u64).expect("assembly tail range");
    let tail =
        CompletionPromptTail::admitted_assembly(assembly_id, BlobId::digest(assembled), range)
            .expect("bounded assembly tail");
    assert_eq!(tail.source_revision_id(), None);
    assert_eq!(tail.assembly_id(), Some(assembly_id));

    let tail_bytes = range
        .as_range()
        .checked_slice(assembled)
        .expect("exact assembly tail")
        .to_vec();
    let specification = FrozenBaseCompletionPrompt::new(
        project_id,
        scope(),
        BlobId::digest(b"assembly-continuation-v1"),
        Vec::new(),
        tail,
    )
    .expect("assembly prompt specification");
    let compiled = specification
        .clone()
        .compile(
            tail_bytes,
            &[ExactPromptSource::admitted_assembly(assembly_id, assembled)],
        )
        .expect("assembly-bound prompt");
    assert_eq!(compiled.specification(), &specification);

    let mut serialized = serde_json::to_value(&specification).expect("serialize specification");
    let serialized_tail = serialized
        .get("tail")
        .and_then(serde_json::Value::as_object)
        .expect("tagged tail object");
    assert!(!serialized_tail.contains_key("source_revision_id"));
    assert_eq!(
        serialized_tail
            .get("assembly_id")
            .and_then(serde_json::Value::as_str),
        Some(assembly_id.to_string().as_str())
    );

    serialized
        .get_mut("tail")
        .and_then(serde_json::Value::as_object_mut)
        .expect("tagged tail object")
        .insert(
            "source_revision_id".to_owned(),
            json!(RevisionId::new().to_string()),
        );
    assert!(serde_json::from_value::<FrozenBaseCompletionPrompt>(serialized).is_err());
}

#[test]
fn frozen_prompt_replay_rejects_byte_range_suffix_and_fingerprint_tampering() {
    let revision_id = RevisionId::new();
    let source = b"Mara kept her hand on the latch.";
    let specification = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"direct-continuation-v1"),
        Vec::new(),
        tail(CompletionTailOrigin::LiveManuscript, revision_id, source, 0),
    )
    .expect("frozen direct-continuation prompt");
    let compiled = specification
        .clone()
        .compile(
            source.to_vec(),
            &[ExactPromptSource::new(revision_id, source)],
        )
        .expect("source-bound compilation");
    specification
        .verify_compiled_evidence(
            compiled.exact_bytes(),
            compiled.tail_prompt_range(),
            compiled.fingerprint(),
        )
        .expect("exact compiled evidence");

    let mut changed = source.to_vec();
    changed[0] = b"m"[0];
    assert_eq!(
        specification
            .verify_compiled_evidence(
                &changed,
                compiled.tail_prompt_range(),
                compiled.fingerprint(),
            )
            .unwrap_err(),
        PromptCompileError::CompiledEvidenceFingerprintMismatch
    );
    assert_eq!(
        specification
            .verify_compiled_evidence(
                source,
                NonEmptyByteRange::new(1, source.len() as u64).expect("shifted tail"),
                compiled.fingerprint(),
            )
            .unwrap_err(),
        PromptCompileError::PromptBytesMismatch
    );
    let mut suffixed = source.to_vec();
    suffixed.push(b"x"[0]);
    assert!(matches!(
        specification.verify_compiled_evidence(
            &suffixed,
            compiled.tail_prompt_range(),
            compiled.fingerprint(),
        ),
        Err(PromptCompileError::ExtraSuffix { extra_bytes: 1 })
    ));
    assert_eq!(
        specification
            .verify_compiled_evidence(
                source,
                compiled.tail_prompt_range(),
                BlobId::digest(b"wrong compiled prompt fingerprint"),
            )
            .unwrap_err(),
        PromptCompileError::CompiledEvidenceFingerprintMismatch
    );
}

#[test]
fn transformed_block_preserves_sources_and_receipt_fingerprints() {
    let source_revision = RevisionId::new();
    let source = b"Mara learns the hatch is locked; the alarm remains unexplained.";
    let rendered = b"The hatch would not move. Behind her, the alarm kept its silence.\n\n";
    let source_ref = source_range(source_revision, source, 0, source.len() as u64);
    let recipe = BlobId::digest(b"event-ledger-to-prose-v2");
    let receipt = BlobId::digest(b"transformation-receipt-0001");
    let witness = PromptBlockWitness::transformation(
        vec![source_ref],
        recipe,
        receipt,
        BlobId::digest(rendered),
    )
    .expect("grounded transformation witness");
    let block = FrozenCompletionPromptBlock::new(
        CompletionPromptBlockRole::OperatorDemonstration,
        ExactPromptBlockBytes::new(rendered.to_vec()).expect("rendered UTF-8"),
        witness,
    )
    .expect("fingerprint-bound transformed block");
    assert!(block.witness().is_transformation());
    assert_eq!(block.witness().sources(), &[source_ref]);
    assert_eq!(block.witness().recipe_fingerprint(), Some(recipe));
    assert_eq!(block.witness().receipt_fingerprint(), Some(receipt));

    let live_revision = RevisionId::new();
    let live = b"Mara listened.";
    let specification = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"treatment"),
        vec![block],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid specification");
    let mut prompt = rendered.to_vec();
    prompt.extend_from_slice(live);
    specification
        .compile(
            prompt,
            &[
                ExactPromptSource::new(source_revision, source),
                ExactPromptSource::new(live_revision, live),
            ],
        )
        .expect("transformed source metadata verifies");

    let bad_witness = PromptBlockWitness::transformation(
        vec![source_ref],
        recipe,
        receipt,
        BlobId::digest(b"different output"),
    )
    .expect("structural witness");
    assert_eq!(
        FrozenCompletionPromptBlock::new(
            CompletionPromptBlockRole::OperatorDemonstration,
            ExactPromptBlockBytes::new(rendered.to_vec()).expect("rendered UTF-8"),
            bad_witness,
        )
        .unwrap_err(),
        PromptCompileError::RenderedBytesFingerprintMismatch
    );
    assert_eq!(
        PromptBlockWitness::transformation(
            vec![source_ref, source_ref],
            recipe,
            receipt,
            BlobId::digest(rendered),
        )
        .unwrap_err(),
        PromptCompileError::DuplicateWitnessSource
    );
}

#[test]
fn undeclared_chat_and_fim_roles_or_fields_fail_deserialization() {
    let source_revision = RevisionId::new();
    let source = b"A title\n\n";
    let live_revision = RevisionId::new();
    let live = b"The manuscript tail.";
    let specification = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![exact_block(
            CompletionPromptBlockRole::Bookfront,
            source_revision,
            source,
            0,
            source.len() as u64,
        )],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid specification");

    for undeclared in ["system", "assistant", "fim_prefix", "fim_suffix"] {
        let mut value = serde_json::to_value(&specification).expect("specification value");
        value["preceding_blocks"][0]["role"] = json!(undeclared);
        assert!(
            serde_json::from_value::<FrozenBaseCompletionPrompt>(value).is_err(),
            "role {undeclared} must not enter base completion"
        );
    }

    let mut hidden = serde_json::to_value(&specification).expect("specification value");
    hidden["chat_template"] = json!("<system>{control}</system>");
    assert!(serde_json::from_value::<FrozenBaseCompletionPrompt>(hidden).is_err());

    let mut missing_witness = serde_json::to_value(specification).expect("specification value");
    missing_witness["preceding_blocks"][0]
        .as_object_mut()
        .expect("block object")
        .remove("witness");
    assert!(serde_json::from_value::<FrozenBaseCompletionPrompt>(missing_witness).is_err());
}

#[test]
fn compiler_rejects_mismatched_block_and_blob() {
    let block_revision = RevisionId::new();
    let source = "AéZ".as_bytes();
    let live_revision = RevisionId::new();
    let live = b"Live tail";
    let exact_sources = [
        ExactPromptSource::new(block_revision, source),
        ExactPromptSource::new(live_revision, live),
    ];

    let mismatched_block = FrozenCompletionPromptBlock::new(
        CompletionPromptBlockRole::SourceApprenticeship,
        ExactPromptBlockBytes::from_text("different bytes").expect("UTF-8 block"),
        PromptBlockWitness::exact_source(source_range(block_revision, source, 0, 1)),
    )
    .expect("structurally valid block");
    let spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![mismatched_block],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid structure");
    let mut prompt = b"different bytes".to_vec();
    prompt.extend_from_slice(live);
    assert_eq!(
        spec.compile(prompt, &exact_sources).unwrap_err(),
        PromptCompileError::BlockSourceBytesMismatch { block_index: 0 }
    );

    let wrong_blob_ref = PromptSourceRange::new(
        block_revision,
        BlobId::digest(b"wrong source"),
        NonEmptyByteRange::new(0, 1).expect("nonempty range"),
    )
    .expect("bounded reference");
    let wrong_blob_block = FrozenCompletionPromptBlock::new(
        CompletionPromptBlockRole::SourceApprenticeship,
        ExactPromptBlockBytes::from_text("A").expect("UTF-8 block"),
        PromptBlockWitness::exact_source(wrong_blob_ref),
    )
    .expect("structural block");
    let spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![wrong_blob_block],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid structure");
    let mut prompt = b"A".to_vec();
    prompt.extend_from_slice(live);
    assert!(matches!(
        spec.compile(prompt, &exact_sources),
        Err(PromptCompileError::SourceBlobMismatch { .. })
    ));
}

#[test]
fn compiler_rejects_split_and_out_of_bounds_source_ranges() {
    let block_revision = RevisionId::new();
    let source = "AéZ".as_bytes();
    let live_revision = RevisionId::new();
    let live = b"Live tail";
    let exact_sources = [
        ExactPromptSource::new(block_revision, source),
        ExactPromptSource::new(live_revision, live),
    ];
    let split_ref = source_range(block_revision, source, 2, 3);
    let split_block = FrozenCompletionPromptBlock::new(
        CompletionPromptBlockRole::SourceApprenticeship,
        ExactPromptBlockBytes::from_text("x").expect("UTF-8 block"),
        PromptBlockWitness::exact_source(split_ref),
    )
    .expect("structural block");
    let spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![split_block],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid structure");
    let mut prompt = b"x".to_vec();
    prompt.extend_from_slice(live);
    assert!(matches!(
        spec.compile(prompt, &exact_sources),
        Err(PromptCompileError::SourceRange {
            error: RangeError::SplitsUtf8 { offset: 2 },
            ..
        })
    ));

    let out_of_bounds_ref = PromptSourceRange::new(
        block_revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(1, 8).expect("nonempty structural range"),
    )
    .expect("range remains below the global source bound");
    let out_of_bounds_block = FrozenCompletionPromptBlock::new(
        CompletionPromptBlockRole::SourceApprenticeship,
        ExactPromptBlockBytes::from_text("x").expect("UTF-8 block"),
        PromptBlockWitness::exact_source(out_of_bounds_ref),
    )
    .expect("structural block");
    let spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![out_of_bounds_block],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid structure");
    let mut prompt = b"x".to_vec();
    prompt.extend_from_slice(live);
    assert!(matches!(
        spec.compile(prompt, &exact_sources),
        Err(PromptCompileError::SourceRange {
            error: RangeError::OutOfBounds { .. },
            ..
        })
    ));
}

#[test]
fn compiler_rejects_missing_duplicate_unused_and_invalid_source_inputs() {
    let block_revision = RevisionId::new();
    let source = "AéZ".as_bytes();
    let live_revision = RevisionId::new();
    let live = b"Live tail";
    let valid_block = exact_block(
        CompletionPromptBlockRole::SourceApprenticeship,
        block_revision,
        source,
        0,
        1,
    );
    let spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![valid_block],
        tail(CompletionTailOrigin::LiveManuscript, live_revision, live, 0),
    )
    .expect("valid structure");
    let mut prompt = b"A".to_vec();
    prompt.extend_from_slice(live);
    assert_eq!(
        spec.clone()
            .compile(
                prompt.clone(),
                &[ExactPromptSource::new(block_revision, source)]
            )
            .unwrap_err(),
        PromptCompileError::MissingSourceBinding(live_revision)
    );
    assert_eq!(
        spec.clone()
            .compile(
                prompt.clone(),
                &[
                    ExactPromptSource::new(block_revision, source),
                    ExactPromptSource::new(block_revision, source),
                    ExactPromptSource::new(live_revision, live),
                ],
            )
            .unwrap_err(),
        PromptCompileError::DuplicateSourceBinding(block_revision)
    );
    assert_eq!(
        spec.clone()
            .compile(
                vec![0xff],
                &[
                    ExactPromptSource::new(block_revision, source),
                    ExactPromptSource::new(live_revision, live),
                ],
            )
            .unwrap_err(),
        PromptCompileError::InvalidPromptUtf8
    );
    let unused_revision = RevisionId::new();
    assert_eq!(
        spec.compile(
            prompt,
            &[
                ExactPromptSource::new(block_revision, source),
                ExactPromptSource::new(live_revision, live),
                ExactPromptSource::new(unused_revision, b"unused"),
            ],
        )
        .unwrap_err(),
        PromptCompileError::UnexpectedSourceBinding(unused_revision)
    );
}

#[test]
fn final_tail_must_be_exact_source_suffix_and_prompt_must_have_no_suffix() {
    let revision = RevisionId::new();
    let source = "AéZ".as_bytes();
    let project = ProjectId::new();
    let call_scope = scope();
    let recipe = BlobId::digest(b"direct continuation");

    let middle_not_tail = CompletionPromptTail::live_manuscript(
        revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(0, 3).expect("middle range"),
    )
    .expect("bounded range");
    let spec =
        FrozenBaseCompletionPrompt::new(project, call_scope, recipe, vec![], middle_not_tail)
            .expect("valid structure");
    assert_eq!(
        spec.compile(
            source[..3].to_vec(),
            &[ExactPromptSource::new(revision, source)]
        )
        .unwrap_err(),
        PromptCompileError::TailNotAtSourceEnd {
            range_end: 3,
            source_end: 4,
        }
    );

    let split_tail = CompletionPromptTail::live_manuscript(
        revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(2, 4).expect("structural range"),
    )
    .expect("bounded range");
    let spec = FrozenBaseCompletionPrompt::new(project, call_scope, recipe, vec![], split_tail)
        .expect("valid structure");
    assert!(matches!(
        spec.compile(
            source[2..].to_vec(),
            &[ExactPromptSource::new(revision, source)]
        ),
        Err(PromptCompileError::SourceRange {
            error: RangeError::SplitsUtf8 { offset: 2 },
            ..
        })
    ));

    let complete_tail = tail(CompletionTailOrigin::LiveManuscript, revision, source, 0);
    let spec = FrozenBaseCompletionPrompt::new(project, call_scope, recipe, vec![], complete_tail)
        .expect("valid direct continuation");
    let mut suffixed = source.to_vec();
    suffixed.extend_from_slice(b"<|fim_suffix|>");
    assert_eq!(
        spec.clone()
            .compile(suffixed, &[ExactPromptSource::new(revision, source)])
            .unwrap_err(),
        PromptCompileError::ExtraSuffix { extra_bytes: 14 }
    );
    let mut prefixed = b"<|assistant|>".to_vec();
    prefixed.extend_from_slice(source);
    assert_eq!(
        spec.compile(prefixed, &[ExactPromptSource::new(revision, source)])
            .unwrap_err(),
        PromptCompileError::PromptBytesMismatch
    );
}

#[test]
fn empty_and_invalid_prompt_inputs_fail_closed() {
    assert_eq!(
        ExactPromptBlockBytes::new(vec![]).unwrap_err(),
        PromptCompileError::EmptyBlock
    );
    assert_eq!(
        ExactPromptBlockBytes::new(vec![0xff]).unwrap_err(),
        PromptCompileError::InvalidBlockUtf8
    );
    assert!(matches!(
        ExactPromptBlockBytes::new(vec![b'x'; MAX_COMPLETION_PROMPT_BLOCK_BYTES + 1]),
        Err(PromptCompileError::BlockTooLarge { .. })
    ));

    assert_eq!(
        NonEmptyByteRange::new(0, 0),
        Err(RangeError::Empty),
        "the strict base-completion form has no blank-tail exception"
    );

    let revision = RevisionId::new();
    let source = b"x";
    let nonempty_spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"direct continuation"),
        vec![],
        tail(CompletionTailOrigin::LiveManuscript, revision, source, 0),
    )
    .expect("nonempty strict tail");
    assert_eq!(
        nonempty_spec
            .compile(vec![], &[ExactPromptSource::new(revision, source)])
            .unwrap_err(),
        PromptCompileError::PromptBytesMismatch
    );
}

#[test]
fn excessive_prompt_inputs_fail_closed() {
    let revision = RevisionId::new();
    let source = b"x";
    let source_ref = source_range(revision, source, 0, 1);
    let block = FrozenCompletionPromptBlock::new(
        CompletionPromptBlockRole::Bookfront,
        ExactPromptBlockBytes::from_text("x").expect("block"),
        PromptBlockWitness::exact_source(source_ref),
    )
    .expect("block");
    let too_many = vec![block.clone(); MAX_COMPLETION_PROMPT_BLOCKS + 1];
    assert_eq!(
        FrozenBaseCompletionPrompt::new(
            ProjectId::new(),
            scope(),
            BlobId::digest(b"recipe"),
            too_many,
            tail(CompletionTailOrigin::LiveManuscript, revision, source, 0),
        )
        .unwrap_err(),
        PromptCompileError::Bound(BoundError::TooMany {
            actual: MAX_COMPLETION_PROMPT_BLOCKS + 1,
            maximum: MAX_COMPLETION_PROMPT_BLOCKS,
        })
    );

    let large_blocks = (0..=MAX_COMPLETION_PROMPT_BYTES / MAX_COMPLETION_PROMPT_BLOCK_BYTES)
        .map(|_| {
            FrozenCompletionPromptBlock::new(
                CompletionPromptBlockRole::Bookfront,
                ExactPromptBlockBytes::new(vec![b'x'; MAX_COMPLETION_PROMPT_BLOCK_BYTES])
                    .expect("maximum-size block"),
                PromptBlockWitness::exact_source(source_ref),
            )
            .expect("structural block")
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        FrozenBaseCompletionPrompt::new(
            ProjectId::new(),
            scope(),
            BlobId::digest(b"recipe"),
            large_blocks,
            tail(CompletionTailOrigin::LiveManuscript, revision, source, 0),
        ),
        Err(PromptCompileError::PromptTooLarge { .. })
    ));

    let spec = FrozenBaseCompletionPrompt::new(
        ProjectId::new(),
        scope(),
        BlobId::digest(b"recipe"),
        vec![],
        tail(CompletionTailOrigin::LiveManuscript, revision, source, 0),
    )
    .expect("direct continuation");
    let bindings = (0..=MAX_PROMPT_SOURCE_BINDINGS)
        .map(|_| ExactPromptSource::new(RevisionId::new(), source))
        .collect::<Vec<_>>();
    assert_eq!(
        spec.compile(source.to_vec(), &bindings).unwrap_err(),
        PromptCompileError::TooManySourceBindings {
            actual: MAX_PROMPT_SOURCE_BINDINGS + 1,
            maximum: MAX_PROMPT_SOURCE_BINDINGS,
        }
    );
}

#[test]
fn canonical_fingerprint_binds_project_scope_and_recipe() {
    let revision = RevisionId::new();
    let source = b"ABtail";
    let fixture = FingerprintFixture {
        revision,
        source,
        tail_start: 2,
    };
    let project = ProjectId::new();
    let call_scope = scope();
    let recipe = BlobId::digest(b"recipe-a");
    let blocks = vec![
        exact_block(CompletionPromptBlockRole::Bookfront, revision, source, 0, 1),
        exact_block(
            CompletionPromptBlockRole::ProjectAnchor,
            revision,
            source,
            1,
            2,
        ),
    ];
    let baseline = fixture.compile(
        project,
        call_scope,
        recipe,
        blocks.clone(),
        CompletionTailOrigin::LiveManuscript,
    );
    for changed in [
        fixture.compile(
            ProjectId::new(),
            call_scope,
            recipe,
            blocks.clone(),
            CompletionTailOrigin::LiveManuscript,
        ),
        fixture.compile(
            project,
            scope(),
            recipe,
            blocks.clone(),
            CompletionTailOrigin::LiveManuscript,
        ),
        fixture.compile(
            project,
            call_scope,
            BlobId::digest(b"recipe-b"),
            blocks,
            CompletionTailOrigin::LiveManuscript,
        ),
    ] {
        assert_ne!(baseline, changed);
    }
}

#[test]
fn canonical_fingerprint_binds_role_order_and_tail_origin() {
    let revision = RevisionId::new();
    let source = b"ABtail";
    let fixture = FingerprintFixture {
        revision,
        source,
        tail_start: 2,
    };
    let project = ProjectId::new();
    let call_scope = scope();
    let recipe = BlobId::digest(b"recipe-a");
    let first = exact_block(CompletionPromptBlockRole::Bookfront, revision, source, 0, 1);
    let second = exact_block(
        CompletionPromptBlockRole::ProjectAnchor,
        revision,
        source,
        1,
        2,
    );
    let baseline = fixture.compile(
        project,
        call_scope,
        recipe,
        vec![first.clone(), second.clone()],
        CompletionTailOrigin::LiveManuscript,
    );
    let reordered = fixture.compile(
        project,
        call_scope,
        recipe,
        vec![second.clone(), first],
        CompletionTailOrigin::LiveManuscript,
    );
    let changed_role = fixture.compile(
        project,
        call_scope,
        recipe,
        vec![
            exact_block(
                CompletionPromptBlockRole::MovementContract,
                revision,
                source,
                0,
                1,
            ),
            second.clone(),
        ],
        CompletionTailOrigin::LiveManuscript,
    );
    let changed_origin = fixture.compile(
        project,
        call_scope,
        recipe,
        vec![
            exact_block(CompletionPromptBlockRole::Bookfront, revision, source, 0, 1),
            second,
        ],
        CompletionTailOrigin::AdmittedAssembly {
            assembly_id: CandidateAssemblyId::new(),
        },
    );
    assert_ne!(baseline, reordered);
    assert_ne!(baseline, changed_role);
    assert_ne!(baseline, changed_origin);
}
