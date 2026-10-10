//! Bounded, cancellable context preparation for native editors. The host owns
//! execution and checks its document/session authority again before use.
use crate::{
    Cancellation, FriendsConfig, FriendsError, PromptPack, compile, continuation_context,
    fingerprint, invalid, mentions, retrieve,
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Condvar, Mutex},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextScope {
    pub project: String,
    pub session: String,
    pub document: String,
}
#[derive(Debug, Clone)]
pub struct PreparedContext {
    pub source_basis: String,
    pub pack: Arc<PromptPack>,
    pub preamble: String,
    pub config_sha256: String,
    pub reserved_handles: Vec<String>,
}
#[derive(Debug, Default)]
struct State {
    active: BTreeMap<ContextScope, (u64, Cancellation)>,
    in_flight: usize,
    cache: VecDeque<(String, PreparedContext, usize)>,
}
#[derive(Debug, Default)]
pub struct NativeContextProvider {
    state: Mutex<State>,
    sequence: AtomicU64,
    epoch: AtomicU64,
    idle: Condvar,
}
struct Lease<'a> {
    provider: &'a NativeContextProvider,
    scope: ContextScope,
    id: u64,
    cancel: Cancellation,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(mut state) = self.provider.state.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
            self.provider.idle.notify_all();
            if state
                .active
                .get(&self.scope)
                .is_some_and(|(id, _)| *id == self.id)
            {
                state.active.remove(&self.scope);
            }
        }
    }
}
impl NativeContextProvider {
    pub fn cancel(&self, scope: &ContextScope) {
        if let Ok(state) = self.state.lock()
            && let Some((_, cancel)) = state.active.get(scope)
        {
            cancel.cancel();
        }
    }
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    pub fn cancel_all(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(state) = self.state.lock() {
            for (_, cancel) in state.active.values() {
                cancel.cancel();
            }
        }
    }
    pub fn cancel_and_drain(&self, timeout: std::time::Duration) -> Result<(), FriendsError> {
        self.cancel_all();
        let state = self
            .state
            .lock()
            .map_err(|_| invalid("archive context state unavailable"))?;
        let (state, _) = self
            .idle
            .wait_timeout_while(state, timeout, |s| s.in_flight != 0)
            .map_err(|_| invalid("archive drain state unavailable"))?;
        if state.in_flight != 0 {
            return Err(invalid("archive preparation is still cancelling"));
        }
        Ok(())
    }
    pub fn prepare(
        &self,
        scope: ContextScope,
        source_basis: &str,
        dotfile: &Path,
        prefix: &str,
    ) -> Result<Option<PreparedContext>, FriendsError> {
        self.prepare_at_epoch(scope, source_basis, dotfile, prefix, self.epoch())
    }
    /// Capture the epoch before queuing work; closing or switching a project
    /// invalidates jobs that have not started yet as well as active retrieval.
    pub fn prepare_at_epoch(
        &self,
        scope: ContextScope,
        source_basis: &str,
        dotfile: &Path,
        prefix: &str,
        epoch: u64,
    ) -> Result<Option<PreparedContext>, FriendsError> {
        let cancel = Cancellation::default();
        let id = self.sequence.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| invalid("archive context state unavailable"))?;
            if epoch != self.epoch() {
                return Err(invalid("archive preparation was cancelled before starting"));
            }
            if let Some((_, previous)) = state.active.get(&scope) {
                previous.cancel();
            }
            if state.in_flight >= 2 {
                return Err(invalid(
                    "archive context is busy; retry after the current preparation",
                ));
            }
            state.in_flight += 1;
            state.active.insert(scope.clone(), (id, cancel.clone()));
        }
        let _lease = Lease {
            provider: self,
            scope: scope.clone(),
            id,
            cancel: cancel.clone(),
        };
        if !dotfile.try_exists()? {
            return Ok(None);
        }
        let config = FriendsConfig::load(dotfile)?;
        let Some(draft) = invitation_window(&config, prefix)? else {
            return Ok(None);
        };
        let config_hash = fingerprint(&config)?;
        let key = fingerprint(&(
            &scope.project,
            &scope.session,
            &scope.document,
            source_basis,
            &config_hash,
            &draft,
        ))?;
        let cached = {
            let state = self
                .state
                .lock()
                .map_err(|_| invalid("archive context state unavailable"))?;
            state
                .cache
                .iter()
                .find(|(k, _, _)| k == &key)
                .map(|(_, c, _)| c.clone())
        };
        if let Some(cached) = cached {
            // Filesystem operations never hold the provider admission mutex.
            let stamp = &cached.pack.circle.archive;
            let meta = std::fs::metadata(&stamp.path)?;
            let current = meta
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| invalid("invalid archive date"))?
                .as_nanos();
            crate::retrieval::check_wal(&stamp.path)?;
            cancel.check()?;
            if meta.len() == stamp.bytes && current == stamp.modified_unix_nanos {
                return Ok(Some(cached));
            }
        }
        cancel.check()?;
        let circle = retrieve(&config, &draft, &cancel)?;
        let pack = Arc::new(compile(&config, circle)?);
        let prepared = PreparedContext {
            source_basis: source_basis.into(),
            preamble: continuation_context(&pack)?,
            pack,
            config_sha256: config_hash,
            reserved_handles: config
                .friends
                .iter()
                .flat_map(|(alias, friend)| [alias.clone(), friend.handle.clone()])
                .collect(),
        };
        cancel.check()?;
        if fingerprint(&FriendsConfig::load(dotfile)?)? != prepared.config_sha256 {
            return Err(invalid("archive dotfile changed during preparation"));
        }
        let size = serde_json::to_vec(prepared.pack.as_ref())?.len() + prepared.preamble.len();
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("archive context state unavailable"))?;
        if state
            .active
            .get(&scope)
            .is_none_or(|(current, _)| *current != id)
        {
            return Err(invalid("archive preparation was superseded"));
        }
        cancel.check()?;
        state.cache.retain(|(k, _, _)| k != &key);
        if size <= 8 * 1024 * 1024 {
            while state.cache.len() >= 8
                || state.cache.iter().map(|(_, _, n)| n).sum::<usize>() + size > 8 * 1024 * 1024
            {
                state.cache.pop_front();
            }
            state.cache.push_back((key, prepared.clone(), size));
        }
        Ok(Some(prepared))
    }
}
impl Drop for NativeContextProvider {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

/// Keep configured invitations from the manuscript, but search around the
/// current writing. Unconfigured handles remain ordinary prose, not errors.
fn invitation_window(config: &FriendsConfig, prefix: &str) -> Result<Option<String>, FriendsError> {
    if prefix.len() > 2 * 1024 * 1024 {
        return Err(invalid(
            "archive context currently supports manuscript prefixes up to 2 MiB",
        ));
    }
    let parsed = mentions(prefix);
    let mut aliases = Vec::new();
    for m in parsed {
        let suffix = &prefix[m.end..];
        if suffix.starts_with('/')
            || suffix
                .strip_prefix('.')
                .is_some_and(|s| s.starts_with(char::is_alphanumeric))
        {
            continue;
        }
        if let Some((alias, _)) = config.friends.iter().find(|(a, f)| {
            a.eq_ignore_ascii_case(&m.handle) || f.handle.eq_ignore_ascii_case(&m.handle)
        }) && !aliases.contains(alias)
        {
            aliases.push(alias.clone());
        }
    }
    if aliases.is_empty() {
        return Ok(None);
    }
    let mut start = prefix.len().saturating_sub(14000);
    while !prefix.is_char_boundary(start) {
        start += 1;
    }
    let mut topic = prefix[start..].to_string();
    for m in mentions(&topic).iter().rev() {
        topic.replace_range(m.start..m.end, " ");
    }
    let invited = aliases
        .into_iter()
        .map(|a| format!("@{a}"))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some(format!("{invited}\n{topic}")))
}
