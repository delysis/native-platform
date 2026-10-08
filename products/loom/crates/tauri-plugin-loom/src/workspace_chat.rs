//! Native chat execution supplied by the application composition root.
//! The workspace owns document projection and worker lifetime; the executor
//! owns canonical chat history, inference and its committed native receipts.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

/// Presentation may inspect this capability, but cannot select or change it.
/// Only installing the native executor opts into the unfinished Mom route.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceChatRoute {
    #[default]
    Loom,
    MomExperimental,
}

/// Use exactly the executor-presence test used by `terminal_run`. A chat pane
/// alone never grants a writer exemption. There is no renderer-side setter.
#[tauri::command]
pub(crate) fn workspace_chat_route<R: Runtime>(app: AppHandle<R>) -> WorkspaceChatRoute {
    if app.try_state::<WorkspaceChatService<R>>().is_some() {
        WorkspaceChatRoute::MomExperimental
    } else {
        WorkspaceChatRoute::Loom
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceChatRequest {
    pub project_id: String,
    pub pane_id: String,
    pub message: String,
    /// Admitted text snapshots, canonicalized as untrusted Mom attachments.
    pub context: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceChatOutput {
    pub conversation_id: String,
    pub text: String,
    pub receipt: serde_json::Value,
}

pub type WorkspaceChatFuture =
    Pin<Box<dyn Future<Output = Result<WorkspaceChatOutput, String>> + Send>>;

pub trait WorkspaceChatExecutor<R: Runtime>: Send + Sync {
    fn dispatch(
        &self,
        app: AppHandle<R>,
        request: WorkspaceChatRequest,
        cancelled: Arc<AtomicBool>,
    ) -> WorkspaceChatFuture;
}

pub struct WorkspaceChatService<R: Runtime>(Arc<dyn WorkspaceChatExecutor<R>>);

impl<R: Runtime> std::fmt::Debug for WorkspaceChatService<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceChatService")
            .finish_non_exhaustive()
    }
}

impl<R: Runtime> Clone for WorkspaceChatService<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: Runtime> WorkspaceChatService<R> {
    pub fn new(executor: Arc<dyn WorkspaceChatExecutor<R>>) -> Self {
        Self(executor)
    }

    pub(crate) fn dispatch(
        &self,
        app: AppHandle<R>,
        request: WorkspaceChatRequest,
        cancelled: Arc<AtomicBool>,
    ) -> WorkspaceChatFuture {
        self.0.dispatch(app, request, cancelled)
    }
}
