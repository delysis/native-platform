//! Bounded, cancellable context preparation for native editors. The host owns
//! execution and checks its document/session authority again before use.
use crate::{
    Cancellation, FriendsConfig, FriendsError, PromptPack, compile, continuation_context,
    fingerprint, invalid,
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
impl PreparedContext {
    /// Revalidate the selected configuration and frozen snapshot immediately
    /// before returning preparation to a host. This performs filesystem I/O;
    /// callers must not hold their document/model admission mutexes.
    pub fn validate_snapshot(
        &self,
        dotfile: &Path,
        source_basis: &str,
    ) -> Result<(), FriendsError> {
        if self.source_basis != source_basis {
            return Err(invalid("archive context source basis changed"));
        }
        self.pack.verify()?;
        if fingerprint(&FriendsConfig::load(dotfile)?)? != self.config_sha256 {
            return Err(invalid("archive dotfile changed after preparation"));
        }
        let stamp = &self.pack.circle.archive;
        let metadata = std::fs::metadata(&stamp.path)?;
        let modified = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid("invalid archive date"))?
            .as_nanos();
        crate::retrieval::check_wal(&stamp.path)?;
        if metadata.len() != stamp.bytes || modified != stamp.modified_unix_nanos {
            return Err(invalid("archive snapshot changed after preparation"));
        }
        Ok(())
    }
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
        let mut state = self
            .provider
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    /// Final runtime-exit boundary: recover a poisoned registry, cancel every
    /// admitted operation and wait until its lease has released all archive I/O.
    pub fn cancel_and_drain_for_exit(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, cancel) in state.active.values() {
            cancel.cancel();
        }
        while state.in_flight != 0 {
            state = self
                .idle
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
    fn admit(&self, scope: ContextScope, epoch: u64) -> Result<Lease<'_>, FriendsError> {
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
        Ok(Lease {
            provider: self,
            scope,
            id,
            cancel,
        })
    }

    /// Read the selected configuration under the same cancellation and drain
    /// authority as retrieval. This does not open the archive or call a model.
    pub fn inspect_config_at_epoch(
        &self,
        scope: ContextScope,
        dotfile: &Path,
        epoch: u64,
    ) -> Result<Option<FriendsConfig>, FriendsError> {
        let lease = self.admit(scope, epoch)?;
        lease.cancel.check()?;
        let config = if dotfile.try_exists()? {
            Some(FriendsConfig::load(dotfile)?)
        } else {
            None
        };
        lease.cancel.check()?;
        Ok(config)
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
        let lease = self.admit(scope.clone(), epoch)?;
        let cancel = &lease.cancel;
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
                cached.validate_snapshot(dotfile, source_basis)?;
                cancel.check()?;
                return Ok(Some(cached));
            }
        }
        cancel.check()?;
        let circle = crate::retrieval::retrieve_prepared_invitation(&config, &draft, cancel)?;
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
        prepared.validate_snapshot(dotfile, source_basis)?;
        cancel.check()?;
        let size = serde_json::to_vec(prepared.pack.as_ref())?.len() + prepared.preamble.len();
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("archive context state unavailable"))?;
        if state
            .active
            .get(&scope)
            .is_none_or(|(current, _)| *current != lease.id)
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
    let parsed = workspace_document::references::document_references(prefix)
        .map_err(|error| invalid(error.to_string()))?;
    let reserved: Vec<_> = config
        .friends
        .iter()
        .flat_map(|(alias, friend)| [alias.clone(), friend.handle.clone()])
        .collect();
    let mut aliases = Vec::new();
    for reference in &parsed {
        if !is_friend_invitation(prefix, reference, &reserved) {
            continue;
        }
        if let Some((alias, _)) = config.friends.iter().find(|(a, f)| {
            a.eq_ignore_ascii_case(&reference.name)
                || f.handle.eq_ignore_ascii_case(&reference.name)
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
    // Keep the full document's lexical state: the topic window can begin
    // inside a fence or quote and must not reinterpret its contents as prose.
    for reference in parsed.iter().rev() {
        if reference.range.start >= start && is_friend_invitation(prefix, reference, &reserved) {
            topic.replace_range(
                reference.range.start - start..reference.range.end - start,
                " ",
            );
        }
    }
    let invited = aliases
        .into_iter()
        .map(|a| format!("@{a}"))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some(format!("{invited}\n{topic}")))
}

/// Only an authored bare configured handle opts into archive context. Quoted,
/// scoped, path and retained-link references continue to name workspace values.
/// Use the original UTF-8 source and the shared grammar's exact range.
pub fn is_friend_invitation(
    source: &str,
    reference: &workspace_document::references::DocumentReference,
    reserved_handles: &[String],
) -> bool {
    let Some(raw) = source.get(reference.range.clone()) else {
        return false;
    };
    let Some(handle) = raw.strip_prefix('@') else {
        return false;
    };
    crate::config::valid_handle(handle)
        && handle == reference.name
        && reserved_handles
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(handle))
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    #[test]
    fn lease_release_survives_a_poisoned_registry_before_exit_drain() {
        let provider = NativeContextProvider::default();
        let scope = ContextScope {
            project: "p".into(),
            session: "s".into(),
            document: "d".into(),
        };
        let cancel = Cancellation::default();
        {
            let mut state = provider.state.lock().expect("controlled registry");
            state.in_flight = 1;
            state.active.insert(scope.clone(), (7, cancel.clone()));
        }
        let lease = Lease {
            provider: &provider,
            scope,
            id: 7,
            cancel: cancel.clone(),
        };
        let poisoned = std::panic::catch_unwind(|| {
            let _guard = provider.state.lock().expect("controlled registry");
            panic!("controlled registry poison");
        });
        assert!(poisoned.is_err());
        drop(lease);
        let state = provider
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.in_flight, 0);
        assert!(state.active.is_empty());
        drop(state);
        assert!(cancel.check().is_err());
        provider.cancel_and_drain_for_exit();
    }
    #[test]
    fn topic_window_keeps_full_document_code_state() {
        let mut config = FriendsConfig::default();
        config.friends.insert(
            "a".into(),
            crate::FriendConfig {
                handle: "a".into(),
                label: None,
                account_id: None,
            },
        );
        let source = format!("@a invited\n~~~\n{}\n@a code\n~~~", "x".repeat(14_100));
        let window = invitation_window(&config, &source)
            .expect("controlled grammar fixture")
            .expect("authored invitation");
        assert!(window.starts_with("@a\n"));
        assert!(window.contains("@a code"));
    }
    #[test]
    fn aliases_preserve_explicit_workspace_reference_domains() {
        let source = "☀ @visa @VISA @\"visa\" @“visa” @visa.md @visa/path @project:visa [visa](loom://document/visa)";
        let references = workspace_document::references::document_references(source)
            .expect("controlled grammar fixture");
        let handles = vec!["visa".to_string()];
        let invited: Vec<_> = references
            .iter()
            .filter(|reference| is_friend_invitation(source, reference, &handles))
            .map(|reference| &source[reference.range.clone()])
            .collect();
        assert_eq!(invited, ["@visa", "@VISA"]);
        assert!(references.iter().any(|reference| reference.name == "visa"
            && !is_friend_invitation(source, reference, &handles)));
    }
}
