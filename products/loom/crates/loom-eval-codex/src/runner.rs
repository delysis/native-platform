use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt as _, process::CommandExt as _};

use jsonschema::Validator;
use loom_types::BlobId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;

use crate::{
    CheckedCodexJsonl, CodexJsonlError, MAX_CODEX_JSONL_BYTES, check_tool_free_codex_jsonl,
};

pub const FRONTIER_MODEL: &str = "gpt-5.6-sol";
pub const FRONTIER_REASONING_EFFORT: &str = "xhigh";
pub const CODEX_DIAGNOSTIC_LOG_FILTER: &str = "error";
pub const CONFIRMATORY_FRESH_RUNS: u8 = 3;
pub const CONFIRMATORY_ORDER_PERMUTATION_CELLS: u8 = 4;
pub const MAX_FRONTIER_PROMPT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FRONTIER_SCHEMA_BYTES: usize = 1024 * 1024;
pub const MAX_FRONTIER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CLI_BINARY_BYTES: u64 = 512 * 1024 * 1024;
pub const FRONTIER_EXEC_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub const FRONTIER_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

pub const PINNED_CHATGPT_CODEX_PATH: &str = "/Applications/ChatGPT.app/Contents/Resources/codex";
pub const PINNED_CHATGPT_CODEX_SHA256: &str =
    "d96ae1ca1ff6fc8587842fa04c92d3ee4d31651a811c2f89b65fcfd9c28473e2";
pub const PINNED_CHATGPT_CODEX_VERSION: &str = "codex-cli 0.146.0-alpha.9.2";
pub const PINNED_CHATGPT_TEAM_ID: &str = "2DC432GLL2";
pub const PINNED_CHATGPT_CODEX_CDHASH_SHA256: &str =
    "dce9780d114a670768798d0dc0de4a96b422c309379e17ef14e2404e08dea2fd";

const FRONTIER_ADAPTER_PROTOCOL: &str = "loom.frontier-critic.diagnostic-chatgpt-codex.v3";
const LIVE_CHALLENGE_FIELD: &str = "loom_live_challenge";
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_PROBE_BYTES: usize = 64 * 1024;
const MAX_PRIVATE_WORKSPACE_ENTRIES: usize = 2;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 64 * 1024;
const MAX_EFFECTIVE_ENVIRONMENT_BYTES: usize = 256 * 1024;

const PACKET_DOMAIN: &[u8] = b"loom/codex-frontier-critic-packet/v2\0";
const RECEIPT_DOMAIN: &[u8] = b"loom/codex-frontier-critic-diagnostic-receipt/v2\0";
const CODE_SIGNATURE_DOMAIN: &[u8] = b"loom/codex-frontier-code-signature/v1\0";
const PROTOCOL_DOMAIN: &[u8] = b"loom/codex-frontier-protocol/v1\0";
const BUILD_DOMAIN: &[u8] = b"loom/codex-frontier-build/v1\0";
const ENVIRONMENT_DOMAIN: &[u8] = b"loom/codex-frontier-environment/v1\0";
const AUTHENTICATION_DOMAIN: &[u8] = b"loom/codex-frontier-authentication/v1\0";
const INVOCATION_DOMAIN: &[u8] = b"loom/codex-frontier-invocation/v1\0";
const CHALLENGE_DOMAIN: &[u8] = b"loom/codex-frontier-live-challenge/v1\0";
const SESSION_DOMAIN: &[u8] = b"loom/codex-frontier-session/v1\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticDisclosurePolicy {
    ManuscriptOnly,
    TraceAwareCloseRead,
}

impl CriticDisclosurePolicy {
    const fn tag(self) -> u8 {
        match self {
            Self::ManuscriptOnly => 0,
            Self::TraceAwareCloseRead => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptInjectionDisposition {
    NotAssessed,
    NoKnownSuspicion,
    Suspected,
}

impl PromptInjectionDisposition {
    const fn tag(self) -> u8 {
        match self {
            Self::NotAssessed => 0,
            Self::NoKnownSuspicion => 1,
            Self::Suspected => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierModelObservation {
    /// This CLI protocol reports the requested model but no independently
    /// authenticated fact proving which model served the turn.
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierExecutionClass {
    CallerPinnedDiagnostic,
    ChatGptBundledDiagnostic,
}

impl FrontierExecutionClass {
    const fn tag(self) -> u8 {
        match self {
            Self::CallerPinnedDiagnostic => 0,
            Self::ChatGptBundledDiagnostic => 1,
        }
    }
}

/// Exact randomized comparison cell supplied to one fresh critic process.
///
/// This packet and every result produced from it are diagnostic data, never
/// writer, store, campaign, benchmark, or evaluation authority.
pub struct FrontierCriticPacket {
    comparison_fingerprint: BlobId,
    order_cell: u8,
    criterion_permutation_fingerprint: BlobId,
    disclosure_policy: CriticDisclosurePolicy,
    prompt_injection_disposition: PromptInjectionDisposition,
    prompt_utf8: Vec<u8>,
    output_schema_utf8: Vec<u8>,
}

impl fmt::Debug for FrontierCriticPacket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrontierCriticPacket")
            .field("packet_fingerprint", &self.packet_fingerprint())
            .field("comparison_fingerprint", &self.comparison_fingerprint)
            .field("order_cell", &self.order_cell)
            .field(
                "criterion_permutation_fingerprint",
                &self.criterion_permutation_fingerprint,
            )
            .field("disclosure_policy", &self.disclosure_policy)
            .field(
                "prompt_injection_disposition",
                &self.prompt_injection_disposition,
            )
            .field("prompt_byte_len", &self.prompt_utf8.len())
            .field("schema_byte_len", &self.output_schema_utf8.len())
            .finish()
    }
}

impl FrontierCriticPacket {
    pub fn new(
        comparison_fingerprint: BlobId,
        order_cell: u8,
        criterion_permutation_fingerprint: BlobId,
        disclosure_policy: CriticDisclosurePolicy,
        prompt_utf8: Vec<u8>,
        output_schema_utf8: Vec<u8>,
    ) -> Result<Self, FrontierCriticError> {
        validate_utf8_bound(
            &prompt_utf8,
            MAX_FRONTIER_PROMPT_BYTES,
            FrontierCriticError::InvalidPrompt,
        )?;
        validate_utf8_bound(
            &output_schema_utf8,
            MAX_FRONTIER_SCHEMA_BYTES,
            FrontierCriticError::InvalidSchema,
        )?;
        if order_cell >= CONFIRMATORY_ORDER_PERMUTATION_CELLS {
            return Err(FrontierCriticError::InvalidOrderCell(order_cell));
        }
        let schema_value: Value = serde_json::from_slice(&output_schema_utf8)
            .map_err(FrontierCriticError::MalformedSchema)?;
        validate_closed_output_schema(&schema_value)?;
        Ok(Self {
            comparison_fingerprint,
            order_cell,
            criterion_permutation_fingerprint,
            disclosure_policy,
            prompt_injection_disposition: PromptInjectionDisposition::NotAssessed,
            prompt_utf8,
            output_schema_utf8,
        })
    }

    pub const fn comparison_fingerprint(&self) -> BlobId {
        self.comparison_fingerprint
    }

    pub const fn order_cell(&self) -> u8 {
        self.order_cell
    }

    pub const fn criterion_permutation_fingerprint(&self) -> BlobId {
        self.criterion_permutation_fingerprint
    }

    pub const fn disclosure_policy(&self) -> CriticDisclosurePolicy {
        self.disclosure_policy
    }

    pub const fn prompt_injection_disposition(&self) -> PromptInjectionDisposition {
        self.prompt_injection_disposition
    }

    pub fn prompt_utf8(&self) -> &[u8] {
        &self.prompt_utf8
    }

    pub fn output_schema_utf8(&self) -> &[u8] {
        &self.output_schema_utf8
    }

    pub fn packet_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(PACKET_DOMAIN);
        digest.update(self.comparison_fingerprint.as_bytes());
        digest.update([
            self.order_cell,
            self.disclosure_policy.tag(),
            self.prompt_injection_disposition.tag(),
        ]);
        digest.update(self.criterion_permutation_fingerprint.as_bytes());
        update_bytes(&mut digest, &self.prompt_utf8);
        update_bytes(&mut digest, &self.output_schema_utf8);
        BlobId::from_bytes(digest.finalize().into())
    }

    pub(crate) fn set_prompt_injection_disposition(
        &mut self,
        disposition: PromptInjectionDisposition,
    ) {
        self.prompt_injection_disposition = disposition;
    }

    fn bind_live_challenge(
        mut self,
        challenge: &LiveChallenge,
    ) -> Result<Self, FrontierCriticError> {
        let challenge_hex = challenge.hex();
        let mut schema: Value = serde_json::from_slice(&self.output_schema_utf8)
            .map_err(FrontierCriticError::MalformedSchema)?;
        let object = schema
            .as_object_mut()
            .ok_or(FrontierCriticError::OpenOutputSchema)?;
        let properties = object
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .ok_or(FrontierCriticError::OpenOutputSchema)?;
        if properties.contains_key(LIVE_CHALLENGE_FIELD) {
            return Err(FrontierCriticError::ReservedChallengeField);
        }
        properties.insert(
            LIVE_CHALLENGE_FIELD.to_owned(),
            serde_json::json!({"type": "string", "const": challenge_hex}),
        );
        object
            .get_mut("required")
            .and_then(Value::as_array_mut)
            .ok_or(FrontierCriticError::OpenOutputSchema)?
            .push(Value::String(LIVE_CHALLENGE_FIELD.to_owned()));
        self.output_schema_utf8 =
            serde_json::to_vec(&schema).map_err(FrontierCriticError::ChallengeSchemaEncoding)?;
        validate_utf8_bound(
            &self.output_schema_utf8,
            MAX_FRONTIER_SCHEMA_BYTES,
            FrontierCriticError::InvalidSchema,
        )?;
        validate_closed_output_schema(&schema)?;

        let suffix = format!(
            "\n\nLive execution challenge: include the exact field \"{LIVE_CHALLENGE_FIELD}\" with value \"{challenge_hex}\" in the returned JSON object. This random value is a freshness diagnostic, not manuscript evidence or authority."
        );
        let new_len = self
            .prompt_utf8
            .len()
            .checked_add(suffix.len())
            .ok_or(FrontierCriticError::InvalidPrompt(usize::MAX))?;
        if new_len > MAX_FRONTIER_PROMPT_BYTES {
            return Err(FrontierCriticError::InvalidPrompt(new_len));
        }
        self.prompt_utf8.extend_from_slice(suffix.as_bytes());
        Ok(self)
    }
}

/// Caller-selected executable facts for non-authoritative diagnostics.
///
/// Diagnostic execution remains useful for development, but its receipt has no
/// conversion into evaluation, campaign, store, benchmark, or edit authority.
#[derive(Clone, Debug)]
pub struct DiagnosticFrontierCriticConfig {
    cli_path: PathBuf,
    expected_cli_sha256: BlobId,
    expected_cli_version: String,
    environment: ReviewedEnvironment,
}

impl DiagnosticFrontierCriticConfig {
    pub fn pinned(
        cli_path: PathBuf,
        expected_cli_sha256: BlobId,
        expected_cli_version: impl Into<String>,
    ) -> Result<Self, FrontierCriticError> {
        let expected_cli_version = expected_cli_version.into();
        if !cli_path.is_absolute()
            || expected_cli_version.is_empty()
            || expected_cli_version.len() > 256
            || expected_cli_version.contains(['\r', '\n'])
        {
            return Err(FrontierCriticError::InvalidConfig);
        }
        Ok(Self {
            cli_path,
            expected_cli_sha256,
            expected_cli_version,
            environment: ReviewedEnvironment::capture()?,
        })
    }

    pub fn cli_path(&self) -> &Path {
        self.cli_path.as_path()
    }

    pub const fn expected_cli_sha256(&self) -> BlobId {
        self.expected_cli_sha256
    }

    pub fn expected_cli_version(&self) -> &str {
        &self.expected_cli_version
    }
}

/// Exact code-signature facts for the copied ChatGPT-bundled executable.
///
/// These facts identify the local executable. They do not attest the remote
/// model or configuration that served the request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiagnosticCodeSignatureEvidence {
    fingerprint: BlobId,
    team_id: String,
    cdhash_sha256: String,
}

impl DiagnosticCodeSignatureEvidence {
    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn team_id(&self) -> &str {
        &self.team_id
    }

    pub fn cdhash_sha256(&self) -> &str {
        &self.cdhash_sha256
    }
}

/// Complete, serializable diagnostic evidence. It is intentionally cloneable
/// because it has no authority and no conversion into an authority type.
#[derive(Clone, Serialize)]
pub struct DiagnosticFrontierCriticReceipt {
    execution_class: FrontierExecutionClass,
    cli_sha256: BlobId,
    cli_version: String,
    authentication_status_utf8: Vec<u8>,
    authentication_receipt_fingerprint: BlobId,
    code_signature: Option<DiagnosticCodeSignatureEvidence>,
    requested_model: String,
    observed_model: FrontierModelObservation,
    requested_reasoning_effort: String,
    diagnostic_log_filter: String,
    environment_policy_fingerprint: BlobId,
    environment_values: Vec<EnvironmentValueEvidence>,
    evaluator_protocol_fingerprint: BlobId,
    adapter_build_fingerprint: BlobId,
    argv_fingerprint: BlobId,
    invocation_fingerprint: BlobId,
    prepared_packet_fingerprint: BlobId,
    executed_packet_fingerprint: BlobId,
    comparison_fingerprint: BlobId,
    order_cell: u8,
    criterion_permutation_fingerprint: BlobId,
    disclosure_policy: CriticDisclosurePolicy,
    prompt_injection_disposition: PromptInjectionDisposition,
    live_challenge_fingerprint: Option<BlobId>,
    fresh_session_fingerprint: BlobId,
    exact_prompt_utf8: Vec<u8>,
    output_schema_utf8: Vec<u8>,
    raw_jsonl_utf8: Vec<u8>,
    final_output_utf8: Vec<u8>,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    tool_activity_observed: bool,
    complete: bool,
    receipt_fingerprint: BlobId,
}

impl fmt::Debug for DiagnosticFrontierCriticReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DiagnosticFrontierCriticReceipt")
            .field("receipt_fingerprint", &self.receipt_fingerprint)
            .field("execution_class", &self.execution_class)
            .field("cli_sha256", &self.cli_sha256)
            .field("cli_version", &self.cli_version)
            .field("code_signature", &self.code_signature)
            .field("requested_model", &self.requested_model)
            .field("observed_model", &self.observed_model)
            .field(
                "requested_reasoning_effort",
                &self.requested_reasoning_effort,
            )
            .field("comparison_fingerprint", &self.comparison_fingerprint)
            .field("order_cell", &self.order_cell)
            .field("disclosure_policy", &self.disclosure_policy)
            .field("prompt_byte_len", &self.exact_prompt_utf8.len())
            .field("schema_byte_len", &self.output_schema_utf8.len())
            .field("jsonl_byte_len", &self.raw_jsonl_utf8.len())
            .field("final_output_byte_len", &self.final_output_utf8.len())
            .field("input_tokens", &self.input_tokens)
            .field("cached_input_tokens", &self.cached_input_tokens)
            .field("output_tokens", &self.output_tokens)
            .field("tool_activity_observed", &self.tool_activity_observed)
            .field("complete", &self.complete)
            .finish_non_exhaustive()
    }
}

impl DiagnosticFrontierCriticReceipt {
    pub const fn receipt_fingerprint(&self) -> BlobId {
        self.receipt_fingerprint
    }

    pub const fn execution_class(&self) -> FrontierExecutionClass {
        self.execution_class
    }

    pub fn final_output_utf8(&self) -> &[u8] {
        &self.final_output_utf8
    }

    pub fn raw_jsonl_utf8(&self) -> &[u8] {
        &self.raw_jsonl_utf8
    }

    pub const fn cli_sha256(&self) -> BlobId {
        self.cli_sha256
    }

    pub fn cli_version(&self) -> &str {
        &self.cli_version
    }

    pub fn authentication_status_utf8(&self) -> &[u8] {
        &self.authentication_status_utf8
    }

    pub const fn authentication_receipt_fingerprint(&self) -> BlobId {
        self.authentication_receipt_fingerprint
    }

    pub const fn code_signature(&self) -> Option<&DiagnosticCodeSignatureEvidence> {
        self.code_signature.as_ref()
    }

    pub fn requested_model(&self) -> &str {
        &self.requested_model
    }

    pub const fn observed_model(&self) -> FrontierModelObservation {
        self.observed_model
    }

    pub fn requested_reasoning_effort(&self) -> &str {
        &self.requested_reasoning_effort
    }

    pub fn diagnostic_log_filter(&self) -> &str {
        &self.diagnostic_log_filter
    }

    pub const fn environment_policy_fingerprint(&self) -> BlobId {
        self.environment_policy_fingerprint
    }

    pub fn environment_values(&self) -> &[EnvironmentValueEvidence] {
        &self.environment_values
    }

    pub const fn evaluator_protocol_fingerprint(&self) -> BlobId {
        self.evaluator_protocol_fingerprint
    }

    pub const fn adapter_build_fingerprint(&self) -> BlobId {
        self.adapter_build_fingerprint
    }

    pub const fn argv_fingerprint(&self) -> BlobId {
        self.argv_fingerprint
    }

    pub const fn invocation_fingerprint(&self) -> BlobId {
        self.invocation_fingerprint
    }

    pub const fn prepared_packet_fingerprint(&self) -> BlobId {
        self.prepared_packet_fingerprint
    }

    pub const fn executed_packet_fingerprint(&self) -> BlobId {
        self.executed_packet_fingerprint
    }

    pub const fn comparison_fingerprint(&self) -> BlobId {
        self.comparison_fingerprint
    }

    pub const fn order_cell(&self) -> u8 {
        self.order_cell
    }

    pub const fn criterion_permutation_fingerprint(&self) -> BlobId {
        self.criterion_permutation_fingerprint
    }

    pub const fn disclosure_policy(&self) -> CriticDisclosurePolicy {
        self.disclosure_policy
    }

    pub const fn prompt_injection_disposition(&self) -> PromptInjectionDisposition {
        self.prompt_injection_disposition
    }

    pub const fn live_challenge_fingerprint(&self) -> Option<BlobId> {
        self.live_challenge_fingerprint
    }

    pub const fn fresh_session_fingerprint(&self) -> BlobId {
        self.fresh_session_fingerprint
    }

    pub fn exact_prompt_utf8(&self) -> &[u8] {
        &self.exact_prompt_utf8
    }

    pub fn output_schema_utf8(&self) -> &[u8] {
        &self.output_schema_utf8
    }

    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    pub const fn cached_input_tokens(&self) -> u64 {
        self.cached_input_tokens
    }

    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    pub const fn tool_activity_observed(&self) -> bool {
        self.tool_activity_observed
    }

    pub const fn complete(&self) -> bool {
        self.complete
    }
}

/// Run an arbitrary pinned executable as a bounded diagnostic. The returned
/// receipt can never mint evaluation, campaign, store, or manuscript authority.
pub fn run_diagnostic_frontier_critic(
    config: &DiagnosticFrontierCriticConfig,
    packet: FrontierCriticPacket,
) -> Result<DiagnosticFrontierCriticReceipt, FrontierCriticError> {
    let owned_cli = OwnedCli::copy_exact(config.cli_path(), config.expected_cli_sha256)?;
    run_exact_frontier_critic(
        owned_cli.path(),
        config.expected_cli_sha256,
        &config.expected_cli_version,
        &config.environment,
        packet.packet_fingerprint(),
        packet,
        FrontierExecutionClass::CallerPinnedDiagnostic,
        None,
        None,
    )
}

/// Run the exact reviewed ChatGPT-bundled CLI as a checked diagnostic.
///
/// The executable identity, signature, version, `ChatGPT` authentication, exact
/// request, tool-free JSONL, schema-valid final output, and private workspace
/// are checked. The serving model/configuration remains unobserved, so this
/// function deliberately returns the same cloneable diagnostic receipt type as
/// caller-pinned execution.
pub fn run_chatgpt_bundled_frontier_critic_diagnostic(
    packet: FrontierCriticPacket,
) -> Result<DiagnosticFrontierCriticReceipt, FrontierCriticError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = packet;
        Err(FrontierCriticError::UnsupportedPinnedPlatform)
    }
    #[cfg(target_os = "macos")]
    {
        run_macos_chatgpt_bundled_frontier_critic_diagnostic(packet)
    }
}

#[cfg(target_os = "macos")]
fn run_macos_chatgpt_bundled_frontier_critic_diagnostic(
    packet: FrontierCriticPacket,
) -> Result<DiagnosticFrontierCriticReceipt, FrontierCriticError> {
    let expected_sha256 = pinned_cli_sha256();
    let environment = ReviewedEnvironment::capture()?;
    let owned_cli = OwnedCli::copy_exact(Path::new(PINNED_CHATGPT_CODEX_PATH), expected_sha256)?;
    owned_cli.revalidate()?;
    let signature = verify_pinned_code_signature(owned_cli.path())?;
    let challenge = LiveChallenge::fresh()?;
    let prepared_packet_fingerprint = packet.packet_fingerprint();
    let challenged_packet = packet.bind_live_challenge(&challenge)?;
    run_exact_frontier_critic(
        owned_cli.path(),
        owned_cli.sha256,
        PINNED_CHATGPT_CODEX_VERSION,
        &environment,
        prepared_packet_fingerprint,
        challenged_packet,
        FrontierExecutionClass::ChatGptBundledDiagnostic,
        Some(challenge.fingerprint),
        Some(signature.evidence()),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_exact_frontier_critic(
    cli_path: &Path,
    cli_sha256: BlobId,
    expected_cli_version: &str,
    environment: &ReviewedEnvironment,
    prepared_packet_fingerprint: BlobId,
    packet: FrontierCriticPacket,
    execution_class: FrontierExecutionClass,
    live_challenge_fingerprint: Option<BlobId>,
    code_signature: Option<DiagnosticCodeSignatureEvidence>,
) -> Result<DiagnosticFrontierCriticReceipt, FrontierCriticError> {
    let authentication_status =
        verify_cli_version_and_authentication(cli_path, expected_cli_version, environment)?;

    let workspace = TempDir::new().map_err(FrontierCriticError::Io)?;
    let schema_path = workspace.path().join("output-schema.json");
    let output_path = workspace.path().join("final-output.json");
    write_new_file(&schema_path, &packet.output_schema_utf8)?;
    let args = build_exec_args(workspace.path(), &schema_path, &output_path);
    let argv_fingerprint = fingerprint_os_values(b"argv\0", &args);
    let evaluator_protocol_fingerprint = evaluator_protocol_fingerprint();
    let adapter_build_fingerprint = adapter_build_fingerprint();
    let invocation_fingerprint = fingerprint_invocation(
        cli_sha256,
        argv_fingerprint,
        environment.fingerprint,
        evaluator_protocol_fingerprint,
        adapter_build_fingerprint,
        packet.packet_fingerprint(),
        live_challenge_fingerprint,
    );

    let mut command = Command::new(cli_path);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    environment.apply(&mut command);
    configure_process_group(&mut command);
    let process_started = Instant::now();
    let child = command.spawn().map_err(FrontierCriticError::Spawn)?;
    let output = capture_child(
        child,
        Some(&packet.prompt_utf8),
        MAX_CODEX_JSONL_BYTES,
        MAX_FRONTIER_OUTPUT_BYTES,
        FRONTIER_EXEC_TIMEOUT,
        process_started,
    )?;
    if !output.status.success() {
        return Err(FrontierCriticError::ExecFailed {
            code: output.status.code(),
            stderr_fingerprint: BlobId::digest(&output.stderr),
        });
    }
    if !output.stderr.is_empty() {
        return Err(FrontierCriticError::UnexpectedStderr);
    }
    let checked = check_tool_free_codex_jsonl(&output.stdout)?;
    let final_output = read_bounded_regular_file(
        &output_path,
        MAX_FRONTIER_OUTPUT_BYTES,
        FrontierCriticError::InvalidFinalOutput,
    )?;
    validate_utf8_bound(
        &final_output,
        MAX_FRONTIER_OUTPUT_BYTES,
        FrontierCriticError::InvalidFinalOutput,
    )?;
    if final_output != checked.final_agent_message().as_bytes() {
        return Err(FrontierCriticError::FinalOutputMismatch);
    }
    let schema: Value = serde_json::from_slice(&packet.output_schema_utf8)
        .map_err(FrontierCriticError::MalformedSchema)?;
    validate_closed_output_schema(&schema)?;
    let instance: Value =
        serde_json::from_slice(&final_output).map_err(FrontierCriticError::MalformedFinalOutput)?;
    let validator = Validator::new(&schema).map_err(|_| FrontierCriticError::InvalidSchema(0))?;
    if !validator.is_valid(&instance) {
        return Err(FrontierCriticError::SchemaViolation);
    }
    let schema_on_disk = read_bounded_regular_file(
        &schema_path,
        MAX_FRONTIER_SCHEMA_BYTES,
        FrontierCriticError::InvalidSchema,
    )?;
    if schema_on_disk != packet.output_schema_utf8 {
        return Err(FrontierCriticError::WorkspaceMutation);
    }
    verify_private_workspace(workspace.path(), &schema_path, &output_path)?;

    Ok(build_diagnostic_receipt(
        execution_class,
        cli_sha256,
        expected_cli_version,
        authentication_status,
        code_signature,
        environment,
        argv_fingerprint,
        invocation_fingerprint,
        evaluator_protocol_fingerprint,
        adapter_build_fingerprint,
        prepared_packet_fingerprint,
        packet,
        live_challenge_fingerprint,
        output.stdout,
        final_output,
        &checked,
    ))
}

fn verify_cli_version_and_authentication(
    cli_path: &Path,
    expected_cli_version: &str,
    environment: &ReviewedEnvironment,
) -> Result<Vec<u8>, FrontierCriticError> {
    let cli_version = run_probe(cli_path, &[OsString::from("--version")], environment)?;
    if trim_single_line(&cli_version)? != expected_cli_version {
        return Err(FrontierCriticError::CliVersionMismatch);
    }
    let authentication_status = run_probe(
        cli_path,
        &[OsString::from("login"), OsString::from("status")],
        environment,
    )?;
    require_chatgpt_authentication(&authentication_status)?;
    Ok(authentication_status)
}

#[allow(clippy::too_many_arguments)]
fn build_diagnostic_receipt(
    execution_class: FrontierExecutionClass,
    cli_sha256: BlobId,
    cli_version: &str,
    authentication_status_utf8: Vec<u8>,
    code_signature: Option<DiagnosticCodeSignatureEvidence>,
    environment: &ReviewedEnvironment,
    argv_fingerprint: BlobId,
    invocation_fingerprint: BlobId,
    evaluator_protocol_fingerprint: BlobId,
    adapter_build_fingerprint: BlobId,
    prepared_packet_fingerprint: BlobId,
    packet: FrontierCriticPacket,
    live_challenge_fingerprint: Option<BlobId>,
    raw_jsonl_utf8: Vec<u8>,
    final_output_utf8: Vec<u8>,
    checked: &CheckedCodexJsonl,
) -> DiagnosticFrontierCriticReceipt {
    let authentication_receipt_fingerprint = fingerprint_authentication(
        cli_sha256,
        environment.fingerprint,
        &authentication_status_utf8,
    );
    let fresh_session_fingerprint = fingerprint_session(checked.thread_id());
    let executed_packet_fingerprint = packet.packet_fingerprint();
    let mut digest = Sha256::new();
    digest.update(RECEIPT_DOMAIN);
    digest.update([execution_class.tag()]);
    digest.update(cli_sha256.as_bytes());
    update_bytes(&mut digest, cli_version.as_bytes());
    update_bytes(&mut digest, &authentication_status_utf8);
    digest.update(authentication_receipt_fingerprint.as_bytes());
    if let Some(signature) = &code_signature {
        digest.update([1]);
        digest.update(signature.fingerprint.as_bytes());
        update_bytes(&mut digest, signature.team_id.as_bytes());
        update_bytes(&mut digest, signature.cdhash_sha256.as_bytes());
    } else {
        digest.update([0]);
    }
    update_bytes(&mut digest, FRONTIER_MODEL.as_bytes());
    digest.update([0]); // observed-model unavailable
    update_bytes(&mut digest, FRONTIER_REASONING_EFFORT.as_bytes());
    update_bytes(&mut digest, CODEX_DIAGNOSTIC_LOG_FILTER.as_bytes());
    digest.update(environment.fingerprint.as_bytes());
    digest.update(evaluator_protocol_fingerprint.as_bytes());
    digest.update(adapter_build_fingerprint.as_bytes());
    digest.update(argv_fingerprint.as_bytes());
    digest.update(invocation_fingerprint.as_bytes());
    digest.update(prepared_packet_fingerprint.as_bytes());
    digest.update(executed_packet_fingerprint.as_bytes());
    if let Some(challenge) = live_challenge_fingerprint {
        digest.update([1]);
        digest.update(challenge.as_bytes());
    } else {
        digest.update([0]);
    }
    digest.update(fresh_session_fingerprint.as_bytes());
    digest.update(packet.comparison_fingerprint.as_bytes());
    digest.update([
        packet.order_cell,
        packet.disclosure_policy.tag(),
        packet.prompt_injection_disposition.tag(),
    ]);
    digest.update(packet.criterion_permutation_fingerprint.as_bytes());
    update_bytes(&mut digest, &packet.prompt_utf8);
    update_bytes(&mut digest, &packet.output_schema_utf8);
    update_bytes(&mut digest, &raw_jsonl_utf8);
    update_bytes(&mut digest, &final_output_utf8);
    digest.update(checked.input_tokens().to_be_bytes());
    digest.update(checked.cached_input_tokens().to_be_bytes());
    digest.update(checked.output_tokens().to_be_bytes());
    digest.update([0, 1]); // no tool activity, complete
    let receipt_fingerprint = BlobId::from_bytes(digest.finalize().into());
    DiagnosticFrontierCriticReceipt {
        execution_class,
        cli_sha256,
        cli_version: cli_version.to_owned(),
        authentication_status_utf8,
        authentication_receipt_fingerprint,
        code_signature,
        requested_model: FRONTIER_MODEL.to_owned(),
        observed_model: FrontierModelObservation::Unavailable,
        requested_reasoning_effort: FRONTIER_REASONING_EFFORT.to_owned(),
        diagnostic_log_filter: CODEX_DIAGNOSTIC_LOG_FILTER.to_owned(),
        environment_policy_fingerprint: environment.fingerprint,
        environment_values: environment.evidence.clone(),
        evaluator_protocol_fingerprint,
        adapter_build_fingerprint,
        argv_fingerprint,
        invocation_fingerprint,
        prepared_packet_fingerprint,
        executed_packet_fingerprint,
        comparison_fingerprint: packet.comparison_fingerprint,
        order_cell: packet.order_cell,
        criterion_permutation_fingerprint: packet.criterion_permutation_fingerprint,
        disclosure_policy: packet.disclosure_policy,
        prompt_injection_disposition: packet.prompt_injection_disposition,
        live_challenge_fingerprint,
        fresh_session_fingerprint,
        exact_prompt_utf8: packet.prompt_utf8,
        output_schema_utf8: packet.output_schema_utf8,
        raw_jsonl_utf8,
        final_output_utf8,
        input_tokens: checked.input_tokens(),
        cached_input_tokens: checked.cached_input_tokens(),
        output_tokens: checked.output_tokens(),
        tool_activity_observed: false,
        complete: true,
        receipt_fingerprint,
    }
}

fn build_exec_args(workspace: &Path, schema_path: &Path, output_path: &Path) -> Vec<OsString> {
    vec![
        "exec".into(),
        "--json".into(),
        "--output-schema".into(),
        schema_path.as_os_str().to_owned(),
        "--sandbox".into(),
        "read-only".into(),
        "--ephemeral".into(),
        "--ignore-user-config".into(),
        "--ignore-rules".into(),
        "--strict-config".into(),
        "--skip-git-repo-check".into(),
        "--color".into(),
        "never".into(),
        "--cd".into(),
        workspace.as_os_str().to_owned(),
        "--output-last-message".into(),
        output_path.as_os_str().to_owned(),
        "--model".into(),
        FRONTIER_MODEL.into(),
        "--config".into(),
        format!("model_reasoning_effort=\"{FRONTIER_REASONING_EFFORT}\"").into(),
        "-".into(),
    ]
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), FrontierCriticError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(FrontierCriticError::Io)?;
    file.write_all(bytes).map_err(FrontierCriticError::Io)?;
    file.sync_all().map_err(FrontierCriticError::Io)
}

fn read_bounded_regular_file(
    path: &Path,
    maximum: usize,
    invalid: fn(usize) -> FrontierCriticError,
) -> Result<Vec<u8>, FrontierCriticError> {
    let metadata = fs::symlink_metadata(path).map_err(FrontierCriticError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX)
    {
        return Err(invalid(
            usize::try_from(metadata.len()).unwrap_or(usize::MAX),
        ));
    }
    let file = File::open(path).map_err(FrontierCriticError::Io)?;
    let read_limit = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(maximum)
            .min(maximum),
    );
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(FrontierCriticError::Io)?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(invalid(bytes.len()));
    }
    Ok(bytes)
}

fn run_probe(
    path: &Path,
    args: &[OsString],
    environment: &ReviewedEnvironment,
) -> Result<Vec<u8>, FrontierCriticError> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    environment.apply(&mut command);
    configure_process_group(&mut command);
    let process_started = Instant::now();
    let child = command.spawn().map_err(FrontierCriticError::Spawn)?;
    let output = capture_child(
        child,
        None,
        MAX_PROBE_BYTES,
        MAX_PROBE_BYTES,
        FRONTIER_PROBE_TIMEOUT,
        process_started,
    )?;
    if !output.status.success() {
        return Err(FrontierCriticError::ProbeFailed);
    }
    match (output.stdout.is_empty(), output.stderr.is_empty()) {
        (false, true) => Ok(output.stdout),
        (true, false) => Ok(output.stderr),
        (true, true) | (false, false) => Err(FrontierCriticError::ProbeFailed),
    }
}

fn require_chatgpt_authentication(bytes: &[u8]) -> Result<(), FrontierCriticError> {
    if trim_single_line(bytes)? == "Logged in using ChatGPT" {
        Ok(())
    } else {
        Err(FrontierCriticError::NotChatGptAuthenticated)
    }
}

struct CapturedChild {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildTermination {
    Completed,
    TimedOut,
    OutputOverflow,
}

#[allow(clippy::too_many_arguments)]
fn capture_child(
    mut child: Child,
    stdin_payload: Option<&[u8]>,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
    process_started: Instant,
) -> Result<CapturedChild, FrontierCriticError> {
    let stdin = if stdin_payload.is_some() {
        Some(
            child
                .stdin
                .take()
                .ok_or(FrontierCriticError::MissingStdin)?,
        )
    } else {
        None
    };
    let stdout = child
        .stdout
        .take()
        .ok_or(FrontierCriticError::MissingStdout)?;
    let stderr = child
        .stderr
        .take()
        .ok_or(FrontierCriticError::MissingStderr)?;
    let stdout_overflow = AtomicBool::new(false);
    let stderr_overflow = AtomicBool::new(false);

    thread::scope(|scope| {
        let stdin_writer = stdin.zip(stdin_payload).map(|(mut stdin, payload)| {
            scope.spawn(move || {
                let result = stdin.write_all(payload);
                drop(stdin);
                result
            })
        });
        let stdout_reader = scope.spawn(|| read_bounded(stdout, stdout_limit, &stdout_overflow));
        let stderr_reader = scope.spawn(|| read_bounded(stderr, stderr_limit, &stderr_overflow));
        let deadline = process_started
            .checked_add(timeout)
            .ok_or(FrontierCriticError::InvalidConfig)?;
        let (status, termination) = loop {
            if let Some(status) = child.try_wait().map_err(FrontierCriticError::Io)? {
                let termination = if stdout_overflow.load(Ordering::Acquire)
                    || stderr_overflow.load(Ordering::Acquire)
                {
                    ChildTermination::OutputOverflow
                } else {
                    ChildTermination::Completed
                };
                break (status, termination);
            }
            if stdout_overflow.load(Ordering::Acquire) || stderr_overflow.load(Ordering::Acquire) {
                terminate_process_group(&mut child)?;
                let status = child.wait().map_err(FrontierCriticError::Io)?;
                break (status, ChildTermination::OutputOverflow);
            }
            if Instant::now() >= deadline {
                terminate_process_group(&mut child)?;
                let status = child.wait().map_err(FrontierCriticError::Io)?;
                break (status, ChildTermination::TimedOut);
            }
            thread::sleep(CHILD_POLL_INTERVAL);
        };

        let stdin_result = stdin_writer
            .map(|writer| {
                writer
                    .join()
                    .map_err(|_| FrontierCriticError::WriterPanicked)?
                    .map_err(FrontierCriticError::Io)
            })
            .transpose();
        let stdout = stdout_reader
            .join()
            .map_err(|_| FrontierCriticError::ReaderPanicked)?
            .map_err(FrontierCriticError::Io)?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| FrontierCriticError::ReaderPanicked)?
            .map_err(FrontierCriticError::Io)?;

        match termination {
            ChildTermination::TimedOut => Err(FrontierCriticError::TimedOut),
            ChildTermination::OutputOverflow => Err(FrontierCriticError::OutputTooLarge),
            ChildTermination::Completed => {
                stdin_result?;
                Ok(CapturedChild {
                    status,
                    stdout,
                    stderr,
                })
            }
        }
    })
}

fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    overflow: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        let remaining = limit.saturating_sub(captured.len());
        if read > remaining {
            captured.extend_from_slice(&buffer[..remaining]);
            overflow.store(true, Ordering::Release);
            return Ok(captured);
        }
        captured.extend_from_slice(&buffer[..read]);
    }
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
}

fn terminate_process_group(child: &mut Child) -> Result<(), FrontierCriticError> {
    #[cfg(unix)]
    {
        use nix::{
            errno::Errno,
            sys::signal::{Signal, killpg},
            unistd::Pid,
        };

        let raw_pid = i32::try_from(child.id()).map_err(|_| FrontierCriticError::InvalidChildId)?;
        match killpg(Pid::from_raw(raw_pid), Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => {
                let _ = child.kill();
                Err(FrontierCriticError::ProcessGroupTermination(error))
            }
        }
    }
    #[cfg(not(unix))]
    {
        child.kill().map_err(FrontierCriticError::Io)
    }
}

fn trim_single_line(bytes: &[u8]) -> Result<&str, FrontierCriticError> {
    let text = std::str::from_utf8(bytes).map_err(|_| FrontierCriticError::ProbeFailed)?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() || trimmed.contains(['\r', '\n']) {
        return Err(FrontierCriticError::ProbeFailed);
    }
    Ok(trimmed)
}

fn validate_utf8_bound(
    bytes: &[u8],
    maximum: usize,
    error: fn(usize) -> FrontierCriticError,
) -> Result<(), FrontierCriticError> {
    if bytes.is_empty() || bytes.len() > maximum || std::str::from_utf8(bytes).is_err() {
        return Err(error(bytes.len()));
    }
    Ok(())
}

fn validate_closed_output_schema(schema: &Value) -> Result<(), FrontierCriticError> {
    let object = schema
        .as_object()
        .ok_or(FrontierCriticError::OpenOutputSchema)?;
    if object.get("type").and_then(Value::as_str) != Some("object")
        || object.get("additionalProperties").and_then(Value::as_bool) != Some(false)
    {
        return Err(FrontierCriticError::OpenOutputSchema);
    }
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .filter(|properties| !properties.is_empty())
        .ok_or(FrontierCriticError::OpenOutputSchema)?;
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .ok_or(FrontierCriticError::OpenOutputSchema)?;
    let required = required
        .iter()
        .map(Value::as_str)
        .collect::<Option<BTreeSet<_>>>()
        .ok_or(FrontierCriticError::OpenOutputSchema)?;
    let property_names = properties
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if required.len() != properties.len() || required != property_names {
        return Err(FrontierCriticError::OpenOutputSchema);
    }
    let _ = Validator::new(schema).map_err(|_| FrontierCriticError::OpenOutputSchema)?;
    Ok(())
}

fn verify_private_workspace(
    workspace: &Path,
    schema_path: &Path,
    output_path: &Path,
) -> Result<(), FrontierCriticError> {
    let expected = [schema_path, output_path]
        .into_iter()
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(workspace).map_err(FrontierCriticError::Io)? {
        if actual.len() >= MAX_PRIVATE_WORKSPACE_ENTRIES {
            return Err(FrontierCriticError::WorkspaceMutation);
        }
        let path = entry.map_err(FrontierCriticError::Io)?.path();
        let metadata = fs::symlink_metadata(&path).map_err(FrontierCriticError::Io)?;
        if !metadata.file_type().is_file() || !actual.insert(path) {
            return Err(FrontierCriticError::WorkspaceMutation);
        }
    }
    if actual != expected {
        return Err(FrontierCriticError::WorkspaceMutation);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentValueClass {
    FixedNonSecret,
    HashedNonSecret,
    HashedPrivatePath,
    HashedSecretBearing,
}

impl EnvironmentValueClass {
    const fn tag(self) -> u8 {
        match self {
            Self::FixedNonSecret => 0,
            Self::HashedNonSecret => 1,
            Self::HashedPrivatePath => 2,
            Self::HashedSecretBearing => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnvironmentValueEvidence {
    name: String,
    value_fingerprint: BlobId,
    class: EnvironmentValueClass,
}

impl EnvironmentValueEvidence {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn value_fingerprint(&self) -> BlobId {
        self.value_fingerprint
    }

    pub const fn class(&self) -> EnvironmentValueClass {
        self.class
    }
}

#[derive(Clone)]
struct ReviewedEnvironment {
    variables: Vec<(OsString, OsString)>,
    evidence: Vec<EnvironmentValueEvidence>,
    fingerprint: BlobId,
}

impl fmt::Debug for ReviewedEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReviewedEnvironment")
            .field("evidence", &self.evidence)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl ReviewedEnvironment {
    fn capture() -> Result<Self, FrontierCriticError> {
        let mut variables = Vec::new();
        let mut evidence = Vec::new();
        push_environment_value(
            &mut variables,
            &mut evidence,
            "PATH",
            OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"),
            EnvironmentValueClass::FixedNonSecret,
        );
        push_environment_value(
            &mut variables,
            &mut evidence,
            "RUST_LOG",
            OsString::from(CODEX_DIAGNOSTIC_LOG_FILTER),
            EnvironmentValueClass::FixedNonSecret,
        );
        push_environment_value(
            &mut variables,
            &mut evidence,
            "TERM",
            OsString::from("dumb"),
            EnvironmentValueClass::FixedNonSecret,
        );

        let home =
            env::var_os("HOME").ok_or(FrontierCriticError::MissingRequiredEnvironment("HOME"))?;
        push_environment_value(
            &mut variables,
            &mut evidence,
            "HOME",
            home,
            EnvironmentValueClass::HashedPrivatePath,
        );
        for (name, class) in [
            ("CODEX_HOME", EnvironmentValueClass::HashedPrivatePath),
            ("TMPDIR", EnvironmentValueClass::HashedPrivatePath),
            ("SSL_CERT_FILE", EnvironmentValueClass::HashedPrivatePath),
            ("SSL_CERT_DIR", EnvironmentValueClass::HashedPrivatePath),
            ("LANG", EnvironmentValueClass::HashedNonSecret),
            ("LC_ALL", EnvironmentValueClass::HashedNonSecret),
            ("TZ", EnvironmentValueClass::HashedNonSecret),
            ("HTTPS_PROXY", EnvironmentValueClass::HashedSecretBearing),
            ("HTTP_PROXY", EnvironmentValueClass::HashedSecretBearing),
            ("ALL_PROXY", EnvironmentValueClass::HashedSecretBearing),
            ("NO_PROXY", EnvironmentValueClass::HashedSecretBearing),
            ("https_proxy", EnvironmentValueClass::HashedSecretBearing),
            ("http_proxy", EnvironmentValueClass::HashedSecretBearing),
            ("all_proxy", EnvironmentValueClass::HashedSecretBearing),
            ("no_proxy", EnvironmentValueClass::HashedSecretBearing),
        ] {
            if let Some(value) = env::var_os(name) {
                push_environment_value(&mut variables, &mut evidence, name, value, class);
            }
        }
        validate_effective_environment(&variables)?;
        let mut digest = Sha256::new();
        digest.update(ENVIRONMENT_DOMAIN);
        digest.update((evidence.len() as u64).to_be_bytes());
        for item in &evidence {
            update_bytes(&mut digest, item.name.as_bytes());
            digest.update([item.class.tag()]);
            digest.update(item.value_fingerprint.as_bytes());
        }
        let fingerprint = BlobId::from_bytes(digest.finalize().into());
        Ok(Self {
            variables,
            evidence,
            fingerprint,
        })
    }

    fn apply(&self, command: &mut Command) {
        command.env_clear();
        command.envs(self.variables.iter().cloned());
    }
}

fn validate_effective_environment(
    variables: &[(OsString, OsString)],
) -> Result<(), FrontierCriticError> {
    let mut total = 0_usize;
    for (name, value) in variables {
        let name = name.as_encoded_bytes();
        let value = value.as_encoded_bytes();
        if name.is_empty()
            || value.is_empty()
            || name.contains(&0)
            || value.contains(&0)
            || value.len() > MAX_ENVIRONMENT_VALUE_BYTES
        {
            return Err(FrontierCriticError::InvalidReviewedEnvironment);
        }
        total = total
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or(FrontierCriticError::InvalidReviewedEnvironment)?;
        if total > MAX_EFFECTIVE_ENVIRONMENT_BYTES {
            return Err(FrontierCriticError::InvalidReviewedEnvironment);
        }
    }
    Ok(())
}

fn push_environment_value(
    variables: &mut Vec<(OsString, OsString)>,
    evidence: &mut Vec<EnvironmentValueEvidence>,
    name: &'static str,
    value: OsString,
    class: EnvironmentValueClass,
) {
    let value_fingerprint = BlobId::digest(value.as_encoded_bytes());
    variables.push((OsString::from(name), value));
    evidence.push(EnvironmentValueEvidence {
        name: name.to_owned(),
        value_fingerprint,
        class,
    });
}

struct OwnedCli {
    _directory: TempDir,
    path: PathBuf,
    sha256: BlobId,
}

impl OwnedCli {
    fn copy_exact(source: &Path, expected_sha256: BlobId) -> Result<Self, FrontierCriticError> {
        let source_metadata = fs::symlink_metadata(source).map_err(FrontierCriticError::Io)?;
        if !source_metadata.file_type().is_file()
            || source_metadata.len() == 0
            || source_metadata.len() > MAX_CLI_BINARY_BYTES
        {
            return Err(FrontierCriticError::InvalidCliFile);
        }
        let source_file = File::open(source).map_err(FrontierCriticError::Io)?;
        let opened_metadata = source_file.metadata().map_err(FrontierCriticError::Io)?;
        if !opened_metadata.is_file()
            || opened_metadata.len() == 0
            || opened_metadata.len() > MAX_CLI_BINARY_BYTES
        {
            return Err(FrontierCriticError::InvalidCliFile);
        }
        let directory = TempDir::new().map_err(FrontierCriticError::Io)?;
        let path = directory.path().join("pinned-codex");
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(FrontierCriticError::Io)?;
        let mut limited_source = source_file.take(MAX_CLI_BINARY_BYTES.saturating_add(1));
        let copied = std::io::copy(&mut limited_source, &mut destination)
            .map_err(FrontierCriticError::Io)?;
        if copied != opened_metadata.len() || copied > MAX_CLI_BINARY_BYTES {
            return Err(FrontierCriticError::InvalidCliFile);
        }
        destination.sync_all().map_err(FrontierCriticError::Io)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))
            .map_err(FrontierCriticError::Io)?;
        drop(destination);
        let actual = hash_bounded_regular_file(&path, MAX_CLI_BINARY_BYTES)?;
        if actual != expected_sha256 {
            return Err(FrontierCriticError::CliDigestMismatch);
        }
        Ok(Self {
            _directory: directory,
            path,
            sha256: actual,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn revalidate(&self) -> Result<(), FrontierCriticError> {
        if hash_bounded_regular_file(&self.path, MAX_CLI_BINARY_BYTES)? == self.sha256 {
            Ok(())
        } else {
            Err(FrontierCriticError::CliDigestMismatch)
        }
    }
}

fn hash_bounded_regular_file(path: &Path, maximum: u64) -> Result<BlobId, FrontierCriticError> {
    let metadata = fs::symlink_metadata(path).map_err(FrontierCriticError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum {
        return Err(FrontierCriticError::InvalidCliFile);
    }
    let mut file = File::open(path).map_err(FrontierCriticError::Io)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(FrontierCriticError::Io)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(read).map_err(|_| FrontierCriticError::InvalidCliFile)?)
            .ok_or(FrontierCriticError::InvalidCliFile)?;
        if total > maximum {
            return Err(FrontierCriticError::InvalidCliFile);
        }
        digest.update(&buffer[..read]);
    }
    if total != metadata.len() {
        return Err(FrontierCriticError::CliChangedDuringRead);
    }
    Ok(BlobId::from_bytes(digest.finalize().into()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PinnedCodeSignature {
    fingerprint: BlobId,
}

impl PinnedCodeSignature {
    fn evidence(self) -> DiagnosticCodeSignatureEvidence {
        DiagnosticCodeSignatureEvidence {
            fingerprint: self.fingerprint,
            team_id: PINNED_CHATGPT_TEAM_ID.to_owned(),
            cdhash_sha256: PINNED_CHATGPT_CODEX_CDHASH_SHA256.to_owned(),
        }
    }
}

#[cfg(target_os = "macos")]
fn verify_pinned_code_signature(path: &Path) -> Result<PinnedCodeSignature, FrontierCriticError> {
    let environment = ReviewedEnvironment::capture()?;
    run_fixed_system_command(
        Path::new("/usr/bin/codesign"),
        &[
            OsString::from("--verify"),
            OsString::from("--strict"),
            OsString::from("--verbose=4"),
            path.as_os_str().to_owned(),
        ],
        &environment,
    )?;
    let details = run_fixed_system_command(
        Path::new("/usr/bin/codesign"),
        &[
            OsString::from("-d"),
            OsString::from("--verbose=4"),
            path.as_os_str().to_owned(),
        ],
        &environment,
    )?;
    let details =
        std::str::from_utf8(&details).map_err(|_| FrontierCriticError::InvalidCodeSignature)?;
    let expected_team = format!("TeamIdentifier={PINNED_CHATGPT_TEAM_ID}");
    let expected_cdhash =
        format!("CandidateCDHashFull sha256={PINNED_CHATGPT_CODEX_CDHASH_SHA256}");
    let expected_authority =
        format!("Authority=Developer ID Application: OpenAI OpCo, LLC ({PINNED_CHATGPT_TEAM_ID})");
    if !details.lines().any(|line| line == expected_team)
        || !details.lines().any(|line| line == expected_cdhash)
        || !details.lines().any(|line| line == expected_authority)
        || !details.lines().any(|line| line == "Identifier=codex")
        || !details
            .lines()
            .any(|line| line == "Hash type=sha256 size=32")
    {
        return Err(FrontierCriticError::CodeSignatureMismatch);
    }
    let mut digest = Sha256::new();
    digest.update(CODE_SIGNATURE_DOMAIN);
    update_bytes(&mut digest, PINNED_CHATGPT_TEAM_ID.as_bytes());
    update_bytes(&mut digest, PINNED_CHATGPT_CODEX_CDHASH_SHA256.as_bytes());
    update_bytes(&mut digest, b"Identifier=codex");
    update_bytes(&mut digest, b"Hash type=sha256 size=32");
    Ok(PinnedCodeSignature {
        fingerprint: BlobId::from_bytes(digest.finalize().into()),
    })
}

#[cfg(not(target_os = "macos"))]
fn verify_pinned_code_signature(_path: &Path) -> Result<PinnedCodeSignature, FrontierCriticError> {
    Err(FrontierCriticError::UnsupportedPinnedPlatform)
}

fn run_fixed_system_command(
    path: &Path,
    args: &[OsString],
    environment: &ReviewedEnvironment,
) -> Result<Vec<u8>, FrontierCriticError> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    environment.apply(&mut command);
    configure_process_group(&mut command);
    let process_started = Instant::now();
    let child = command.spawn().map_err(FrontierCriticError::Spawn)?;
    let output = capture_child(
        child,
        None,
        MAX_PROBE_BYTES,
        MAX_PROBE_BYTES,
        FRONTIER_PROBE_TIMEOUT,
        process_started,
    )?;
    if !output.status.success() {
        return Err(FrontierCriticError::InvalidCodeSignature);
    }
    match (output.stdout.is_empty(), output.stderr.is_empty()) {
        (false, true) => Ok(output.stdout),
        (true, false) => Ok(output.stderr),
        (true, true) => Ok(Vec::new()),
        (false, false) => Err(FrontierCriticError::InvalidCodeSignature),
    }
}

struct LiveChallenge {
    bytes: [u8; 32],
    fingerprint: BlobId,
}

impl LiveChallenge {
    fn fresh() -> Result<Self, FrontierCriticError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| FrontierCriticError::RandomUnavailable)?;
        let mut digest = Sha256::new();
        digest.update(CHALLENGE_DOMAIN);
        digest.update(bytes);
        Ok(Self {
            bytes,
            fingerprint: BlobId::from_bytes(digest.finalize().into()),
        })
    }

    fn hex(&self) -> String {
        hex::encode(self.bytes)
    }
}

fn pinned_cli_sha256() -> BlobId {
    BlobId::from_str(PINNED_CHATGPT_CODEX_SHA256).expect("reviewed SHA-256 is canonical")
}

fn evaluator_protocol_fingerprint() -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PROTOCOL_DOMAIN);
    update_bytes(&mut digest, FRONTIER_ADAPTER_PROTOCOL.as_bytes());
    update_bytes(&mut digest, FRONTIER_MODEL.as_bytes());
    update_bytes(&mut digest, FRONTIER_REASONING_EFFORT.as_bytes());
    update_bytes(&mut digest, CODEX_DIAGNOSTIC_LOG_FILTER.as_bytes());
    update_bytes(&mut digest, LIVE_CHALLENGE_FIELD.as_bytes());
    digest.update((MAX_FRONTIER_PROMPT_BYTES as u64).to_be_bytes());
    digest.update((MAX_FRONTIER_SCHEMA_BYTES as u64).to_be_bytes());
    digest.update((MAX_FRONTIER_OUTPUT_BYTES as u64).to_be_bytes());
    digest.update((MAX_CODEX_JSONL_BYTES as u64).to_be_bytes());
    digest.update(FRONTIER_EXEC_TIMEOUT.as_secs().to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn adapter_build_fingerprint() -> BlobId {
    let mut digest = Sha256::new();
    digest.update(BUILD_DOMAIN);
    update_bytes(&mut digest, env!("CARGO_PKG_NAME").as_bytes());
    update_bytes(&mut digest, env!("CARGO_PKG_VERSION").as_bytes());
    update_bytes(&mut digest, FRONTIER_ADAPTER_PROTOCOL.as_bytes());
    for source in [
        include_bytes!("lib.rs").as_slice(),
        include_bytes!("runner.rs").as_slice(),
        include_bytes!("jsonl.rs").as_slice(),
        include_bytes!("blind_pair.rs").as_slice(),
        include_bytes!("criterion.rs").as_slice(),
        include_bytes!("prompt_policy.rs").as_slice(),
        include_bytes!("../Cargo.toml").as_slice(),
        include_bytes!("../../loom-eval/src/blind.rs").as_slice(),
        include_bytes!("../../loom-eval/src/evidence.rs").as_slice(),
        include_bytes!("../../../Cargo.lock").as_slice(),
    ] {
        update_bytes(&mut digest, source);
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_authentication(cli: BlobId, environment: BlobId, bytes: &[u8]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(AUTHENTICATION_DOMAIN);
    digest.update(cli.as_bytes());
    digest.update(environment.as_bytes());
    update_bytes(&mut digest, bytes);
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_invocation(
    cli: BlobId,
    argv: BlobId,
    environment: BlobId,
    protocol: BlobId,
    build: BlobId,
    packet: BlobId,
    challenge: Option<BlobId>,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(INVOCATION_DOMAIN);
    digest.update(cli.as_bytes());
    digest.update(argv.as_bytes());
    digest.update(environment.as_bytes());
    digest.update(protocol.as_bytes());
    digest.update(build.as_bytes());
    digest.update(packet.as_bytes());
    if let Some(challenge) = challenge {
        digest.update([1]);
        digest.update(challenge.as_bytes());
    } else {
        digest.update([0]);
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_session(thread_id: &str) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(SESSION_DOMAIN);
    update_bytes(&mut digest, thread_id.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_os_values(domain: &[u8], values: &[OsString]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        update_bytes(&mut digest, value.as_encoded_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[derive(Debug, Error)]
pub enum FrontierCriticError {
    #[error("frontier critic configuration is invalid")]
    InvalidConfig,
    #[error("frontier prompt byte length {0} is invalid")]
    InvalidPrompt(usize),
    #[error("frontier output-schema byte length {0} is invalid")]
    InvalidSchema(usize),
    #[error("frontier final-output byte length {0} is invalid")]
    InvalidFinalOutput(usize),
    #[error("frontier comparison order cell {0} is outside 0..4")]
    InvalidOrderCell(u8),
    #[error("frontier JSON Schema is malformed")]
    MalformedSchema(#[source] serde_json::Error),
    #[error("frontier JSON Schema must be a closed object with every property required")]
    OpenOutputSchema,
    #[error("frontier JSON Schema reserves the live challenge field")]
    ReservedChallengeField,
    #[error("failed to encode the challenge-bound JSON Schema")]
    ChallengeSchemaEncoding(#[source] serde_json::Error),
    #[error("frontier final output is malformed JSON")]
    MalformedFinalOutput(#[source] serde_json::Error),
    #[error("frontier final output violates its exact JSON Schema")]
    SchemaViolation,
    #[error("configured Codex CLI is not a bounded regular file")]
    InvalidCliFile,
    #[error("Codex CLI changed while its exact bytes were being read")]
    CliChangedDuringRead,
    #[error("Codex CLI digest does not match the pinned executable")]
    CliDigestMismatch,
    #[error("Codex CLI version does not match the pinned version")]
    CliVersionMismatch,
    #[error("Codex CLI probe failed or emitted an ambiguous stream")]
    ProbeFailed,
    #[error("Codex CLI is not authenticated through ChatGPT")]
    NotChatGptAuthenticated,
    #[error("the pinned ChatGPT-bundled diagnostic is unavailable on this platform")]
    UnsupportedPinnedPlatform,
    #[error("pinned Codex code signature is invalid")]
    InvalidCodeSignature,
    #[error("pinned Codex code signature facts do not match the reviewed build")]
    CodeSignatureMismatch,
    #[error("required reviewed environment variable {0} is absent")]
    MissingRequiredEnvironment(&'static str),
    #[error("reviewed environment contains an empty, unbounded, or invalid value")]
    InvalidReviewedEnvironment,
    #[error("OS randomness is unavailable for a fresh live challenge")]
    RandomUnavailable,
    #[error("failed to spawn Codex CLI")]
    Spawn(#[source] std::io::Error),
    #[error("Codex CLI stdin was not piped")]
    MissingStdin,
    #[error("Codex CLI stdout was not piped")]
    MissingStdout,
    #[error("Codex CLI stderr was not piped")]
    MissingStderr,
    #[error("Codex CLI stdin writer panicked")]
    WriterPanicked,
    #[error("Codex CLI output reader panicked")]
    ReaderPanicked,
    #[error("Codex CLI child PID cannot represent a process group")]
    InvalidChildId,
    #[cfg(unix)]
    #[error("failed to terminate the complete Codex process group")]
    ProcessGroupTermination(#[source] nix::errno::Errno),
    #[error("Codex CLI exceeded its pinned execution timeout")]
    TimedOut,
    #[error("Codex CLI execution failed with code {code:?}")]
    ExecFailed {
        code: Option<i32>,
        stderr_fingerprint: BlobId,
    },
    #[error("Codex CLI output exceeds its bound")]
    OutputTooLarge,
    #[error("Codex CLI emitted unexpected stderr")]
    UnexpectedStderr,
    #[error("Codex CLI final-output file differs from its final JSONL message")]
    FinalOutputMismatch,
    #[error("Codex CLI mutated its private packet workspace")]
    WorkspaceMutation,
    #[error(transparent)]
    Jsonl(#[from] CodexJsonlError),
    #[error("frontier critic I/O failed")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> FrontierCriticPacket {
        FrontierCriticPacket::new(
            BlobId::digest(b"comparison"),
            3,
            BlobId::digest(b"permutation"),
            CriticDisclosurePolicy::ManuscriptOnly,
            b"Compare candidate A and candidate B.".to_vec(),
            br#"{"type":"object","required":["winner"],"properties":{"winner":{"enum":["A","B","tie"]}},"additionalProperties":false}"#.to_vec(),
        )
        .expect("packet")
    }

    fn ok_packet() -> FrontierCriticPacket {
        FrontierCriticPacket::new(
            BlobId::digest(b"comparison"),
            0,
            BlobId::digest(b"permutation"),
            CriticDisclosurePolicy::ManuscriptOnly,
            b"Return ok.".to_vec(),
            br#"{"type":"object","required":["ok"],"properties":{"ok":{"type":"boolean","const":true}},"additionalProperties":false}"#.to_vec(),
        )
        .expect("packet")
    }

    #[cfg(unix)]
    fn run_fake_cli(
        script: &[u8],
        expected_version: &str,
    ) -> Result<DiagnosticFrontierCriticReceipt, FrontierCriticError> {
        let directory = TempDir::new().expect("tempdir");
        let cli = directory.path().join("diagnostic-cli");
        fs::write(&cli, script).expect("script");
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).expect("executable");
        let config =
            DiagnosticFrontierCriticConfig::pinned(cli, BlobId::digest(script), expected_version)
                .expect("diagnostic config");
        run_diagnostic_frontier_critic(&config, ok_packet())
    }

    #[cfg(unix)]
    fn fake_exec_script(exec_body: &str) -> Vec<u8> {
        format!(
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'diagnostic-cli 1'
  exit 0
fi
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  printf '%s\n' 'Logged in using ChatGPT'
  exit 0
fi
output=''
workspace=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output-last-message" ]; then
    shift
    output="$1"
  elif [ "$1" = "--cd" ]; then
    shift
    workspace="$1"
  fi
  shift
done
{exec_body}
"#
        )
        .into_bytes()
    }

    #[test]
    fn packet_rejects_unbounded_invalid_and_non_schema_inputs() {
        let schema = br#"{"type":"object","required":["winner"],"properties":{"winner":{"enum":["A","B","tie"]}},"additionalProperties":false}"#;
        assert!(matches!(
            FrontierCriticPacket::new(
                BlobId::digest(b"comparison"),
                4,
                BlobId::digest(b"permutation"),
                CriticDisclosurePolicy::ManuscriptOnly,
                b"prompt".to_vec(),
                schema.to_vec(),
            ),
            Err(FrontierCriticError::InvalidOrderCell(4))
        ));
        for open_schema in [
            br"{}".as_slice(),
            br#"{"type":"object","properties":{"winner":{"type":"string"}},"required":["winner"]}"#,
            br#"{"type":"object","additionalProperties":false,"properties":{"winner":{"type":"string"}},"required":[]}"#,
        ] {
            assert!(matches!(
                FrontierCriticPacket::new(
                    BlobId::digest(b"comparison"),
                    0,
                    BlobId::digest(b"permutation"),
                    CriticDisclosurePolicy::ManuscriptOnly,
                    b"prompt".to_vec(),
                    open_schema.to_vec(),
                ),
                Err(FrontierCriticError::OpenOutputSchema)
            ));
        }
    }

    #[test]
    fn diagnostic_config_requires_an_absolute_pinned_executable_path() {
        assert!(
            DiagnosticFrontierCriticConfig::pinned(
                PathBuf::from("codex"),
                BlobId::digest(b"binary"),
                "codex-cli 0.146.0",
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_receipt_is_cloneable_diagnostic_and_argv_is_exact() {
        let script = br#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'diagnostic-cli 1'
  exit 0
fi
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  printf '%s\n' 'Logged in using ChatGPT'
  exit 0
fi
if [ "$#" -ne 22 ] ||
   [ "$1" != "exec" ] || [ "$2" != "--json" ] ||
   [ "$3" != "--output-schema" ] || [ "$5" != "--sandbox" ] ||
   [ "$6" != "read-only" ] || [ "$7" != "--ephemeral" ] ||
   [ "$8" != "--ignore-user-config" ] || [ "$9" != "--ignore-rules" ] ||
   [ "${10}" != "--strict-config" ] || [ "${11}" != "--skip-git-repo-check" ] ||
   [ "${12}" != "--color" ] || [ "${13}" != "never" ] ||
   [ "${14}" != "--cd" ] || [ "${16}" != "--output-last-message" ] ||
   [ "${18}" != "--model" ] || [ "${19}" != "gpt-5.6-sol" ] ||
   [ "${20}" != "--config" ] || [ "${21}" != 'model_reasoning_effort="xhigh"' ] ||
   [ "${22}" != "-" ]; then
  exit 90
fi
schema="$4"
workspace="${15}"
output="${17}"
if [ "$schema" != "$workspace/output-schema.json" ] ||
   [ "$output" != "$workspace/final-output.json" ]; then
  exit 91
fi
if [ "$(/bin/cat)" != "Return ok." ]; then
  exit 92
fi
printf '%s' '{"ok":true}' > "$output"
printf '%s\n' \
  '{"type":"thread.started","thread_id":"diagnostic-thread"}' \
  '{"type":"turn.started"}' \
  '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"ok\":true}"}}' \
  '{"type":"turn.completed","usage":{"input_tokens":2,"cached_input_tokens":0,"output_tokens":1}}'
"#;
        let receipt = run_fake_cli(script, "diagnostic-cli 1").expect("diagnostic execution");
        let duplicate = receipt.clone();
        assert_eq!(
            receipt.execution_class(),
            FrontierExecutionClass::CallerPinnedDiagnostic
        );
        assert_eq!(receipt.live_challenge_fingerprint(), None);
        assert_eq!(
            receipt.observed_model(),
            FrontierModelObservation::Unavailable
        );
        assert_eq!(receipt.final_output_utf8(), b"{\"ok\":true}");
        assert_eq!(
            receipt.receipt_fingerprint(),
            duplicate.receipt_fingerprint()
        );
        assert!(receipt.code_signature().is_none());
        let debug = format!("{receipt:?}");
        assert!(!debug.contains("Return ok."));
        assert!(!debug.contains("{\"ok\":true}"));
        let serialized = serde_json::to_value(&receipt).expect("serializable diagnostic");
        assert_eq!(
            serialized["exact_prompt_utf8"],
            serde_json::json!([82, 101, 116, 117, 114, 110, 32, 111, 107, 46])
        );
        assert_eq!(
            serialized["observed_model"],
            serde_json::json!("unavailable")
        );
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_rejects_wrong_version_and_non_chatgpt_authentication() {
        let wrong_version = br#"#!/bin/sh
if [ "$1" = "--version" ]; then printf '%s\n' 'diagnostic-cli 2'; exit 0; fi
exit 99
"#;
        assert!(matches!(
            run_fake_cli(wrong_version, "diagnostic-cli 1"),
            Err(FrontierCriticError::CliVersionMismatch)
        ));

        let api_key_auth = br#"#!/bin/sh
if [ "$1" = "--version" ]; then printf '%s\n' 'diagnostic-cli 1'; exit 0; fi
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  printf '%s\n' 'Logged in using an API key'
  exit 0
fi
exit 99
"#;
        assert!(matches!(
            run_fake_cli(api_key_auth, "diagnostic-cli 1"),
            Err(FrontierCriticError::NotChatGptAuthenticated)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_rejects_tool_activity_malformed_and_incomplete_jsonl() {
        let tool = fake_exec_script(
            r#"printf '%s' '{"ok":true}' > "$output"
printf '%s\n' \
  '{"type":"thread.started","thread_id":"fake-thread"}' \
  '{"type":"turn.started"}' \
  '{"type":"item.started","item":{"id":"tool-1","type":"command_execution"}}' \
  '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"ok\":true}"}}' \
  '{"type":"turn.completed","usage":{"input_tokens":2,"cached_input_tokens":0,"output_tokens":1}}'"#,
        );
        assert!(matches!(
            run_fake_cli(&tool, "diagnostic-cli 1"),
            Err(FrontierCriticError::Jsonl(
                CodexJsonlError::ToolOrUnknownItem
            ))
        ));

        let malformed = fake_exec_script(
            r#"printf '%s' '{"ok":true}' > "$output"
printf '%s\n' 'not-json'"#,
        );
        assert!(matches!(
            run_fake_cli(&malformed, "diagnostic-cli 1"),
            Err(FrontierCriticError::Jsonl(CodexJsonlError::MalformedJson(
                _
            )))
        ));

        let incomplete = fake_exec_script(
            r#"printf '%s' '{"ok":true}' > "$output"
printf '%s\n' \
  '{"type":"thread.started","thread_id":"fake-thread"}' \
  '{"type":"turn.started"}' \
  '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"ok\":true}"}}'"#,
        );
        assert!(matches!(
            run_fake_cli(&incomplete, "diagnostic-cli 1"),
            Err(FrontierCriticError::Jsonl(
                CodexJsonlError::IncompleteLifecycle
            ))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_rejects_stderr_output_mismatch_schema_violation_and_workspace_mutation() {
        let valid_jsonl = r#"printf '%s\n' \
  '{"type":"thread.started","thread_id":"fake-thread"}' \
  '{"type":"turn.started"}' \
  '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"ok\":true}"}}' \
  '{"type":"turn.completed","usage":{"input_tokens":2,"cached_input_tokens":0,"output_tokens":1}}'"#;

        let stderr = fake_exec_script(&format!(
            "printf '%s' '{{\"ok\":true}}' > \"$output\"\nprintf '%s' 'unexpected' >&2\n{valid_jsonl}"
        ));
        assert!(matches!(
            run_fake_cli(&stderr, "diagnostic-cli 1"),
            Err(FrontierCriticError::UnexpectedStderr)
        ));

        let mismatch = fake_exec_script(&format!(
            "printf '%s' '{{\"ok\":false}}' > \"$output\"\n{valid_jsonl}"
        ));
        assert!(matches!(
            run_fake_cli(&mismatch, "diagnostic-cli 1"),
            Err(FrontierCriticError::FinalOutputMismatch)
        ));

        let schema_violation_jsonl = r#"printf '%s\n' \
  '{"type":"thread.started","thread_id":"fake-thread"}' \
  '{"type":"turn.started"}' \
  '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"ok\":false}"}}' \
  '{"type":"turn.completed","usage":{"input_tokens":2,"cached_input_tokens":0,"output_tokens":1}}'"#;
        let schema_violation = fake_exec_script(&format!(
            "printf '%s' '{{\"ok\":false}}' > \"$output\"\n{schema_violation_jsonl}"
        ));
        assert!(matches!(
            run_fake_cli(&schema_violation, "diagnostic-cli 1"),
            Err(FrontierCriticError::SchemaViolation)
        ));

        let mutation = fake_exec_script(&format!(
            "printf '%s' '{{\"ok\":true}}' > \"$output\"\nprintf '%s' x > \"$workspace/mutation\"\n{valid_jsonl}"
        ));
        assert!(matches!(
            run_fake_cli(&mutation, "diagnostic-cli 1"),
            Err(FrontierCriticError::WorkspaceMutation)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn fake_cli_rejects_explicitly_contradictory_model_or_config_metadata() {
        let contradiction = fake_exec_script(
            r#"printf '%s' '{"ok":true}' > "$output"
printf '%s\n' \
  '{"type":"thread.started","thread_id":"fake-thread","model":"not-the-requested-model"}' \
  '{"type":"turn.started"}' \
  '{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"ok\":true}"}}' \
  '{"type":"turn.completed","usage":{"input_tokens":2,"cached_input_tokens":0,"output_tokens":1}}'"#,
        );
        assert!(matches!(
            run_fake_cli(&contradiction, "diagnostic-cli 1"),
            Err(FrontierCriticError::Jsonl(
                CodexJsonlError::UnexpectedExecutionMetadata
            ))
        ));
    }

    #[test]
    fn live_challenge_is_bound_into_prompt_schema_and_packet_fingerprint() {
        let mut bytes = [0_u8; 32];
        bytes[0] = 7;
        let challenge = LiveChallenge {
            bytes,
            fingerprint: BlobId::digest(b"challenge"),
        };
        let original = packet();
        let original_fingerprint = original.packet_fingerprint();
        let challenged = original.bind_live_challenge(&challenge).expect("challenge");
        assert_ne!(original_fingerprint, challenged.packet_fingerprint());
        let prompt = std::str::from_utf8(challenged.prompt_utf8()).expect("UTF-8");
        assert!(prompt.contains(&challenge.hex()));
        let schema: Value =
            serde_json::from_slice(challenged.output_schema_utf8()).expect("schema");
        assert_eq!(
            schema["properties"][LIVE_CHALLENGE_FIELD]["const"],
            challenge.hex()
        );
        assert!(
            schema["required"]
                .as_array()
                .expect("required")
                .iter()
                .any(|value| value == LIVE_CHALLENGE_FIELD)
        );
    }

    #[test]
    fn bounded_reader_stops_at_first_overflow_instead_of_draining() {
        let overflow = AtomicBool::new(false);
        let bytes = read_bounded(&b"0123456789"[..], 4, &overflow).expect("read");
        assert_eq!(bytes, b"0123");
        assert!(overflow.load(Ordering::Acquire));
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_pipe_capture_does_not_deadlock_when_child_writes_before_reading() {
        let payload = vec![b'x'; 256 * 1024];
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "/bin/dd if=/dev/zero bs=262144 count=1 2>/dev/null; /bin/cat >/dev/null",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut command);
        let started = Instant::now();
        let child = command.spawn().expect("spawn");
        let output = capture_child(
            child,
            Some(&payload),
            300 * 1024,
            1024,
            Duration::from_secs(5),
            started,
        )
        .expect("concurrent capture");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 256 * 1024);
        assert!(output.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_complete_process_group() {
        use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

        let directory = TempDir::new().expect("tempdir");
        let pid_path = directory.path().join("grandchild.pid");
        let script = format!("sleep 30 & echo $! > {}; wait", pid_path.display());
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut command);
        let started = Instant::now();
        let child = command.spawn().expect("spawn");
        assert!(matches!(
            capture_child(child, None, 1024, 1024, Duration::from_millis(150), started,),
            Err(FrontierCriticError::TimedOut)
        ));
        let grandchild = fs::read_to_string(&pid_path)
            .expect("grandchild PID")
            .trim()
            .parse::<i32>()
            .expect("numeric PID");
        let pid = Pid::from_raw(grandchild);
        let mut gone = false;
        for _ in 0..100 {
            if matches!(kill(pid, None), Err(Errno::ESRCH)) {
                gone = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(gone, "grandchild survived process-group termination");
    }

    #[test]
    fn cleared_environment_exposes_only_reviewed_hashed_values() {
        let environment = ReviewedEnvironment::capture().expect("reviewed environment");
        let mut command = Command::new("/usr/bin/env");
        command
            .env("UNREVIEWED_SECRET", "must-not-survive")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        environment.apply(&mut command);
        configure_process_group(&mut command);
        let started = Instant::now();
        let child = command.spawn().expect("spawn");
        let output = capture_child(
            child,
            None,
            MAX_PROBE_BYTES,
            1024,
            Duration::from_secs(5),
            started,
        )
        .expect("environment output");
        let text = std::str::from_utf8(&output.stdout).expect("UTF-8 environment");
        assert!(!text.contains("UNREVIEWED_SECRET="));
        assert!(!text.contains("OPENAI_API_KEY="));
        assert!(text.contains("RUST_LOG=error"));
        assert!(
            environment
                .evidence
                .iter()
                .all(|item| item.value_fingerprint != BlobId::digest(b""))
        );
    }

    #[test]
    fn reviewed_environment_values_are_bounded_before_process_spawn() {
        let oversized = vec![(
            OsString::from("HTTPS_PROXY"),
            OsString::from("x".repeat(MAX_ENVIRONMENT_VALUE_BYTES + 1)),
        )];
        assert!(matches!(
            validate_effective_environment(&oversized),
            Err(FrontierCriticError::InvalidReviewedEnvironment)
        ));
    }

    #[test]
    fn private_cli_copy_is_not_affected_by_later_source_mutation() {
        let directory = TempDir::new().expect("tempdir");
        let source = directory.path().join("source-cli");
        fs::write(&source, b"reviewed executable bytes").expect("source");
        let expected = BlobId::digest(b"reviewed executable bytes");
        let owned = OwnedCli::copy_exact(&source, expected).expect("private copy");
        fs::write(&source, b"changed after approval").expect("mutate source");
        owned.revalidate().expect("owned bytes remain exact");
        assert_eq!(
            fs::read(owned.path()).expect("owned bytes"),
            b"reviewed executable bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_output_read_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("tempdir");
        let target = directory.path().join("target");
        let link = directory.path().join("output");
        fs::write(&target, b"{}").expect("target");
        symlink(&target, &link).expect("symlink");
        assert!(matches!(
            read_bounded_regular_file(&link, 1024, FrontierCriticError::InvalidFinalOutput),
            Err(FrontierCriticError::InvalidFinalOutput(_))
        ));
    }

    #[test]
    fn workspace_enumeration_rejects_a_third_entry_without_collecting_it() {
        let workspace = TempDir::new().expect("workspace");
        let schema = workspace.path().join("output-schema.json");
        let output = workspace.path().join("final-output.json");
        fs::write(&schema, b"{}").expect("schema");
        fs::write(&output, b"{}").expect("output");
        fs::write(workspace.path().join("mutation"), b"x").expect("mutation");
        assert!(matches!(
            verify_private_workspace(workspace.path(), &schema, &output),
            Err(FrontierCriticError::WorkspaceMutation)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the exact reviewed ChatGPT bundle and current ChatGPT authentication"]
    fn exact_bundled_cli_identity_checks_copy_signature_version_and_authentication() {
        let environment = ReviewedEnvironment::capture().expect("reviewed environment");
        let owned = OwnedCli::copy_exact(Path::new(PINNED_CHATGPT_CODEX_PATH), pinned_cli_sha256())
            .expect("pinned private copy");
        let signature = verify_pinned_code_signature(owned.path()).expect("pinned signature");
        let authentication = verify_cli_version_and_authentication(
            owned.path(),
            PINNED_CHATGPT_CODEX_VERSION,
            &environment,
        )
        .expect("pinned version and ChatGPT authentication");
        assert_eq!(
            trim_single_line(&authentication).expect("auth line"),
            "Logged in using ChatGPT"
        );
        assert_eq!(owned.sha256, pinned_cli_sha256());
        assert_ne!(signature.fingerprint, BlobId::digest(b"unsigned"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the exact reviewed ChatGPT bundle, ChatGPT authentication, and a live frontier-model call"]
    fn real_pinned_chatgpt_cli_runs_tool_free_structured_diagnostic() {
        let receipt = run_chatgpt_bundled_frontier_critic_diagnostic(ok_packet())
            .expect("real tool-free diagnostic receipt");
        let value: Value =
            serde_json::from_slice(receipt.final_output_utf8()).expect("structured output");
        assert_eq!(value["ok"], true);
        assert_eq!(
            value[LIVE_CHALLENGE_FIELD],
            serde_json::from_slice::<Value>(receipt.output_schema_utf8()).expect("executed schema")
                ["properties"][LIVE_CHALLENGE_FIELD]["const"]
        );
        assert_eq!(
            receipt.execution_class(),
            FrontierExecutionClass::ChatGptBundledDiagnostic
        );
        assert_eq!(
            receipt.observed_model(),
            FrontierModelObservation::Unavailable
        );
        assert_eq!(receipt.cli_sha256(), pinned_cli_sha256());
        assert_eq!(receipt.cli_version(), PINNED_CHATGPT_CODEX_VERSION);
        assert!(receipt.code_signature().is_some());
        assert!(receipt.live_challenge_fingerprint().is_some());
        assert_ne!(
            receipt.prepared_packet_fingerprint(),
            receipt.executed_packet_fingerprint()
        );
    }
}
