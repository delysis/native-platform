use llama_native_host::NativeHost;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};

const MENTION_RUNNING: u8 = 0;
const MENTION_CANCELLED: u8 = 1;
const MENTION_TERMINAL: u8 = 2;
const CHAT_RUNNING: u8 = 0;
const CHAT_CANCELLED: u8 = 1;
const CHAT_TERMINAL: u8 = 2;

pub(crate) type MentionCancelKey = (String, String);
pub(crate) type MentionCancelRegistry = BTreeMap<MentionCancelKey, Arc<MentionCancelControl>>;

pub(crate) struct MentionCancelControl(AtomicU8);

impl MentionCancelControl {
    pub(crate) fn running(cancelled: bool) -> Self {
        Self(AtomicU8::new(if cancelled {
            MENTION_CANCELLED
        } else {
            MENTION_RUNNING
        }))
    }

    pub(crate) fn request_cancel(&self) -> bool {
        self.0
            .compare_exchange(
                MENTION_RUNNING,
                MENTION_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        self.0.load(Ordering::Acquire) == MENTION_CANCELLED
    }

    pub(crate) fn arbitrate_terminal(&self) -> bool {
        match self.0.compare_exchange(
            MENTION_RUNNING,
            MENTION_TERMINAL,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => false,
            Err(MENTION_CANCELLED) => true,
            Err(_) => false,
        }
    }
}

struct ChatCancelControl(AtomicU8);

impl ChatCancelControl {
    fn new(cancelled: bool) -> Self {
        Self(AtomicU8::new(if cancelled {
            CHAT_CANCELLED
        } else {
            CHAT_RUNNING
        }))
    }

    fn request_cancel(&self) -> bool {
        self.0
            .compare_exchange(
                CHAT_RUNNING,
                CHAT_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn cancellation_requested(&self) -> bool {
        self.0.load(Ordering::Acquire) == CHAT_CANCELLED
    }

    fn arbitrate_terminal(&self) -> bool {
        loop {
            let state = self.0.load(Ordering::Acquire);
            match state {
                CHAT_RUNNING | CHAT_CANCELLED => {
                    if self
                        .0
                        .compare_exchange(state, CHAT_TERMINAL, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return state == CHAT_CANCELLED;
                    }
                }
                CHAT_TERMINAL => return false,
                _ => unreachable!("chat cancellation control entered an invalid state"),
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ToolLoopControl {
    cancel_requested: Arc<AtomicBool>,
    current_model_request_id: Arc<Mutex<Option<String>>>,
}

impl ToolLoopControl {
    pub(crate) fn new(cancelled: bool) -> Self {
        Self {
            cancel_requested: Arc::new(AtomicBool::new(cancelled)),
            current_model_request_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn request_cancel(&self) -> bool {
        !self.cancel_requested.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::Acquire)
    }

    pub(crate) fn set_current_model_request_id(&self, request_id: Option<String>) {
        if let Ok(mut current) = self.current_model_request_id.lock() {
            *current = request_id;
        }
    }

    pub(crate) fn current_model_request_id(&self) -> Option<String> {
        self.current_model_request_id
            .lock()
            .ok()
            .and_then(|request_id| request_id.clone())
    }
}

struct ChatOperation {
    conversation_id: String,
    cancellation: Arc<ChatCancelControl>,
    identity: Arc<()>,
}

struct McpOperation {
    cancellation: Arc<AtomicBool>,
    identity: Arc<()>,
}

struct ToolLoopOperation {
    control: ToolLoopControl,
    identity: Arc<()>,
}

struct OperationRegistries {
    next_mcp_id: u64,
    chats: BTreeMap<String, ChatOperation>,
    mentions: MentionCancelRegistry,
    mcp: BTreeMap<u64, McpOperation>,
    tool_loops: HashMap<String, ToolLoopOperation>,
}

struct OperationScopeInner {
    native_host: Weak<NativeHost>,
    quiescing: AtomicBool,
    native_key: Option<crate::native_runtime::ProductHostKey>,
    registries: Mutex<OperationRegistries>,
}

/// Runtime-local authority for every live Mom product operation.
///
/// Durable request documents remain recovery and user-visible state. This
/// scope is the sole in-process cancellation/control owner, so two runtime
/// instances cannot discover or cancel each other's live work.
#[derive(Clone)]
pub struct OperationScope(Arc<OperationScopeInner>);

impl OperationScope {
    #[must_use]
    pub fn for_native_host(host: &Arc<NativeHost>) -> Self {
        Self::new(Arc::downgrade(host), None)
    }

    #[must_use]
    pub fn detached() -> Self {
        Self::new(Weak::new(), None)
    }

    pub(crate) fn for_product_host(
        host: &Arc<NativeHost>,
        key: crate::native_runtime::ProductHostKey,
    ) -> Self {
        Self::new(Arc::downgrade(host), Some(key))
    }

    pub(crate) fn matches_native_key(&self, key: &crate::native_runtime::ProductHostKey) -> bool {
        self.0.native_key.as_ref().is_none_or(|bound| bound == key)
    }

    pub(crate) fn native_host(&self) -> Option<Arc<NativeHost>> {
        if self.0.quiescing.load(Ordering::Acquire) || self.0.registries.is_poisoned() {
            return None;
        }
        self.0.native_host.upgrade()
    }

    fn new(
        native_host: Weak<NativeHost>,
        native_key: Option<crate::native_runtime::ProductHostKey>,
    ) -> Self {
        Self(Arc::new(OperationScopeInner {
            native_host,
            quiescing: AtomicBool::new(false),
            native_key,
            registries: Mutex::new(OperationRegistries {
                next_mcp_id: 0,
                chats: BTreeMap::new(),
                mentions: BTreeMap::new(),
                mcp: BTreeMap::new(),
                tool_loops: HashMap::new(),
            }),
        }))
    }

    pub(crate) fn register_chat(
        &self,
        request_id: &str,
        conversation_id: &str,
    ) -> anyhow::Result<ChatOperationLease> {
        let identity = Arc::new(());
        let mut registries = self
            .0
            .registries
            .lock()
            .map_err(|_| anyhow::anyhow!("chat operation registry is unavailable"))?;
        if registries.chats.contains_key(request_id) {
            anyhow::bail!("chat request identity is already active in this runtime");
        }
        let cancellation = Arc::new(ChatCancelControl::new(
            self.0.quiescing.load(Ordering::Acquire),
        ));
        registries.chats.insert(
            request_id.to_owned(),
            ChatOperation {
                conversation_id: conversation_id.to_owned(),
                cancellation: Arc::clone(&cancellation),
                identity: Arc::clone(&identity),
            },
        );
        Ok(ChatOperationLease {
            scope: self.clone(),
            request_id: request_id.to_owned(),
            cancellation,
            identity,
        })
    }

    pub(crate) fn chat_request_is_active(&self, request_id: &str, conversation_id: &str) -> bool {
        self.0
            .registries
            .lock()
            .ok()
            .and_then(|registries| {
                registries
                    .chats
                    .get(request_id)
                    .map(|operation| operation.conversation_id == conversation_id)
            })
            .unwrap_or(false)
    }

    pub(crate) fn request_chat_cancellation(&self, request_id: &str) -> bool {
        self.0
            .registries
            .lock()
            .ok()
            .and_then(|registries| {
                registries
                    .chats
                    .get(request_id)
                    .map(|operation| operation.cancellation.request_cancel())
            })
            .unwrap_or(false)
    }

    pub(crate) fn with_mention_registry<R>(
        &self,
        operation: impl FnOnce(&mut MentionCancelRegistry, bool) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        let mut registries = self
            .0
            .registries
            .lock()
            .map_err(|_| anyhow::anyhow!("Persona cancellation registry is unavailable"))?;
        let quiescing = self.0.quiescing.load(Ordering::Acquire);
        operation(&mut registries.mentions, quiescing)
    }

    pub(crate) fn register_mcp(&self) -> anyhow::Result<McpOperationLease> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let identity = Arc::new(());
        let mut registries = self
            .0
            .registries
            .lock()
            .map_err(|_| anyhow::anyhow!("MCP operation registry is unavailable"))?;
        let id = registries
            .next_mcp_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("MCP operation identity space is exhausted"))?;
        registries.next_mcp_id = id;
        if self.0.quiescing.load(Ordering::Acquire) {
            cancellation.store(true, Ordering::Release);
        }
        registries.mcp.insert(
            id,
            McpOperation {
                cancellation: Arc::clone(&cancellation),
                identity: Arc::clone(&identity),
            },
        );
        Ok(McpOperationLease {
            scope: self.clone(),
            id,
            cancellation,
            identity,
        })
    }

    pub(crate) fn register_tool_loop(
        &self,
        request_id: &str,
    ) -> anyhow::Result<(ToolLoopControl, ToolLoopOperationLease)> {
        let identity = Arc::new(());
        let mut registries = self
            .0
            .registries
            .lock()
            .map_err(|_| anyhow::anyhow!("tool-loop control registry is unavailable"))?;
        if registries.tool_loops.contains_key(request_id) {
            anyhow::bail!("tool-loop request identity is already active in this runtime");
        }
        let control = ToolLoopControl::new(self.0.quiescing.load(Ordering::Acquire));
        registries.tool_loops.insert(
            request_id.to_owned(),
            ToolLoopOperation {
                control: control.clone(),
                identity: Arc::clone(&identity),
            },
        );
        Ok((
            control,
            ToolLoopOperationLease {
                scope: self.clone(),
                request_id: request_id.to_owned(),
                identity,
            },
        ))
    }

    pub(crate) fn tool_loop_control(&self, request_id: &str) -> Option<ToolLoopControl> {
        self.0.registries.lock().ok().and_then(|registries| {
            registries
                .tool_loops
                .get(request_id)
                .map(|operation| operation.control.clone())
        })
    }

    pub(crate) fn cancel_native(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        self.0
            .native_host
            .upgrade()
            .map_or(0, |host| host.cancel(request_id, branch_id))
    }

    pub(crate) fn skip_native_reasoning(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        self.0
            .native_host
            .upgrade()
            .map_or(0, |host| host.skip_reasoning(request_id, branch_id))
    }

    /// Closes this scope to new uncancelled operations and requests
    /// cancellation from every operation currently owned by this runtime.
    pub fn request_cancellation(&self) -> usize {
        let (chat_requests, mention_requests, mcp_cancellations, tool_controls) = {
            let Ok(registries) = self.0.registries.lock() else {
                return 0;
            };
            self.0.quiescing.store(true, Ordering::Release);
            let chats = registries
                .chats
                .iter()
                .filter(|(_, operation)| operation.cancellation.request_cancel())
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            let mentions = registries
                .mentions
                .iter()
                .filter(|(_, control)| control.request_cancel())
                .map(|((invocation_id, target_id), _)| (invocation_id.clone(), target_id.clone()))
                .collect::<Vec<_>>();
            let mcp = registries
                .mcp
                .values()
                .filter(|operation| !operation.cancellation.swap(true, Ordering::AcqRel))
                .count();
            let tools = registries
                .tool_loops
                .values()
                .map(|operation| operation.control.clone())
                .collect::<Vec<_>>();
            (chats, mentions, mcp, tools)
        };

        let mut cancelled = mcp_cancellations;
        for request_id in chat_requests {
            cancelled = cancelled.saturating_add(1.max(self.cancel_native(&request_id, None)));
        }
        for (invocation_id, target_id) in mention_requests {
            cancelled = cancelled.saturating_add(
                1.max(self.cancel_native(&invocation_id, Some(target_id.as_str()))),
            );
        }
        for control in tool_controls {
            let newly_cancelled = control.request_cancel();
            if let Some(request_id) = control.current_model_request_id() {
                cancelled = cancelled.saturating_add(self.cancel_native(&request_id, None));
            }
            if newly_cancelled {
                cancelled = cancelled.saturating_add(1);
            }
        }
        cancelled
    }

    #[must_use]
    pub fn active_operation_count(&self) -> usize {
        self.0.registries.lock().map_or(0, |registries| {
            registries
                .chats
                .len()
                .saturating_add(registries.mentions.len())
                .saturating_add(registries.mcp.len())
                .saturating_add(registries.tool_loops.len())
        })
    }
}

pub(crate) struct ChatOperationLease {
    scope: OperationScope,
    request_id: String,
    cancellation: Arc<ChatCancelControl>,
    identity: Arc<()>,
}

impl ChatOperationLease {
    pub(crate) fn cancellation_requested(&self) -> bool {
        self.cancellation.cancellation_requested()
    }

    pub(crate) fn request_cancel(&self) -> bool {
        self.cancellation.request_cancel()
    }

    pub(crate) fn arbitrate_terminal(&self) -> bool {
        self.cancellation.arbitrate_terminal()
    }

    pub(crate) fn with_native_admission<R>(
        &self,
        admit: impl FnOnce() -> R,
    ) -> anyhow::Result<Option<R>> {
        let registries = self
            .scope
            .0
            .registries
            .lock()
            .map_err(|_| anyhow::anyhow!("chat operation registry is unavailable"))?;
        let Some(operation) = registries.chats.get(&self.request_id) else {
            return Ok(None);
        };
        if !Arc::ptr_eq(&operation.identity, &self.identity)
            || operation.cancellation.cancellation_requested()
        {
            return Ok(None);
        }
        Ok(Some(admit()))
    }
}

impl Drop for ChatOperationLease {
    fn drop(&mut self) {
        let Ok(mut registries) = self.scope.0.registries.lock() else {
            return;
        };
        if registries
            .chats
            .get(&self.request_id)
            .is_some_and(|operation| Arc::ptr_eq(&operation.identity, &self.identity))
        {
            registries.chats.remove(&self.request_id);
        }
    }
}

pub(crate) struct McpOperationLease {
    scope: OperationScope,
    id: u64,
    cancellation: Arc<AtomicBool>,
    identity: Arc<()>,
}

impl McpOperationLease {
    pub(crate) fn cancellation_requested(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
}

impl Drop for McpOperationLease {
    fn drop(&mut self) {
        let Ok(mut registries) = self.scope.0.registries.lock() else {
            return;
        };
        if registries
            .mcp
            .get(&self.id)
            .is_some_and(|operation| Arc::ptr_eq(&operation.identity, &self.identity))
        {
            registries.mcp.remove(&self.id);
        }
    }
}

pub(crate) struct ToolLoopOperationLease {
    scope: OperationScope,
    request_id: String,
    identity: Arc<()>,
}

impl Drop for ToolLoopOperationLease {
    fn drop(&mut self) {
        let Ok(mut registries) = self.scope.0.registries.lock() else {
            return;
        };
        if registries
            .tool_loops
            .get(&self.request_id)
            .is_some_and(|operation| Arc::ptr_eq(&operation.identity, &self.identity))
        {
            registries.tool_loops.remove(&self.request_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MentionCancelControl, OperationScope};
    use std::sync::{Arc, Barrier};

    #[test]
    fn barrier_driven_two_runtime_cancellation_is_exactly_isolated() {
        let left = OperationScope::detached();
        let right = OperationScope::detached();
        let left_chat = left
            .register_chat("same-chat-request", "same-conversation")
            .expect("left chat operation");
        let right_chat = right
            .register_chat("same-chat-request", "same-conversation")
            .expect("right chat operation");
        let register_mention = |scope: &OperationScope| {
            scope
                .with_mention_registry(|registry, quiescing| {
                    let control = Arc::new(MentionCancelControl::running(quiescing));
                    registry.insert(
                        ("same-invocation".to_owned(), "same-target".to_owned()),
                        Arc::clone(&control),
                    );
                    Ok(control)
                })
                .expect("mention operation")
        };
        let left_mention = register_mention(&left);
        let right_mention = register_mention(&right);
        let left_mcp = left.register_mcp().expect("left MCP operation");
        let right_mcp = right.register_mcp().expect("right MCP operation");
        let (left_tool, left_tool_lease) = left
            .register_tool_loop("same-tool-request")
            .expect("left tool loop");
        let (right_tool, right_tool_lease) = right
            .register_tool_loop("same-tool-request")
            .expect("right tool loop");
        let barrier = Arc::new(Barrier::new(2));
        let cancel_left = {
            let barrier = Arc::clone(&barrier);
            let left = left.clone();
            std::thread::spawn(move || {
                barrier.wait();
                left.request_cancellation()
            })
        };

        barrier.wait();
        assert!(cancel_left.join().expect("left cancellation") >= 4);
        assert!(left_chat.cancellation_requested());
        assert!(left_mention.cancellation_requested());
        assert!(left_mcp.cancellation_requested());
        assert!(left_tool.cancellation_requested());
        assert!(!right_chat.cancellation_requested());
        assert!(!right_mention.cancellation_requested());
        assert!(!right_mcp.cancellation_requested());
        assert!(!right_tool.cancellation_requested());
        assert!(right.chat_request_is_active("same-chat-request", "same-conversation"));
        assert_eq!(left.active_operation_count(), 4);
        assert_eq!(right.active_operation_count(), 4);

        drop((left_chat, left_mcp, left_tool_lease));
        drop((right_chat, right_mcp, right_tool_lease));
        for scope in [&left, &right] {
            scope
                .with_mention_registry(|registry, _| {
                    registry.clear();
                    Ok(())
                })
                .expect("clear mention operation");
        }
        assert_eq!(left.active_operation_count(), 0);
        assert_eq!(right.active_operation_count(), 0);
    }

    #[test]
    fn quiescing_scope_pre_cancels_late_registration_and_drains_to_zero() {
        let scope = OperationScope::detached();
        scope.request_cancellation();
        let chat = scope
            .register_chat("late-chat", "late-conversation")
            .expect("late chat registration");
        let mcp = scope.register_mcp().expect("late MCP registration");
        let (tool, tool_lease) = scope
            .register_tool_loop("late-tool")
            .expect("late tool registration");
        let mention = scope
            .with_mention_registry(|registry, quiescing| {
                let mention = Arc::new(MentionCancelControl::running(quiescing));
                registry.insert(
                    ("late-invocation".to_owned(), "late-target".to_owned()),
                    Arc::clone(&mention),
                );
                Ok(mention)
            })
            .expect("mention registration");

        assert!(chat.cancellation_requested());
        assert!(mcp.cancellation_requested());
        assert!(tool.cancellation_requested());
        assert!(mention.cancellation_requested());
        assert_eq!(scope.active_operation_count(), 4);

        drop((chat, mcp, tool_lease));
        scope
            .with_mention_registry(|registry, _| {
                registry.clear();
                Ok(())
            })
            .expect("mention registry");
        assert_eq!(scope.active_operation_count(), 0);
    }

    #[test]
    fn barrier_driven_quiesce_prevents_late_chat_native_admission() {
        let scope = OperationScope::detached();
        let chat = scope
            .register_chat("chat-request", "conversation")
            .expect("chat registration");
        let start = Arc::new(Barrier::new(2));
        let cancelled = Arc::new(Barrier::new(2));
        let cancellation = {
            let scope = scope.clone();
            let start = Arc::clone(&start);
            let cancelled = Arc::clone(&cancelled);
            std::thread::spawn(move || {
                start.wait();
                scope.request_cancellation();
                cancelled.wait();
            })
        };

        start.wait();
        cancelled.wait();
        cancellation.join().expect("scope cancellation");
        assert!(chat.cancellation_requested());
        assert_eq!(
            chat.with_native_admission(|| "native admission ran")
                .expect("native admission arbitration"),
            None
        );
        assert!(chat.arbitrate_terminal());
        drop(chat);
        assert_eq!(scope.active_operation_count(), 0);
    }
}
