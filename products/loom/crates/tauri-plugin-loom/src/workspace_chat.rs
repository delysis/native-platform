//! Native chat execution supplied by the application composition root.
//! The workspace owns document projection and worker lifetime; the executor
//! owns canonical chat history, inference and its committed native receipts.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};

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
