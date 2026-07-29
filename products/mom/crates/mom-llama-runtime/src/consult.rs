use crate::config::resolve_settings;
use crate::conversation_store::{MessageRole, get_or_create_conversation};
use crate::native_runtime::{cancel_native_request, resident_model, resident_model_for_slot};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::{Result, anyhow};
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, GenerationEventKind, GenerationMetrics,
    GenerationRequest, GenerationState, SamplingConfig, SharedPrefixBatchRequest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

const PANELS_NAMESPACE: &str = "consult-panels.v1";
const RUNS_NAMESPACE: &str = "consult-runs.v1";
const MAX_SEATS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultPersona {
    pub id: String,
    pub label: String,
    pub description: String,
    pub perspective_prompt: String,
    #[serde(default)]
    pub model_slot: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultPanel {
    pub id: String,
    pub name: String,
    pub personas: Vec<ConsultPersona>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ConsultPanelDb {
    panels: Vec<ConsultPanel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsultSeatResult {
    pub seat_id: String,
    pub label: String,
    pub description: String,
    pub state: GenerationState,
    pub text: String,
    pub model_id: String,
    pub receipt_id: String,
    pub content_sha256: String,
    pub metrics: GenerationMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsultRunState {
    Running,
    Completed,
    PartiallyCancelled,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultSynthesis {
    pub text: String,
    pub receipt_id: String,
    pub content_sha256: String,
    pub source_receipt_ids: Vec<String>,
    pub source_content_sha256: Vec<String>,
    pub derived: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsultRun {
    pub id: String,
    pub conversation_id: String,
    pub panel_id: String,
    pub prompt: String,
    pub model_id: String,
    pub state: ConsultRunState,
    pub seats: Vec<ConsultSeatResult>,
    pub synthesis: Option<ConsultSynthesis>,
    pub created_at: String,
    pub updated_at: String,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
    pub medical_authority: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct ConsultRunDb {
    runs: Vec<ConsultRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultStartInput {
    pub conversation_id: String,
    pub prompt: String,
    pub panel_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsultStartOptions {
    pub timeout_s: f64,
    pub fake_fixture: bool,
}

impl Default for ConsultStartOptions {
    fn default() -> Self {
        Self {
            timeout_s: 180.0,
            fake_fixture: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultStreamEvent {
    pub schema: String,
    pub run_id: String,
    pub seat_id: String,
    pub event: String,
    pub delta: Option<String>,
    pub state: Option<GenerationState>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsultCancelOutput {
    pub run_id: String,
    pub seat_id: Option<String>,
    pub cancelled_sequences: usize,
}

pub fn consult_panel_list() -> Result<CommandResult<Vec<ConsultPanel>>> {
    let panels = load_panels()?.panels;
    Ok(CommandResult::passed(
        "mom_llama.consult_panel_list",
        "contracted",
        panels,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn consult_panel_create(
    name: String,
    personas: Vec<ConsultPersona>,
) -> Result<CommandResult<ConsultPanel>> {
    if personas.is_empty() || personas.len() > MAX_SEATS {
        return Ok(CommandResult::blocked(
            "mom_llama.consult_panel_create",
            "stub_blocked",
            Blocker::new(
                "consult_seat_count_invalid",
                "A consult panel must contain between one and four reasoning perspectives.",
                vec!["Choose one to four perspectives.".to_string()],
            ),
        ));
    }
    let now = now_ms().to_string();
    let panel = ConsultPanel {
        id: Uuid::new_v4().to_string(),
        name: if name.trim().is_empty() {
            "Consult group".to_string()
        } else {
            name.trim().to_string()
        },
        personas,
        created_at: now.clone(),
        updated_at: now,
    };
    let store = RuntimeStore::current()?;
    store.mutate(PANELS_NAMESPACE, ConsultPanelDb::default, |db| {
        db.panels.insert(0, panel.clone());
        Ok(())
    })?;
    Ok(CommandResult::passed(
        "mom_llama.consult_panel_create",
        "contracted",
        panel,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn consult_start(
    input: ConsultStartInput,
    options: ConsultStartOptions,
) -> Result<CommandResult<ConsultRun>> {
    consult_start_stream(input, options, None::<fn(ConsultStreamEvent) -> Result<()>>)
}

pub fn consult_start_stream<F>(
    input: ConsultStartInput,
    options: ConsultStartOptions,
    mut on_event: Option<F>,
) -> Result<CommandResult<ConsultRun>>
where
    F: FnMut(ConsultStreamEvent) -> Result<()>,
{
    if input.prompt.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.consult_start",
            "stub_blocked",
            Blocker::new(
                "consult_prompt_empty",
                "The consult question is empty.",
                vec!["Type a question for the consult group.".to_string()],
            ),
        ));
    }
    let panel = selected_panel(input.panel_id.as_deref())?;
    let settings = resolve_settings()?;
    let (_, conversation) = get_or_create_conversation(&input.conversation_id)?;
    let run_id = Uuid::new_v4().to_string();
    let created_at = now_ms().to_string();
    let mut run = ConsultRun {
        id: run_id.clone(),
        conversation_id: input.conversation_id.clone(),
        panel_id: panel.id.clone(),
        prompt: input.prompt.clone(),
        model_id: settings
            .model_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|value| value.to_str())
            .unwrap_or("fixture-model")
            .to_string(),
        state: ConsultRunState::Running,
        seats: panel
            .personas
            .iter()
            .map(|persona| pending_seat(&run_id, persona, &settings))
            .collect(),
        synthesis: None,
        created_at: created_at.clone(),
        updated_at: created_at,
        real_engine_invoked: false,
        fake_fixture: options.fake_fixture,
        medical_authority: false,
    };
    save_run(&run)?;

    if options.fake_fixture {
        for seat in &mut run.seats {
            seat.state = GenerationState::Completed;
            seat.text = format!("Fixture response from {}.", seat.label);
            seat.content_sha256 = sha256(&seat.text);
            emit(
                &mut on_event,
                consult_event(
                    &run_id,
                    &seat.seat_id,
                    "completed",
                    None,
                    Some(GenerationState::Completed),
                    options,
                ),
            )?;
        }
    } else {
        let outputs = match run_native_consult(
            &settings,
            &panel,
            &run_id,
            common_messages(&conversation.messages, &input.prompt),
            options,
            &mut on_event,
        ) {
            Ok(outputs) => outputs,
            Err(ConsultExecutionError::Blocked(blocked)) => {
                run.state = ConsultRunState::Failed;
                run.updated_at = now_ms().to_string();
                save_run(&run)?;
                return Ok(CommandResult::blocked(
                    "mom_llama.consult_start",
                    &blocked.readiness,
                    blocked.blocker,
                ));
            }
            Err(ConsultExecutionError::Runtime(error)) => return Err(error),
        };
        run.real_engine_invoked = true;
        let model_ids = outputs
            .iter()
            .map(|output| output.model_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        run.model_id = if model_ids.len() == 1 {
            model_ids.iter().next().cloned().unwrap_or_default()
        } else {
            "multiple-native-models".to_string()
        };
        let by_id = outputs
            .into_iter()
            .map(|output| (output.branch_id.clone(), output))
            .collect::<BTreeMap<_, _>>();
        for seat in &mut run.seats {
            let Some(output) = by_id.get(&seat.seat_id) else {
                seat.state = GenerationState::Failed;
                continue;
            };
            seat.state = output.state;
            seat.text = output.text.clone();
            seat.content_sha256 = sha256(&seat.text);
            seat.metrics = output.metrics.clone();
            seat.model_id = output.model_id.clone();
        }
    }
    run.state = terminal_run_state(&run.seats);
    run.updated_at = now_ms().to_string();
    save_run(&run)?;
    Ok(CommandResult::passed(
        "mom_llama.consult_start",
        if options.fake_fixture {
            "fake_fixture_exercised"
        } else {
            "real_prompt_smoke_passed"
        },
        run,
        vec![RuntimeStore::current()?.path().display().to_string()],
        Vec::new(),
        !options.fake_fixture,
        options.fake_fixture,
    ))
}

pub fn consult_status(run_id: &str) -> Result<CommandResult<ConsultRun>> {
    let Some(run) = load_runs()?.runs.into_iter().find(|run| run.id == run_id) else {
        return Ok(run_not_found("mom_llama.consult_status", run_id));
    };
    Ok(CommandResult::passed(
        "mom_llama.consult_status",
        "contracted",
        run,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn consult_cancel(
    run_id: &str,
    seat_id: Option<&str>,
) -> Result<CommandResult<ConsultCancelOutput>> {
    let cancelled = cancel_native_request(run_id, seat_id);
    if cancelled == 0 {
        return Ok(CommandResult::blocked(
            "mom_llama.consult_cancel",
            "stub_blocked",
            Blocker::new(
                "consult_not_active",
                "That consult stream is no longer active.",
                vec!["Refresh the consult group.".to_string()],
            ),
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.consult_cancel",
        "contracted",
        ConsultCancelOutput {
            run_id: run_id.to_string(),
            seat_id: seat_id.map(str::to_string),
            cancelled_sequences: cancelled,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn consult_synthesize(
    run_id: &str,
    selected_seat_ids: Vec<String>,
) -> Result<CommandResult<ConsultSynthesis>> {
    let mut run = match load_runs()?.runs.into_iter().find(|run| run.id == run_id) {
        Some(run) => run,
        None => return Ok(run_not_found("mom_llama.consult_synthesize", run_id)),
    };
    let selected = run
        .seats
        .iter()
        .filter(|seat| {
            seat.state == GenerationState::Completed
                && (selected_seat_ids.is_empty() || selected_seat_ids.contains(&seat.seat_id))
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.consult_synthesize",
            "stub_blocked",
            Blocker::new(
                "consult_sources_not_terminal",
                "No completed consult responses are selected for synthesis.",
                vec!["Wait for at least one response to finish.".to_string()],
            ),
        ));
    }
    let settings = resolve_settings()?;
    let handle = match resident_model(&settings) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.consult_synthesize",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let sources = selected
        .iter()
        .map(|seat| format!("## {}\n{}", seat.label, seat.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let ticket = handle
        .generate(GenerationRequest {
            request_id: format!("{run_id}:synthesis"),
            model_id: handle.status().model_id,
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "Synthesize the supplied reasoning perspectives. Preserve disagreements, distinguish evidence from uncertainty, and do not imply that any perspective is a clinician or medical authority.".to_string(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!("Question:\n{}\n\nPerspectives:\n{sources}", run.prompt),
                },
            ],
            sampling: sampling_config(&settings),
            media: Vec::new(),
            cached_prefix: None,
        })
        .map_err(|error| anyhow!(error))?;
    let output = ticket
        .wait()
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("native synthesis returned no output"))?;
    let synthesis = ConsultSynthesis {
        text: output.text.clone(),
        receipt_id: format!("mom_llama.consult_synthesize:{run_id}"),
        content_sha256: sha256(&output.text),
        source_receipt_ids: selected
            .iter()
            .map(|seat| seat.receipt_id.clone())
            .collect(),
        source_content_sha256: selected
            .iter()
            .map(|seat| seat.content_sha256.clone())
            .collect(),
        derived: true,
        created_at: now_ms().to_string(),
    };
    run.synthesis = Some(synthesis.clone());
    run.updated_at = now_ms().to_string();
    save_run(&run)?;
    Ok(CommandResult::passed(
        "mom_llama.consult_synthesize",
        "real_prompt_smoke_passed",
        synthesis,
        vec![RuntimeStore::current()?.path().display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}

fn load_panels() -> Result<ConsultPanelDb> {
    let store = RuntimeStore::current()?;
    let mut db = store
        .get::<ConsultPanelDb>(PANELS_NAMESPACE)?
        .unwrap_or_default();
    if db.panels.is_empty() {
        db.panels.push(default_panel());
        store.put(PANELS_NAMESPACE, &db)?;
    }
    Ok(db)
}

fn selected_panel(id: Option<&str>) -> Result<ConsultPanel> {
    let db = load_panels()?;
    id.and_then(|id| db.panels.iter().find(|panel| panel.id == id).cloned())
        .or_else(|| db.panels.first().cloned())
        .ok_or_else(|| anyhow!("no consult panel is configured"))
}

fn load_runs() -> Result<ConsultRunDb> {
    Ok(RuntimeStore::current()?
        .get(RUNS_NAMESPACE)?
        .unwrap_or_default())
}

fn save_run(run: &ConsultRun) -> Result<()> {
    RuntimeStore::current()?.mutate(RUNS_NAMESPACE, ConsultRunDb::default, |db| {
        if let Some(current) = db.runs.iter_mut().find(|current| current.id == run.id) {
            *current = run.clone();
        } else {
            db.runs.insert(0, run.clone());
        }
        Ok(())
    })
}

fn pending_seat(
    run_id: &str,
    persona: &ConsultPersona,
    settings: &crate::config::Settings,
) -> ConsultSeatResult {
    ConsultSeatResult {
        seat_id: persona.id.clone(),
        label: persona.label.clone(),
        description: persona.description.clone(),
        state: GenerationState::Queued,
        text: String::new(),
        model_id: settings
            .model_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|value| value.to_str())
            .unwrap_or("fixture-model")
            .to_string(),
        receipt_id: format!("mom_llama.consult.member:{run_id}:{}", persona.id),
        content_sha256: String::new(),
        metrics: GenerationMetrics::default(),
    }
}

fn common_messages(
    messages: &[crate::conversation_store::Message],
    prompt: &str,
) -> Vec<ChatMessage> {
    let mut common = vec![ChatMessage {
        role: ChatRole::System,
        content: "You are one reasoning perspective in a private local consult group. You are not a clinician and must not claim medical authority. Be clear about uncertainty and suggest professional review when appropriate.".to_string(),
    }];
    common.extend(messages.iter().map(|message| ChatMessage {
        role: match message.role {
            MessageRole::System => ChatRole::System,
            MessageRole::User => ChatRole::User,
            MessageRole::Assistant => ChatRole::Assistant,
        },
        content: message.content.clone(),
    }));
    common.push(ChatMessage {
        role: ChatRole::User,
        content: prompt.to_string(),
    });
    common
}

fn sampling_config(settings: &crate::config::Settings) -> SamplingConfig {
    settings.sampling_config()
}

fn terminal_run_state(seats: &[ConsultSeatResult]) -> ConsultRunState {
    let completed = seats
        .iter()
        .filter(|seat| seat.state == GenerationState::Completed)
        .count();
    let cancelled = seats
        .iter()
        .filter(|seat| seat.state == GenerationState::Cancelled)
        .count();
    if completed == seats.len() {
        ConsultRunState::Completed
    } else if cancelled == seats.len() {
        ConsultRunState::Cancelled
    } else if completed > 0 && cancelled > 0 {
        ConsultRunState::PartiallyCancelled
    } else {
        ConsultRunState::Failed
    }
}

fn emit<F>(callback: &mut Option<F>, event: ConsultStreamEvent) -> Result<()>
where
    F: FnMut(ConsultStreamEvent) -> Result<()>,
{
    if let Some(callback) = callback.as_mut() {
        callback(event)?;
    }
    Ok(())
}

enum ConsultExecutionError {
    Blocked(crate::engine::ValidationBlocker),
    Runtime(anyhow::Error),
}

fn run_native_consult<F>(
    settings: &crate::config::Settings,
    panel: &ConsultPanel,
    run_id: &str,
    common_messages: Vec<ChatMessage>,
    options: ConsultStartOptions,
    on_event: &mut Option<F>,
) -> std::result::Result<Vec<llama_native_types::GenerationOutput>, ConsultExecutionError>
where
    F: FnMut(ConsultStreamEvent) -> Result<()>,
{
    let mut groups = BTreeMap::<usize, Vec<&ConsultPersona>>::new();
    for persona in &panel.personas {
        groups
            .entry(persona.model_slot.unwrap_or(0))
            .or_default()
            .push(persona);
    }
    let mut tickets = Vec::with_capacity(groups.len());
    for (slot_id, personas) in groups {
        let handle = if slot_id == 0 {
            resident_model(settings)
        } else {
            resident_model_for_slot(settings, slot_id, None)
        }
        .map_err(ConsultExecutionError::Blocked)?;
        let status = handle.status();
        let request = SharedPrefixBatchRequest {
            request_id: run_id.to_string(),
            model_id: status.model_id,
            common_messages: common_messages.clone(),
            branches: personas
                .into_iter()
                .map(|persona| BranchRequest {
                    branch_id: persona.id.clone(),
                    label: persona.label.clone(),
                    instruction: persona.perspective_prompt.clone(),
                    sampling: sampling_config(settings),
                })
                .collect(),
            cached_prefix: None,
        };
        let ticket = handle
            .generate_shared_prefix(request)
            .map_err(|error| ConsultExecutionError::Runtime(anyhow!(error)))?;
        tickets.push(ticket);
    }

    let started = Instant::now();
    let timeout = Duration::from_secs_f64(options.timeout_s.max(0.001));
    let mut disconnected = vec![false; tickets.len()];
    while disconnected.iter().any(|done| !done) {
        if started.elapsed() >= timeout {
            for ticket in &tickets {
                ticket.cancel_all();
            }
        }
        let mut made_progress = false;
        for (index, ticket) in tickets.iter().enumerate() {
            if disconnected[index] {
                continue;
            }
            loop {
                match ticket.events.try_recv() {
                    Ok(event) => {
                        made_progress = true;
                        let (name, delta, state) = match event.event {
                            GenerationEventKind::Delta { text } => ("delta", Some(text), None),
                            GenerationEventKind::State { state } => ("state", None, Some(state)),
                            GenerationEventKind::Warning { message, .. } => {
                                ("warning", Some(message), None)
                            }
                        };
                        emit(
                            on_event,
                            consult_event(run_id, &event.branch_id, name, delta, state, options),
                        )
                        .map_err(ConsultExecutionError::Runtime)?;
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        disconnected[index] = true;
                        break;
                    }
                }
            }
        }
        if !made_progress {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let mut outputs = Vec::new();
    for ticket in tickets {
        outputs.extend(
            ticket
                .wait()
                .map_err(|error| ConsultExecutionError::Runtime(anyhow!(error)))?,
        );
    }
    Ok(outputs)
}

fn consult_event(
    run_id: &str,
    seat_id: &str,
    event: &str,
    delta: Option<String>,
    state: Option<GenerationState>,
    options: ConsultStartOptions,
) -> ConsultStreamEvent {
    ConsultStreamEvent {
        schema: "mom_llama.consult_stream_event.v1".to_string(),
        run_id: run_id.to_string(),
        seat_id: seat_id.to_string(),
        event: event.to_string(),
        delta,
        state,
        real_engine_invoked: !options.fake_fixture,
        fake_fixture: options.fake_fixture,
    }
}

fn run_not_found<T>(command: &str, run_id: &str) -> CommandResult<T>
where
    T: Serialize,
{
    CommandResult::blocked(
        command,
        "stub_blocked",
        Blocker::new(
            "consult_run_not_found",
            format!("Consult run {run_id} was not found."),
            vec!["Start a new consult group.".to_string()],
        ),
    )
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn default_panel() -> ConsultPanel {
    let now = now_ms().to_string();
    ConsultPanel {
        id: "balanced-four".to_string(),
        name: "Balanced consult group".to_string(),
        personas: vec![
            ConsultPersona {
                id: "evidence".to_string(),
                label: "Evidence lens".to_string(),
                description: "Separates observations, evidence, and uncertainty.".to_string(),
                perspective_prompt: "Answer the preceding question by identifying the strongest evidence, missing facts, uncertainty, and what would change the conclusion.".to_string(),
                model_slot: None,
            },
            ConsultPersona {
                id: "whole-person".to_string(),
                label: "Whole-person lens".to_string(),
                description: "Considers goals, context, preferences, and tradeoffs.".to_string(),
                perspective_prompt: "Answer the preceding question by considering the person's goals, context, preferences, burdens, and tradeoffs. Do not claim clinical authority.".to_string(),
                model_slot: None,
            },
            ConsultPersona {
                id: "practical".to_string(),
                label: "Practical lens".to_string(),
                description: "Turns reasoning into safe, concrete next steps.".to_string(),
                perspective_prompt: "Answer the preceding question with a small sequence of practical, reversible next steps and clear escalation conditions.".to_string(),
                model_slot: None,
            },
            ConsultPersona {
                id: "skeptical".to_string(),
                label: "Skeptical lens".to_string(),
                description: "Challenges assumptions and searches for failure modes.".to_string(),
                perspective_prompt: "Answer the preceding question by challenging assumptions, identifying risks and failure modes, and presenting the strongest reasonable alternative interpretation.".to_string(),
                model_slot: None,
            },
        ],
        created_at: now.clone(),
        updated_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_panel_is_bounded_and_disclaims_authority() {
        let panel = default_panel();
        assert_eq!(panel.personas.len(), MAX_SEATS);
        assert!(
            panel
                .personas
                .iter()
                .all(|persona| !persona.label.contains("Doctor"))
        );
    }

    #[test]
    fn terminal_state_preserves_independent_cancellation() {
        let settings = crate::config::Settings::defaults_for_data_dir(std::env::temp_dir());
        let panel = default_panel();
        let mut seats = panel
            .personas
            .iter()
            .map(|persona| pending_seat("run", persona, &settings))
            .collect::<Vec<_>>();
        for seat in &mut seats {
            seat.state = GenerationState::Completed;
        }
        seats[0].state = GenerationState::Cancelled;
        assert_eq!(
            terminal_run_state(&seats),
            ConsultRunState::PartiallyCancelled
        );
    }
}
