use std::sync::Arc;

use llama_native_host::{
    ApplicationNativeFinalizer, ApplicationNativeOwner, JoinedApplicationNative,
    ProcessExitJoinedApplicationNative,
};
use llama_native_types::{NativeError, NativeErrorCode};
use loom_backend_llama::NativeHostRuntime;
use mom_llama_app::EmbeddedMom;

/// Native construction and finalization belong to the normal Loom root.
/// Document workers drain through the Loom plugin before this coordinator
/// drains Mom's product services and joins the one shared host.
pub(crate) struct Application {
    owner: ApplicationNativeOwner,
    mom: EmbeddedMom,
}

impl std::fmt::Debug for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Application")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl Application {
    pub(crate) fn new() -> Result<(Arc<Self>, Arc<NativeHostRuntime>), NativeError> {
        let owner =
            ApplicationNativeOwner::new(mom_llama_runtime::native_runtime::application_host);
        let document = owner.client()?;
        let mom = EmbeddedMom::new(owner.client()?);
        let application = Arc::new(Self { owner, mom });
        let finalizer: Arc<dyn ApplicationNativeFinalizer> = application.clone();
        let document_runtime = Arc::new(NativeHostRuntime::from_application(document, finalizer));
        Ok((application, document_runtime))
    }

    pub(crate) fn configure(
        &self,
        builder: tauri::Builder<tauri::Wry>,
    ) -> tauri::Builder<tauri::Wry> {
        let builder = builder.manage(tauri_plugin_loom::WorkspaceChatService::new(Arc::new(
            MomWorkspaceExecutor {
                mom: self.mom.clone(),
            },
        )));
        self.mom.configure(builder)
    }

    fn drain_mom(&self) -> Result<(), NativeError> {
        tauri::async_runtime::block_on(self.mom.drain()).map_err(|error| {
            NativeError::new(
                NativeErrorCode::Internal,
                format!("Mom service drain failed: {error}"),
            )
        })
    }
}

impl ApplicationNativeFinalizer for Application {
    fn shutdown_joined(&self) -> Result<JoinedApplicationNative, NativeError> {
        self.drain_mom()?;
        self.owner.shutdown_joined()
    }

    fn shutdown_for_process_exit(&self) -> ProcessExitJoinedApplicationNative {
        let drained = self.drain_mom();
        let joined = self.owner.shutdown_for_process_exit();
        if let Err(error) = drained {
            eprintln!(
                "Loom drained its native host but cannot prove Mom service shutdown: {}",
                error.message
            );
            std::process::abort();
        }
        joined
    }
}

struct MomWorkspaceExecutor {
    mom: EmbeddedMom,
}

impl tauri_plugin_loom::WorkspaceChatExecutor<tauri::Wry> for MomWorkspaceExecutor {
    fn dispatch(
        &self,
        app: tauri::AppHandle,
        request: tauri_plugin_loom::WorkspaceChatRequest,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> tauri_plugin_loom::WorkspaceChatFuture {
        let mom = self.mom.clone();
        Box::pin(async move {
            // The existing pane owns a project-wide chat history. A session
            // reopening retains this identity; switching source documents does
            // not manufacture another conversation or import a flat transcript.
            let conversation_id = workspace_conversation_id(&request.project_id, &request.pane_id);
            let result = mom
                .dispatch_cancellable(
                    app,
                    mom_llama_runtime::MentionDispatchInput {
                        conversation_id: conversation_id.clone(),
                        message: request.message,
                    },
                    request.context,
                    cancelled,
                    None::<fn(mom_llama_runtime::ChatDispatchStreamEvent) -> anyhow::Result<()>>,
                )
                .await?;
            if let Some(blocker) = &result.blocker {
                return Err(format!("{}: {}", blocker.code, blocker.message));
            }
            if !result.receipt.real_engine_invoked || result.receipt.fake_fixture {
                return Err("Mom dispatch did not commit a real native result".into());
            }
            let text = match result.result.as_ref() {
                Some(mom_llama_runtime::ChatDispatchOutput::Direct { output, .. }) => {
                    match output
                        .reasoning_content
                        .as_deref()
                        .filter(|text| !text.is_empty())
                    {
                        Some(reasoning) => {
                            format!("<think>{reasoning}</think>{}", output.assistant_text)
                        }
                        None => output.assistant_text.clone(),
                    }
                }
                Some(mom_llama_runtime::ChatDispatchOutput::Mention { invocation, .. }) => {
                    let committed = invocation
                        .results
                        .iter()
                        .filter(|target| {
                            target.message_id.is_some()
                                && target.real_engine_invoked
                                && !target.fake_fixture
                        })
                        .map(|target| format!("@{}\n\n{}", target.handle, target.text))
                        .collect::<Vec<_>>();
                    if committed.is_empty() {
                        return Err(format!(
                            "Mom consultation produced no committed native reply ({:?})",
                            invocation.state
                        ));
                    }
                    committed.join("\n\n")
                }
                None => return Err("Mom dispatch returned no committed result".into()),
            };
            let conversation_id = match result.result.as_ref().expect("checked dispatch result") {
                mom_llama_runtime::ChatDispatchOutput::Direct {
                    conversation_id, ..
                }
                | mom_llama_runtime::ChatDispatchOutput::Mention {
                    conversation_id, ..
                } => conversation_id.clone(),
            };
            // Retain native identities and execution evidence, not a second
            // plaintext copy of Mom's encrypted source conversations.
            let binding = match result.result.as_ref().expect("checked dispatch result") {
                mom_llama_runtime::ChatDispatchOutput::Direct { output, .. } => serde_json::json!({
                    "kind": "direct", "request_id": output.request_id,
                    "user_message_id": output.user_message_id,
                    "assistant_message_id": output.assistant_message_id,
                    "cache_id": output.cache_id, "cache_reused": output.cache_reused,
                    "reasoning_incomplete": output.reasoning_incomplete,
                }),
                mom_llama_runtime::ChatDispatchOutput::Mention { invocation, .. } => {
                    serde_json::json!({
                        "kind": "mention", "invocation_id": invocation.id,
                        "user_message_id": invocation.user_message_id, "state": invocation.state,
                        "targets": invocation.results.iter().map(|target| serde_json::json!({
                            "target_id": target.target_id, "message_id": target.message_id,
                            "model_id": target.model_id, "cache_id": target.cache_id,
                            "cache_reused": target.cache_reused, "state": target.state,
                            "real_engine_invoked": target.real_engine_invoked,
                            "fake_fixture": target.fake_fixture,
                        })).collect::<Vec<_>>(),
                    })
                }
            };
            Ok(tauri_plugin_loom::WorkspaceChatOutput {
                conversation_id,
                text,
                receipt: serde_json::json!({"command_receipt": result.receipt, "binding": binding}),
            })
        })
    }
}

fn workspace_conversation_id(project: &str, pane: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"delysis-loom-workspace-chat-v1\0");
    digest.update(project.as_bytes());
    digest.update([0]);
    digest.update(pane.as_bytes());
    let mut identity: [u8; 16] = digest.finalize()[..16].try_into().expect("SHA-256 prefix");
    identity[6] = (identity[6] & 0x0f) | 0x80;
    identity[8] = (identity[8] & 0x3f) | 0x80;
    let value = u128::from_be_bytes(identity);
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
