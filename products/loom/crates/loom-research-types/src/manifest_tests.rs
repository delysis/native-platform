use std::fmt::Write as _;

use super::*;

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const ONE_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TWO_HASH: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn core_manifest() -> String {
    r#"format = "loom.core-pack.v1"
name = "fiction-core"
description = "General behavioral fiction criteria"

[[criteria]]
id = "continuity"
label = "Continuity"
description = "Tracks facts and physical state"
weight = 1.0
behavioral_anchors = ["No unsupported state changes", "Blocking remains legible"]
tags = ["state", "causal"]

[prompt_roles.bookfront]
description = "Natural publication-shaped front matter"
max_tokens = 1024

[prompt_roles.story_state]
description = "Evidence-grounded state only"
max_tokens = 2048
"#
    .to_owned()
}

fn genre_manifest() -> String {
    format!(
        r#"format = "loom.genre-pack.v1"
name = "mystery"
description = "Reveal logic without ontology lock-in"
genre_functions = ["mystery_reveal_logic", "suspense_causality"]

[core_pack]
format = "loom.core-pack.v1"
artifact_sha256 = "{ZERO_HASH}"

[criteria.reveal_logic]
description = "Clues support the eventual reveal"
weight_multiplier = 1.25
behavioral_anchors = ["The reveal is inferable", "Misdirection remains honest"]

[project_anchors.scene]
description = "Nearby project prose with matching causal function"
retrieval_limit = 2
"#
    )
}

fn model_bindings_manifest() -> String {
    format!(
        r#"format = "loom.model-bindings.v1"
name = "gemma-local"
description = "Pinned writer and controller artifacts"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{ZERO_HASH}"
model_bytes = 4954576032
tokenizer_sha256 = "{ONE_HASH}"
architecture = "gemma4"
context_tokens = 32768
capabilities = ["completion", "logits"]
adapters = []

[[bindings]]
id = "critic"
role = "critic"
model_sha256 = "{TWO_HASH}"
model_bytes = 123456789
tokenizer_sha256 = "{ONE_HASH}"
architecture = "gemma4"
context_tokens = 16384
capabilities = ["grammar", "instruct"]

[[bindings.adapters]]
artifact_sha256 = "{TWO_HASH}"
scale = 0.5
"#
    )
}

fn campaign_manifest(temperature: &str) -> String {
    format!(
        r#"format = "loom.campaign.v1"
name = "blocked-fiction-search"
description = "One frozen campaign definition"
seed = 42
selection = "successive_halving"

[core_pack]
format = "loom.core-pack.v1"
artifact_sha256 = "{ZERO_HASH}"

[genre_pack]
format = "loom.genre-pack.v1"
artifact_sha256 = "{ONE_HASH}"

[model_bindings]
format = "loom.model-bindings.v1"
artifact_sha256 = "{TWO_HASH}"

[budget]
max_writer_tokens = 1000000
max_controller_tokens = 100000
max_evaluations = 600

[[cases]]
id = "case-01"
genre_function = "mystery_reveal_logic"
source_sha256 = "{ONE_HASH}"
max_context_tokens = 8192

[[treatments]]
id = "direct-knee"
prompt_topology = "exact_direct_continuation"
samples_per_case = 8
max_output_tokens = 1024
control_parameters = {{ eta = 0.1, guidance_rescale = 0.7 }}

[treatments.sampler]
temperature = {temperature}
top_k = 40
top_p = 0.95
min_p = 0.05
typical_p = 1.0
repetition_penalty = 1.05
cfg_scale = 1.5
"#
    )
}

fn benchmark_manifest() -> String {
    format!(
        r#"format = "loom.benchmark.v1"
name = "five-function-confirmation"
description = "Sealed nested-N comparison"
seed = 99
nested_n = [32, 1, 8, 2, 16, 4]

[campaign]
format = "loom.campaign.v1"
artifact_sha256 = "{ZERO_HASH}"

[review]
frontier_model = "gpt-5-6-sol"
fresh_runs = 3
order_permutation_cells = 4

[[contenders]]
id = "efficient"
profile_sha256 = "{ONE_HASH}"

[[contenders]]
id = "frontier"
profile_sha256 = "{TWO_HASH}"

[[functions]]
id = "mystery_reveal_logic"
case_ids = ["mystery-06", "mystery-01"]

[[functions]]
id = "voice_heavy_literary_character_work"
case_ids = ["voice-01", "voice-06"]
"#
    )
}

#[test]
fn compiles_every_required_manifest_kind() {
    let fixtures = [
        (core_manifest(), ManifestFormat::CorePackV1),
        (genre_manifest(), ManifestFormat::GenrePackV1),
        (model_bindings_manifest(), ManifestFormat::ModelBindingsV1),
        (campaign_manifest("0.8"), ManifestFormat::CampaignV1),
        (benchmark_manifest(), ManifestFormat::BenchmarkV1),
    ];

    for (source, expected) in fixtures {
        let compiled = compile_manifest(source.as_bytes()).expect("valid manifest");
        assert_eq!(compiled.format(), expected);
        assert_eq!(compiled.source_bytes(), source.as_bytes());
        assert_ne!(
            compiled.source_hash().as_blob_id(),
            compiled.artifact_hash().as_blob_id()
        );
        assert!(
            compiled
                .canonical_bytes()
                .starts_with(CANONICAL_MANIFEST_DOMAIN)
        );
        compiled.verify_integrity().expect("intact compilation");
    }
}

#[test]
fn source_bytes_are_exact_but_semantic_hash_ignores_formatting() {
    let source = core_manifest();
    let with_comment = source.replace(
        "name = \"fiction-core\"",
        "# retained operator note\nname = \"fiction-core\"",
    );
    let left = compile_manifest(source.as_bytes()).expect("left");
    let right = compile_manifest(with_comment.as_bytes()).expect("right");

    assert_ne!(left.source_bytes(), right.source_bytes());
    assert_ne!(left.source_hash(), right.source_hash());
    assert_eq!(left.document(), right.document());
    assert_eq!(left.canonical_bytes(), right.canonical_bytes());
    assert_eq!(left.artifact_hash(), right.artifact_hash());
}

#[test]
fn maps_and_declared_sets_are_order_invariant() {
    let left_source = core_manifest();
    let right_source = left_source
        .replace(
            "tags = [\"state\", \"causal\"]",
            "tags = [\"causal\", \"state\"]",
        )
        .replace(
            "[prompt_roles.bookfront]\ndescription = \"Natural publication-shaped front matter\"\nmax_tokens = 1024\n\n[prompt_roles.story_state]\ndescription = \"Evidence-grounded state only\"\nmax_tokens = 2048",
            "[prompt_roles.story_state]\ndescription = \"Evidence-grounded state only\"\nmax_tokens = 2048\n\n[prompt_roles.bookfront]\ndescription = \"Natural publication-shaped front matter\"\nmax_tokens = 1024",
        );
    let left = compile_manifest(left_source.as_bytes()).expect("left");
    let right = compile_manifest(right_source.as_bytes()).expect("right");

    assert_ne!(left.source_hash(), right.source_hash());
    assert_eq!(left.document(), right.document());
    assert_eq!(left.artifact_hash(), right.artifact_hash());

    let benchmark_left = compile_manifest(benchmark_manifest().as_bytes()).expect("benchmark");
    let benchmark_right_source = benchmark_manifest()
        .replace("[32, 1, 8, 2, 16, 4]", "[1, 2, 4, 8, 16, 32]")
        .replace(
            "[\"mystery-06\", \"mystery-01\"]",
            "[\"mystery-01\", \"mystery-06\"]",
        );
    let benchmark_right =
        compile_manifest(benchmark_right_source.as_bytes()).expect("reordered benchmark");
    assert_eq!(
        benchmark_left.artifact_hash(),
        benchmark_right.artifact_hash()
    );
}

#[test]
fn exact_float_bits_distinguish_positive_and_negative_zero() {
    let positive = compile_manifest(campaign_manifest("0.0").as_bytes()).expect("positive zero");
    let negative = compile_manifest(campaign_manifest("-0.0").as_bytes()).expect("negative zero");
    assert_ne!(positive.document(), negative.document());
    assert_ne!(positive.canonical_bytes(), negative.canonical_bytes());
    assert_ne!(positive.artifact_hash(), negative.artifact_hash());
}

#[test]
fn nonfinite_treatment_values_are_rejected() {
    let error = compile_manifest(campaign_manifest("nan").as_bytes()).expect_err("reject NaN");
    assert!(error.to_string().contains("non-finite"));
    let error = compile_manifest(campaign_manifest("inf").as_bytes()).expect_err("reject infinity");
    assert!(error.to_string().contains("non-finite"));
}

#[test]
fn model_identity_fields_change_fingerprint_and_unknown_fields_fail() {
    let source = model_bindings_manifest();
    let baseline = compile_manifest(source.as_bytes()).expect("baseline model bindings");
    let changed_size = compile_manifest(
        source
            .replacen("model_bytes = 4954576032", "model_bytes = 4954576033", 1)
            .as_bytes(),
    )
    .expect("changed model size");
    let with_projector = compile_manifest(
        source
            .replacen(
                &format!("tokenizer_sha256 = \"{ONE_HASH}\""),
                &format!(
                    "tokenizer_sha256 = \"{ONE_HASH}\"\nmultimodal_projector_sha256 = \"{TWO_HASH}\""
                ),
                1,
            )
            .as_bytes(),
    )
    .expect("projector-bound model");

    assert_ne!(baseline.artifact_hash(), changed_size.artifact_hash());
    assert_ne!(baseline.artifact_hash(), with_projector.artifact_hash());

    let unknown = source.replacen(
        "model_bytes = 4954576032",
        "model_bytes = 4954576032\nmodel_path = \"/private/model.gguf\"",
        1,
    );
    assert!(compile_manifest(unknown.as_bytes()).is_err());
}

#[test]
fn unknown_fields_fail_at_top_level_and_nested_levels() {
    let top_level = core_manifest().replace(
        "description = \"General behavioral fiction criteria\"",
        "description = \"General behavioral fiction criteria\"\nsecret_override = true",
    );
    assert!(compile_manifest(top_level.as_bytes()).is_err());

    let nested = core_manifest().replace(
        "max_tokens = 1024",
        "max_tokens = 1024\nundeclared = \"no\"",
    );
    assert!(compile_manifest(nested.as_bytes()).is_err());
}

#[test]
fn source_and_schema_bounds_fail_closed() {
    assert!(matches!(
        compile_manifest(&[]),
        Err(ManifestCompileError::EmptySource)
    ));
    assert!(matches!(
        compile_manifest(&vec![b' '; MAX_MANIFEST_SOURCE_BYTES + 1]),
        Err(ManifestCompileError::SourceTooLarge { .. })
    ));
    assert!(matches!(
        compile_manifest(&[0xff]),
        Err(ManifestCompileError::InvalidUtf8 {
            valid_up_to: 0,
            error_len: Some(1)
        })
    ));

    let oversized_name = "x".repeat(MAX_MANIFEST_NAME_BYTES + 1);
    let source = core_manifest().replace("fiction-core", &oversized_name);
    assert!(compile_manifest(source.as_bytes()).is_err());

    let mut many_roles = core_manifest();
    for index in 0..=MAX_PROMPT_ROLES {
        write!(
            &mut many_roles,
            "\n[prompt_roles.extra{index}]\ndescription = \"role\"\nmax_tokens = 1\n"
        )
        .expect("write to string");
    }
    assert!(compile_manifest(many_roles.as_bytes()).is_err());
}

#[test]
fn toml_errors_are_structured_located_and_source_redacted() {
    const SECRET: &str = "the-unpublished-ending-is-albatross";
    let malformed = format!("format = \"loom.core-pack.v1\"\nname = \"{SECRET}\n");
    let error = compile_manifest(malformed.as_bytes()).expect_err("malformed TOML must fail");
    let location = match &error {
        ManifestCompileError::Toml {
            category: ManifestTomlErrorCategory::Syntax,
            location: Some(location),
        } => *location,
        other => panic!("unexpected redacted error: {other:?}"),
    };
    assert_eq!(location.line, 2);
    assert!(location.column > 1);
    assert!(!error.to_string().contains(SECRET));
    assert!(!format!("{error:?}").contains(SECRET));

    let unknown_field = core_manifest().replace(
        "name = \"fiction-core\"",
        &format!("name = \"fiction-core\"\n{SECRET} = \"do not retain this prose\""),
    );
    let error = compile_manifest(unknown_field.as_bytes()).expect_err("unknown field must fail");
    assert!(matches!(
        error,
        ManifestCompileError::Toml {
            category: ManifestTomlErrorCategory::UnknownField,
            location: Some(ManifestSourceLocation { line: 3, .. })
        }
    ));
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(SECRET));
    assert!(!rendered.contains("do not retain this prose"));
}

#[test]
fn toml_error_categories_discard_deserializer_messages() {
    const SECRET: &str = "classified-source-fragment";
    let cases = [
        (
            core_manifest().replace("name = \"fiction-core\"\n", ""),
            ManifestTomlErrorCategory::MissingField,
        ),
        (
            core_manifest().replace(
                "name = \"fiction-core\"",
                &format!("name = \"fiction-core\"\nname = \"{SECRET}\""),
            ),
            ManifestTomlErrorCategory::DuplicateField,
        ),
        (
            core_manifest().replace("max_tokens = 1024", &format!("max_tokens = \"{SECRET}\"")),
            ManifestTomlErrorCategory::InvalidType,
        ),
        (
            campaign_manifest("0.8").replace(
                "selection = \"successive_halving\"",
                &format!("selection = \"{SECRET}\""),
            ),
            ManifestTomlErrorCategory::InvalidValue,
        ),
        (
            core_manifest().replace("fiction-core", &SECRET.repeat(MAX_MANIFEST_NAME_BYTES)),
            ManifestTomlErrorCategory::ConstraintViolation,
        ),
    ];

    for (source, expected) in cases {
        let error = compile_manifest(source.as_bytes()).expect_err("invalid TOML must fail");
        assert!(matches!(
            error,
            ManifestCompileError::Toml { category, .. } if category == expected
        ));
        let rendered = format!("{error:?}\n{error}");
        assert!(!rendered.contains(SECRET));
    }
}

#[test]
fn source_locations_count_unicode_columns_without_retaining_text() {
    let source = "αβ\nγ";
    assert_eq!(
        manifest_source_location(source, "α".len()),
        ManifestSourceLocation { line: 1, column: 2 }
    );
    assert_eq!(
        manifest_source_location(source, "αβ\n".len()),
        ManifestSourceLocation { line: 2, column: 1 }
    );
}

#[test]
fn integrity_recompile_errors_do_not_capture_source_diagnostics() {
    const SECRET: &str = "private-manuscript-sentence";
    let malformed = format!("format = \"loom.core-pack.v1\"\nname = \"{SECRET}\n");
    let mut compiled = compile_manifest(core_manifest().as_bytes()).expect("valid baseline");
    compiled.source_bytes = malformed.as_bytes().to_vec();
    compiled.source_hash = ManifestSourceHash(BlobId::digest(malformed.as_bytes()));

    let error = compiled
        .verify_integrity()
        .expect_err("recompile of malformed source must fail");
    assert!(matches!(
        error,
        ManifestIntegrityError::Recompile(ref inner)
            if matches!(
                inner.as_ref(),
                ManifestCompileError::Toml {
                    category: ManifestTomlErrorCategory::Syntax,
                    location: Some(_)
                }
            )
    ));
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(SECRET));
}

#[test]
fn campaign_runtime_bounds_accept_exact_limits_without_clamping() {
    let exact_writer_demand = u64::from(MAX_TREATMENT_OUTPUT_TOKENS);
    let exact_controller_budget = MAX_CAMPAIGN_TOKEN_BUDGET - exact_writer_demand;
    let source = campaign_manifest("0.8")
        .replace("samples_per_case = 8", "samples_per_case = 1")
        .replace(
            "max_output_tokens = 1024",
            &format!("max_output_tokens = {MAX_TREATMENT_OUTPUT_TOKENS}"),
        )
        .replace("top_k = 40", &format!("top_k = {MAX_SAMPLER_TOP_K}"))
        .replace(
            "max_writer_tokens = 1000000",
            &format!("max_writer_tokens = {exact_writer_demand}"),
        )
        .replace(
            "max_controller_tokens = 100000",
            &format!("max_controller_tokens = {exact_controller_budget}"),
        )
        .replace(
            "max_evaluations = 600",
            &format!("max_evaluations = {MAX_CAMPAIGN_EVALUATIONS}"),
        );
    let compiled = compile_manifest(source.as_bytes()).expect("exact limits are valid");
    let ManifestDocument::Campaign(campaign) = compiled.document() else {
        panic!("campaign fixture compiled to the wrong document kind");
    };
    let treatment = &campaign.treatments()[0];
    assert_eq!(treatment.samples_per_case, 1);
    assert_eq!(treatment.max_output_tokens, MAX_TREATMENT_OUTPUT_TOKENS);
    assert_eq!(treatment.sampler.top_k, MAX_SAMPLER_TOP_K);
    assert_eq!(campaign.budget().max_writer_tokens, exact_writer_demand);
    assert_eq!(
        campaign.budget().max_controller_tokens,
        exact_controller_budget
    );
    assert_eq!(campaign.budget().max_evaluations, MAX_CAMPAIGN_EVALUATIONS);

    let samples_source = campaign_manifest("0.8").replace(
        "samples_per_case = 8",
        &format!("samples_per_case = {MAX_BASE_WRITER_BATCH_CASES}"),
    );
    compile_manifest(samples_source.as_bytes()).expect("maximum sample pool is valid");
}

#[test]
fn campaign_manifest_accepts_controller_free_budget() {
    let source = campaign_manifest("0.8").replace(
        "max_controller_tokens = 100000",
        "max_controller_tokens = 0",
    );
    let compiled = compile_manifest(source.as_bytes()).expect("controller-free campaign");
    let ManifestDocument::Campaign(campaign) = compiled.document() else {
        panic!("campaign fixture compiled to the wrong document kind");
    };
    assert_eq!(campaign.budget().max_controller_tokens, 0);
}

#[test]
fn campaign_runtime_bounds_reject_each_value_above_its_limit() {
    let cases = [
        (
            campaign_manifest("0.8").replace(
                "samples_per_case = 8",
                &format!("samples_per_case = {}", MAX_BASE_WRITER_BATCH_CASES + 1),
            ),
            "treatments.samples_per_case",
        ),
        (
            campaign_manifest("0.8").replace(
                "max_output_tokens = 1024",
                &format!("max_output_tokens = {}", MAX_TREATMENT_OUTPUT_TOKENS + 1),
            ),
            "treatments.max_output_tokens",
        ),
        (
            campaign_manifest("0.8")
                .replace("top_k = 40", &format!("top_k = {}", MAX_SAMPLER_TOP_K + 1)),
            "sampler.top_k",
        ),
        (
            campaign_manifest("0.8").replace(
                "max_writer_tokens = 1000000",
                &format!("max_writer_tokens = {}", MAX_CAMPAIGN_TOKEN_BUDGET + 1),
            ),
            "budget.max_writer_tokens",
        ),
        (
            campaign_manifest("0.8").replace(
                "max_controller_tokens = 100000",
                &format!("max_controller_tokens = {}", MAX_CAMPAIGN_TOKEN_BUDGET + 1),
            ),
            "budget.max_controller_tokens",
        ),
        (
            campaign_manifest("0.8").replace(
                "max_evaluations = 600",
                &format!("max_evaluations = {}", MAX_CAMPAIGN_EVALUATIONS + 1),
            ),
            "budget.max_evaluations",
        ),
    ];

    for (source, field) in cases {
        let error = compile_manifest(source.as_bytes()).expect_err("above-limit value must fail");
        assert!(matches!(
            error,
            ManifestCompileError::InvalidField {
                field: actual_field,
                violation: ManifestFieldViolation::ExceedsMaximum { .. }
            } if actual_field == field
        ));
    }
}

#[test]
fn campaign_budget_must_cover_checked_declared_writer_demand() {
    let underfunded =
        campaign_manifest("0.8").replace("max_writer_tokens = 1000000", "max_writer_tokens = 8191");
    let error = compile_manifest(underfunded.as_bytes()).expect_err("8192 tokens are declared");
    assert_eq!(
        error,
        ManifestCompileError::InvalidField {
            field: "budget.max_writer_tokens",
            violation: ManifestFieldViolation::InsufficientWriterBudget {
                required: 8 * 1024,
                available: 8191,
            },
        }
    );

    let exact =
        campaign_manifest("0.8").replace("max_writer_tokens = 1000000", "max_writer_tokens = 8192");
    compile_manifest(exact.as_bytes()).expect("exact declared demand must fit");

    let excessive_aggregate = campaign_manifest("0.8")
        .replace(
            "max_writer_tokens = 1000000",
            &format!("max_writer_tokens = {MAX_CAMPAIGN_TOKEN_BUDGET}"),
        )
        .replace(
            "max_controller_tokens = 100000",
            "max_controller_tokens = 1",
        );
    let error = compile_manifest(excessive_aggregate.as_bytes())
        .expect_err("aggregate budget above the global ceiling must fail");
    assert!(matches!(
        error,
        ManifestCompileError::InvalidField {
            field: "budget.aggregate_token_budget",
            violation: ManifestFieldViolation::ExceedsMaximum {
                actual,
                maximum: MAX_CAMPAIGN_TOKEN_BUDGET
            }
        } if actual == MAX_CAMPAIGN_TOKEN_BUDGET + 1
    ));
}

#[test]
fn aggregate_campaign_demand_cannot_exceed_global_budget() {
    let mut source = campaign_manifest("0.8")
        .replace(
            "max_writer_tokens = 1000000",
            &format!(
                "max_writer_tokens = {}",
                MAX_CAMPAIGN_TOKEN_BUDGET - 100_000
            ),
        )
        .replace(
            "samples_per_case = 8",
            &format!("samples_per_case = {MAX_BASE_WRITER_BATCH_CASES}"),
        )
        .replace(
            "max_output_tokens = 1024",
            &format!("max_output_tokens = {MAX_TREATMENT_OUTPUT_TOKENS}"),
        );
    write!(
        &mut source,
        "\n[[cases]]\nid = \"case-02\"\ngenre_function = \"mystery_reveal_logic\"\nsource_sha256 = \"{TWO_HASH}\"\nmax_context_tokens = 8192\n"
    )
    .expect("append second case");
    for index in 1..MAX_TREATMENTS {
        write!(
            &mut source,
            r#"
[[treatments]]
id = "treatment-{index}"
prompt_topology = "exact_direct_continuation"
samples_per_case = {MAX_BASE_WRITER_BATCH_CASES}
max_output_tokens = {MAX_TREATMENT_OUTPUT_TOKENS}
control_parameters = {{}}

[treatments.sampler]
temperature = 0.8
top_k = 0
top_p = 0.95
min_p = 0.05
typical_p = 1.0
repetition_penalty = 1.05
"#
        )
        .expect("append treatment");
    }

    let error = compile_manifest(source.as_bytes()).expect_err("aggregate demand is pathological");
    assert!(matches!(
        error,
        ManifestCompileError::InvalidField {
            field: "campaign.maximum_declared_writer_tokens",
            violation: ManifestFieldViolation::ExceedsMaximum {
                actual,
                maximum: MAX_CAMPAIGN_TOKEN_BUDGET
            }
        } if actual > MAX_CAMPAIGN_TOKEN_BUDGET
    ));
}

#[test]
fn compiler_is_the_single_top_level_semantic_admission_path() {
    let wrong_format = core_manifest().replace("loom.core-pack.v1", "loom.genre-pack.v1");
    assert!(compile_manifest(wrong_format.as_bytes()).is_err());

    let duplicate_criterion = format!(
        "{}\n[[criteria]]\nid = \"continuity\"\nlabel = \"Duplicate\"\ndescription = \"Rejected\"\nweight = 1.0\nbehavioral_anchors = [\"Rejected\"]\ntags = []\n",
        core_manifest()
    );
    let error = compile_manifest(duplicate_criterion.as_bytes()).expect_err("duplicate ids fail");
    assert!(error.to_string().contains("criteria.id"));

    let zero_token_limit = core_manifest().replace("max_tokens = 1024", "max_tokens = 0");
    let error = compile_manifest(zero_token_limit.as_bytes()).expect_err("zero token limit fails");
    assert!(error.to_string().contains("prompt_roles.max_tokens"));

    let wrong_reference = genre_manifest().replace(
        "format = \"loom.core-pack.v1\"\nartifact_sha256",
        "format = \"loom.campaign.v1\"\nartifact_sha256",
    );
    let error = compile_manifest(wrong_reference.as_bytes()).expect_err("wrong reference kind");
    assert!(error.to_string().contains("core_pack"));

    let invalid_sampler = campaign_manifest("0.8").replace("top_p = 0.95", "top_p = 1.01");
    let error = compile_manifest(invalid_sampler.as_bytes()).expect_err("range check");
    assert!(error.to_string().contains("sampler.top_p"));

    let mut comment_padded = core_manifest();
    comment_padded.push('#');
    comment_padded.push_str(&"x".repeat(MAX_MANIFEST_SOURCE_BYTES));
    assert!(matches!(
        compile_manifest(comment_padded.as_bytes()),
        Err(ManifestCompileError::SourceTooLarge { .. })
    ));
}

#[test]
fn wrong_or_unknown_formats_are_rejected() {
    let wrong = core_manifest().replace("loom.core-pack.v1", "loom.genre-pack.v1");
    assert!(compile_manifest(wrong.as_bytes()).is_err());

    let unknown = core_manifest().replace("loom.core-pack.v1", "loom.core-pack.v2");
    assert!(compile_manifest(unknown.as_bytes()).is_err());

    let wrong_reference = genre_manifest().replace(
        "format = \"loom.core-pack.v1\"\nartifact_sha256",
        "format = \"loom.campaign.v1\"\nartifact_sha256",
    );
    let error = compile_manifest(wrong_reference.as_bytes()).expect_err("wrong reference kind");
    assert!(error.to_string().contains("core_pack"));
}

#[test]
fn integrity_verification_detects_every_internal_tamper_class() {
    let intact = compile_manifest(core_manifest().as_bytes()).expect("compile");

    let mut source = intact.clone();
    source.source_bytes.push(b' ');
    assert_eq!(
        source.verify_integrity(),
        Err(ManifestIntegrityError::SourceHash)
    );

    let mut document = intact.clone();
    document.document = compile_manifest(genre_manifest().as_bytes())
        .expect("other document")
        .document;
    assert_eq!(
        document.verify_integrity(),
        Err(ManifestIntegrityError::Document)
    );

    let mut canonical = intact.clone();
    canonical.canonical_bytes.push(0);
    assert_eq!(
        canonical.verify_integrity(),
        Err(ManifestIntegrityError::CanonicalBytes)
    );

    let mut artifact = intact;
    artifact.artifact_hash = ManifestArtifactHash(BlobId::digest(b"tampered"));
    assert_eq!(
        artifact.verify_integrity(),
        Err(ManifestIntegrityError::ArtifactHash)
    );
}
