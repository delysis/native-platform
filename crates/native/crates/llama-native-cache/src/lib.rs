//! Tiered prefix-cache contracts.
//!
//! This crate is storage-agnostic. It selects compatible native sequence
//! states, manages the bounded in-memory tier, and defines the complete
//! fingerprint that encrypted persistent stores must bind.

use llama_native_types::{PromptForm, PromptTokenPolicy, SequenceStateBlob};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CacheTier {
    MemoryLru,
    SessionPersistent,
    PersonaPack,
}

impl CacheTier {
    const fn lookup_priority(self) -> u8 {
        match self {
            Self::MemoryLru => 3,
            Self::PersonaPack => 2,
            Self::SessionPersistent => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheFingerprint {
    #[serde(default)]
    pub prompt_form: PromptForm,
    #[serde(default)]
    pub prompt_token_policy: PromptTokenPolicy,
    pub model_sha256: String,
    pub binding_version: String,
    pub build_id: String,
    pub tokenizer_sha256: String,
    pub chat_template_sha256: String,
    pub multimodal_projector_sha256: Option<String>,
    #[serde(default)]
    pub lora_adapters_sha256: Vec<String>,
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub max_sequences: u32,
    pub device: String,
    pub rope_config_sha256: String,
    pub kv_layout_sha256: String,
}

impl CacheFingerprint {
    #[must_use]
    pub fn stable_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(format!("{:?}", self.prompt_form).as_bytes());
        hasher.update([0]);
        hasher.update(format!("{:?}", self.prompt_token_policy).as_bytes());
        hasher.update([0]);
        hasher.update(self.model_sha256.as_bytes());
        hasher.update([0]);
        hasher.update(self.binding_version.as_bytes());
        hasher.update([0]);
        hasher.update(self.build_id.as_bytes());
        hasher.update([0]);
        hasher.update(self.tokenizer_sha256.as_bytes());
        hasher.update([0]);
        hasher.update(self.chat_template_sha256.as_bytes());
        hasher.update([0]);
        if let Some(projector) = &self.multimodal_projector_sha256 {
            hasher.update(projector.as_bytes());
        }
        for adapter in &self.lora_adapters_sha256 {
            hasher.update([0]);
            hasher.update(adapter.as_bytes());
        }
        hasher.update(self.context_tokens.to_le_bytes());
        hasher.update(self.batch_tokens.to_le_bytes());
        hasher.update(self.max_sequences.to_le_bytes());
        hasher.update(self.device.as_bytes());
        hasher.update(self.rope_config_sha256.as_bytes());
        hasher.update(self.kv_layout_sha256.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheEntryState {
    Ready,
    Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrefixCacheMetadata {
    pub id: String,
    pub tier: CacheTier,
    pub owner_id: Option<String>,
    pub label: String,
    pub fingerprint: CacheFingerprint,
    pub token_ids: Vec<i32>,
    pub token_sha256: String,
    pub state_bytes: usize,
    pub created_at_ms: u128,
    pub last_used_at_ms: u128,
    pub state: CacheEntryState,
}

impl PrefixCacheMetadata {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        tier: CacheTier,
        fingerprint: CacheFingerprint,
        token_ids: Vec<i32>,
        state_bytes: usize,
        now_ms: u128,
    ) -> Self {
        let id = id.into();
        let token_sha256 = token_sha256(&token_ids);
        Self {
            label: id.clone(),
            id,
            tier,
            owner_id: None,
            fingerprint,
            token_ids,
            token_sha256,
            state_bytes,
            created_at_ms: now_ms,
            last_used_at_ms: now_ms,
            state: CacheEntryState::Ready,
        }
    }

    #[must_use]
    pub fn with_owner(mut self, owner_id: impl Into<String>) -> Self {
        self.owner_id = Some(owner_id.into());
        self
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.state == CacheEntryState::Ready
            && !self.token_ids.is_empty()
            && self.state_bytes > 0
            && self.token_sha256 == token_sha256(&self.token_ids)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrefixCacheValue {
    pub metadata: PrefixCacheMetadata,
    pub sequence: SequenceStateBlob,
}

impl PrefixCacheValue {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.metadata.is_valid()
            && self.sequence.token_count == self.metadata.token_ids.len()
            && self.sequence.token_ids == self.metadata.token_ids
            && self.sequence.bytes.len() == self.metadata.state_bytes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheMatch {
    pub id: String,
    pub tier: CacheTier,
    pub matched_tokens: usize,
    pub exact: bool,
}

#[must_use]
pub fn longest_compatible_prefix(
    entries: &[PrefixCacheMetadata],
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
) -> Option<CacheMatch> {
    longest_compatible_prefix_for_owner(entries, fingerprint, prompt_token_ids, None)
}

#[must_use]
pub fn longest_compatible_prefix_for_owner(
    entries: &[PrefixCacheMetadata],
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
    owner_id: Option<&str>,
) -> Option<CacheMatch> {
    entries
        .iter()
        .filter(|entry| {
            entry.is_valid()
                && &entry.fingerprint == fingerprint
                && owner_id.is_none_or(|owner| entry.owner_id.as_deref() == Some(owner))
                && entry.token_ids.len() < prompt_token_ids.len()
                && prompt_token_ids.starts_with(&entry.token_ids)
        })
        .max_by(|left, right| {
            left.token_ids
                .len()
                .cmp(&right.token_ids.len())
                .then_with(|| {
                    left.tier
                        .lookup_priority()
                        .cmp(&right.tier.lookup_priority())
                })
                .then_with(|| left.last_used_at_ms.cmp(&right.last_used_at_ms))
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|entry| CacheMatch {
            id: entry.id.clone(),
            tier: entry.tier,
            matched_tokens: entry.token_ids.len(),
            exact: entry.token_ids.len().saturating_add(1) == prompt_token_ids.len(),
        })
}

#[derive(Debug)]
pub struct MemoryPrefixCache {
    capacity_bytes: usize,
    used_bytes: usize,
    values: HashMap<String, PrefixCacheValue>,
    least_to_most_recent: VecDeque<String>,
}

impl MemoryPrefixCache {
    #[must_use]
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            capacity_bytes,
            used_bytes: 0,
            values: HashMap::new(),
            least_to_most_recent: VecDeque::new(),
        }
    }

    #[must_use]
    pub const fn capacity_bytes(&self) -> usize {
        self.capacity_bytes
    }

    #[must_use]
    pub const fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn insert(&mut self, mut value: PrefixCacheValue) -> Vec<String> {
        if !value.is_valid() || value.metadata.state_bytes > self.capacity_bytes {
            return Vec::new();
        }
        value.metadata.tier = CacheTier::MemoryLru;
        let id = value.metadata.id.clone();
        if let Some(previous) = self.values.remove(&id) {
            self.used_bytes = self
                .used_bytes
                .saturating_sub(previous.metadata.state_bytes);
            self.remove_from_order(&id);
        }
        self.used_bytes = self.used_bytes.saturating_add(value.metadata.state_bytes);
        self.values.insert(id.clone(), value);
        self.least_to_most_recent.push_back(id);

        let mut evicted = Vec::new();
        while self.used_bytes > self.capacity_bytes {
            let Some(oldest) = self.least_to_most_recent.pop_front() else {
                break;
            };
            if let Some(removed) = self.values.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(removed.metadata.state_bytes);
                evicted.push(oldest);
            }
        }
        evicted
    }

    pub fn lookup(
        &mut self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        now_ms: u128,
    ) -> Option<PrefixCacheValue> {
        let matched = self.best_match(fingerprint, prompt_token_ids)?;
        self.get(&matched.id, now_ms)
    }

    #[must_use]
    pub fn best_match(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
    ) -> Option<CacheMatch> {
        let metadata = self
            .values
            .values()
            .map(|value| value.metadata.clone())
            .collect::<Vec<_>>();
        longest_compatible_prefix(&metadata, fingerprint, prompt_token_ids)
    }

    #[must_use]
    pub fn best_match_for_owner(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        owner_id: &str,
    ) -> Option<CacheMatch> {
        let metadata = self
            .values
            .values()
            .map(|value| value.metadata.clone())
            .collect::<Vec<_>>();
        longest_compatible_prefix_for_owner(
            &metadata,
            fingerprint,
            prompt_token_ids,
            Some(owner_id),
        )
    }

    pub fn get(&mut self, id: &str, now_ms: u128) -> Option<PrefixCacheValue> {
        let mut value = self.values.get(id)?.clone();
        value.metadata.last_used_at_ms = now_ms;
        if let Some(stored) = self.values.get_mut(id) {
            stored.metadata.last_used_at_ms = now_ms;
        }
        self.remove_from_order(id);
        self.least_to_most_recent.push_back(id.to_string());
        Some(value)
    }

    pub fn invalidate(&mut self, id: &str) -> bool {
        let Some(value) = self.values.remove(id) else {
            return false;
        };
        self.used_bytes = self.used_bytes.saturating_sub(value.metadata.state_bytes);
        self.remove_from_order(id);
        true
    }

    pub fn clear(&mut self) {
        self.used_bytes = 0;
        self.values.clear();
        self.least_to_most_recent.clear();
    }

    fn remove_from_order(&mut self, id: &str) {
        self.least_to_most_recent
            .retain(|candidate| candidate != id);
    }
}

#[must_use]
pub fn token_sha256(token_ids: &[i32]) -> String {
    let mut hasher = Sha256::new();
    for token in token_ids {
        hasher.update(token.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> CacheFingerprint {
        CacheFingerprint {
            prompt_form: PromptForm::Chat,
            prompt_token_policy: PromptTokenPolicy::ChatTemplate,
            model_sha256: "model".to_string(),
            binding_version: "binding".to_string(),
            build_id: "build".to_string(),
            tokenizer_sha256: "tokenizer".to_string(),
            chat_template_sha256: "template".to_string(),
            multimodal_projector_sha256: None,
            lora_adapters_sha256: Vec::new(),
            context_tokens: 8192,
            batch_tokens: 512,
            max_sequences: 4,
            device: "metal".to_string(),
            rope_config_sha256: "rope".to_string(),
            kv_layout_sha256: "kv".to_string(),
        }
    }

    fn value(
        id: &str,
        tier: CacheTier,
        tokens: &[i32],
        bytes: usize,
        now_ms: u128,
    ) -> PrefixCacheValue {
        PrefixCacheValue {
            metadata: PrefixCacheMetadata::new(
                id,
                tier,
                fingerprint(),
                tokens.to_vec(),
                bytes,
                now_ms,
            ),
            sequence: SequenceStateBlob {
                sequence_id: 0,
                token_count: tokens.len(),
                bytes: vec![7; bytes],
                token_ids: tokens.to_vec(),
            },
        }
    }

    #[test]
    fn longest_token_exact_compatible_prefix_wins() {
        let entries = [
            value("short", CacheTier::PersonaPack, &[1, 2], 3, 1).metadata,
            value("long", CacheTier::SessionPersistent, &[1, 2, 3], 3, 1).metadata,
            value("wrong", CacheTier::MemoryLru, &[1, 9, 3, 4], 3, 1).metadata,
        ];
        let matched = longest_compatible_prefix(&entries, &fingerprint(), &[1, 2, 3, 4])
            .expect("a compatible cache");
        assert_eq!(matched.id, "long");
        assert_eq!(matched.matched_tokens, 3);
        assert!(matched.exact);
    }

    #[test]
    fn persona_cache_lookup_never_crosses_owner_boundary() {
        let mut alice = value("alice", CacheTier::PersonaPack, &[1, 2], 3, 1).metadata;
        alice.owner_id = Some("persona:alice:v1".to_string());
        let mut bob = value("bob", CacheTier::PersonaPack, &[1, 2, 3], 4, 2).metadata;
        bob.owner_id = Some("persona:bob:v1".to_string());
        let entries = [alice, bob];
        let matched = longest_compatible_prefix_for_owner(
            &entries,
            &fingerprint(),
            &[1, 2, 3, 4],
            Some("persona:alice:v1"),
        )
        .expect("Alice's cache must remain independently addressable");
        assert_eq!(matched.id, "alice");
        assert!(
            longest_compatible_prefix_for_owner(
                &entries,
                &fingerprint(),
                &[1, 2, 3, 4],
                Some("persona:carol:v1"),
            )
            .is_none()
        );
    }

    #[test]
    fn revised_persona_prompt_cannot_reuse_a_stale_incompatible_prefix() {
        let stale = value("persona-v1", CacheTier::PersonaPack, &[10, 20, 30], 3, 1).metadata;
        assert!(
            longest_compatible_prefix(
                std::slice::from_ref(&stale),
                &fingerprint(),
                &[10, 20, 31, 40],
            )
            .is_none(),
            "a changed persona token must invalidate the old cached prefix"
        );
        let compatible = longest_compatible_prefix(
            std::slice::from_ref(&stale),
            &fingerprint(),
            &[10, 20, 30, 40],
        )
        .expect("an unchanged prefix may be extended safely");
        assert_eq!(compatible.id, "persona-v1");
        assert_eq!(compatible.matched_tokens, 3);
    }

    #[test]
    fn every_runtime_fingerprint_dimension_is_authoritative() {
        let baseline = value("cache", CacheTier::PersonaPack, &[1, 2], 3, 1).metadata;
        let prompt = [1, 2, 3];
        let mut candidates = Vec::new();
        let mut changed = fingerprint();
        changed.prompt_form = PromptForm::Completion;
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.prompt_token_policy = PromptTokenPolicy::ExactTokenIds;
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.model_sha256.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.binding_version.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.build_id.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.tokenizer_sha256.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.chat_template_sha256.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.multimodal_projector_sha256 = Some("projector".to_string());
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.lora_adapters_sha256.push("lora".to_string());
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.context_tokens += 1;
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.batch_tokens += 1;
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.max_sequences -= 1;
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.device.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.rope_config_sha256.push('x');
        candidates.push(changed);
        let mut changed = fingerprint();
        changed.kv_layout_sha256.push('x');
        candidates.push(changed);

        for candidate in candidates {
            assert!(
                longest_compatible_prefix(std::slice::from_ref(&baseline), &candidate, &prompt)
                    .is_none()
            );
        }
    }

    #[test]
    fn memory_tier_is_bounded_lru_and_hits_promote_entries() {
        let mut cache = MemoryPrefixCache::new(8);
        assert!(
            cache
                .insert(value("a", CacheTier::PersonaPack, &[1], 4, 1))
                .is_empty()
        );
        assert!(
            cache
                .insert(value("b", CacheTier::PersonaPack, &[2], 4, 2))
                .is_empty()
        );
        assert!(
            cache
                .lookup(&fingerprint(), &[1, 9], 3)
                .is_some_and(|hit| hit.metadata.id == "a")
        );
        let evicted = cache.insert(value("c", CacheTier::PersonaPack, &[3], 4, 4));
        assert_eq!(evicted, vec!["b"]);
        assert!(cache.lookup(&fingerprint(), &[2, 9], 5).is_none());
        assert!(cache.lookup(&fingerprint(), &[1, 9], 5).is_some());
        assert!(cache.lookup(&fingerprint(), &[3, 9], 5).is_some());
        assert_eq!(cache.used_bytes(), 8);
    }

    #[test]
    fn corrupt_metadata_or_state_never_enters_memory_cache() {
        let mut cache = MemoryPrefixCache::new(64);
        let mut corrupt = value("corrupt", CacheTier::PersonaPack, &[1, 2], 4, 1);
        corrupt.metadata.token_sha256 = "wrong".to_string();
        assert!(cache.insert(corrupt).is_empty());
        assert!(cache.is_empty());
    }
}
