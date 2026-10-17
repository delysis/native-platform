//! Tiered prefix-cache contracts.
//!
//! This crate is storage-agnostic. It selects compatible native sequence
//! states, manages the bounded in-memory tier, and defines the complete
//! fingerprint that encrypted persistent stores must bind.

use llama_native_types::{
    PromptForm, PromptTokenPolicy, SequenceReconstruction, SequenceStateBlob,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

mod fingerprint;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconstruction: Option<SequenceReconstruction>,
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
            reconstruction: None,
        }
    }

    /// Reconstruct without reading the potentially large native spill object.
    #[must_use]
    pub fn replay_value(&self) -> Option<PrefixCacheValue> {
        if !self.is_valid() {
            return None;
        }
        let sequence = self
            .reconstruction
            .as_ref()?
            .sequence(self.token_ids.clone())?;
        let mut metadata = self.clone();
        metadata.state_bytes = sequence.bytes.len();
        Some(PrefixCacheValue { metadata, sequence })
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
    /// Bind the small disk index to the exported snapshot once, at publication.
    pub fn bind_reconstruction(&mut self) {
        self.metadata.reconstruction = Some(self.sequence.reconstruction());
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheOwnerScope<'a> {
    Unowned,
    Exact(&'a str),
}

impl CacheOwnerScope<'_> {
    #[must_use]
    pub fn matches(self, owner_id: Option<&str>) -> bool {
        match self {
            Self::Unowned => owner_id.is_none(),
            Self::Exact(expected) => owner_id == Some(expected),
        }
    }
}

#[must_use]
pub fn longest_compatible_prefix(
    entries: &[PrefixCacheMetadata],
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
) -> Option<CacheMatch> {
    longest_compatible_prefix_for_scope(
        entries,
        fingerprint,
        prompt_token_ids,
        CacheOwnerScope::Unowned,
    )
}

#[must_use]
pub fn longest_compatible_prefix_for_owner(
    entries: &[PrefixCacheMetadata],
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
    owner_id: &str,
) -> Option<CacheMatch> {
    longest_compatible_prefix_for_scope(
        entries,
        fingerprint,
        prompt_token_ids,
        CacheOwnerScope::Exact(owner_id),
    )
}

/// Number of evaluated positions shared with a new prompt. Leave the final
/// token live so generation has logits even for an entirely cached prompt.
#[must_use]
pub fn common_prefix_tokens(cached: &[i32], prompt: &[i32]) -> usize {
    cached
        .iter()
        .zip(prompt)
        .take(prompt.len().saturating_sub(1))
        .take_while(|(a, b)| a == b)
        .count()
}

#[must_use]
pub fn longest_compatible_prefix_for_scope(
    entries: &[PrefixCacheMetadata],
    fingerprint: &CacheFingerprint,
    prompt_token_ids: &[i32],
    owner_scope: CacheOwnerScope<'_>,
) -> Option<CacheMatch> {
    entries
        .iter()
        .filter(|entry| {
            entry.is_valid()
                && &entry.fingerprint == fingerprint
                && owner_scope.matches(entry.owner_id.as_deref())
        })
        .map(|entry| {
            (
                entry,
                common_prefix_tokens(&entry.token_ids, prompt_token_ids),
            )
        })
        .filter(|(_, matched)| *matched > 0)
        .max_by(|(left, l), (right, r)| {
            l.cmp(r)
                .then_with(|| {
                    left.tier
                        .lookup_priority()
                        .cmp(&right.tier.lookup_priority())
                })
                .then_with(|| left.last_used_at_ms.cmp(&right.last_used_at_ms))
                .then_with(|| left.id.cmp(&right.id))
        })
        .map(|(entry, matched)| CacheMatch {
            id: entry.id.clone(),
            tier: entry.tier,
            matched_tokens: matched,
            exact: matched + 1 == prompt_token_ids.len(),
        })
}

/// Complete cache-admission result. Store failures remain typed errors at the
/// host boundary; policy and authority refusals are explicit outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheInsertOutcome {
    Disabled,
    InvalidRejected,
    OwnerRejected,
    Stored {
        memory_resident: bool,
        persistent_resident: bool,
        evicted: Vec<String>,
    },
}

#[derive(Debug)]
struct CachedPrefix {
    value: Arc<PrefixCacheValue>,
    last_used_at_ms: u128,
    memory_bytes: usize,
}

/// Validation and retained-allocation accounting precede the host-wide lock.
pub struct ValidatedPrefixCacheValue {
    value: PrefixCacheValue,
    memory_bytes: usize,
}
impl ValidatedPrefixCacheValue {
    pub fn new(value: PrefixCacheValue) -> Option<Self> {
        if !value.is_valid() {
            return None;
        }
        let memory_bytes = retained_memory_bytes(&value)?;
        Some(Self {
            value,
            memory_bytes,
        })
    }

    /// Charge for this resident value, including token/metadata capacities and
    /// its two cache-owned ID copies. Shared snapshot allocations are charged
    /// in full per entry, not discounted on the assumption of another owner.
    #[must_use]
    pub const fn memory_bytes(&self) -> usize {
        self.memory_bytes
    }
}

fn retained_memory_bytes(value: &PrefixCacheValue) -> Option<usize> {
    use std::mem::size_of;

    // Exhaustive destructuring keeps newly retained fields in the accounting review.
    let PrefixCacheValue { metadata, sequence } = value;
    let PrefixCacheMetadata {
        id,
        tier: _,
        owner_id,
        label,
        fingerprint,
        token_ids,
        token_sha256,
        state_bytes: _,
        created_at_ms: _,
        last_used_at_ms: _,
        state: _,
        reconstruction,
    } = metadata;
    let CacheFingerprint {
        prompt_form: _,
        prompt_token_policy: _,
        model_sha256,
        binding_version,
        build_id,
        tokenizer_sha256,
        chat_template_sha256,
        multimodal_projector_sha256,
        lora_adapters_sha256,
        context_tokens: _,
        batch_tokens: _,
        max_sequences: _,
        device,
        rope_config_sha256,
        kv_layout_sha256,
    } = fingerprint;
    let SequenceStateBlob {
        sequence_id: _,
        token_count: _,
        bytes,
        token_ids: sequence_tokens,
    } = sequence;
    let reconstruction_bytes = match reconstruction {
        Some(SequenceReconstruction {
            sequence_id: _,
            header,
            export_receipt: _,
        }) => header.capacity(),
        None => 0,
    };
    let mut total = [
        size_of::<PrefixCacheValue>(),
        size_of::<CachedPrefix>(),
        2 * size_of::<Box<str>>(), // map key and LRU key
        size_of::<Vec<u8>>(),      // the snapshot's Arc allocation owns a Vec header
        4 * size_of::<usize>(),    // strong/weak counters for both Arc allocations
        id.len().checked_mul(2)?,  // boxed cache keys have no spare capacity
        id.capacity(),
        owner_id.as_ref().map_or(0, String::capacity),
        label.capacity(),
        token_sha256.capacity(),
        token_ids.capacity().checked_mul(size_of::<i32>())?,
        sequence_tokens.capacity().checked_mul(size_of::<i32>())?,
        bytes.capacity(),
        reconstruction_bytes,
        lora_adapters_sha256
            .capacity()
            .checked_mul(size_of::<String>())?,
        multimodal_projector_sha256
            .as_ref()
            .map_or(0, String::capacity),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)?;
    for text in [
        model_sha256,
        binding_version,
        build_id,
        tokenizer_sha256,
        chat_template_sha256,
        device,
        rope_config_sha256,
        kv_layout_sha256,
    ]
    .into_iter()
    .chain(lora_adapters_sha256)
    {
        total = total.checked_add(text.capacity())?;
    }
    Some(total)
}

/// A budget for resident value allocations and their cache-owned keys.
///
/// The charge includes allocated capacities, not just serialized state length.
/// Allocator bookkeeping and map/deque spare buckets are not an RSS measurement;
/// caller-held Arc values and lookup copies remain outside this cache's budget.
#[derive(Debug)]
pub struct MemoryPrefixCache {
    capacity_bytes: usize,
    used_bytes: usize,
    values: HashMap<Box<str>, CachedPrefix>,
    least_to_most_recent: VecDeque<Box<str>>,
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

    #[must_use]
    pub fn contains_id(&self, id: &str) -> bool {
        self.values.contains_key(id)
    }

    pub fn insert(&mut self, value: PrefixCacheValue) -> CacheInsertOutcome {
        let Some(value) = ValidatedPrefixCacheValue::new(value) else {
            return CacheInsertOutcome::InvalidRejected;
        };
        self.insert_validated(value)
    }

    pub fn insert_validated(&mut self, value: ValidatedPrefixCacheValue) -> CacheInsertOutcome {
        let ValidatedPrefixCacheValue {
            mut value,
            memory_bytes,
        } = value;
        if memory_bytes > self.capacity_bytes {
            return CacheInsertOutcome::InvalidRejected;
        }
        value.metadata.tier = CacheTier::MemoryLru;
        let id: Box<str> = value.metadata.id.as_str().into();
        if let Some(previous) = self.values.remove(&id) {
            self.used_bytes -= previous.memory_bytes;
            self.remove_from_order(&id);
        }

        // Make room before adding: even a usize::MAX budget cannot overflow
        // the accounting sum or evict the newly admitted value itself.
        let mut evicted = Vec::new();
        while self.used_bytes > self.capacity_bytes - memory_bytes {
            let oldest = self
                .least_to_most_recent
                .pop_front()
                .expect("charged entries retain an LRU position");
            if let Some(removed) = self.values.remove(&oldest) {
                self.used_bytes -= removed.memory_bytes;
                evicted.push(oldest.into_string());
            }
        }
        self.used_bytes += memory_bytes;
        self.values.insert(
            id.clone(),
            CachedPrefix {
                last_used_at_ms: value.metadata.last_used_at_ms,
                value: Arc::new(value),
                memory_bytes,
            },
        );
        self.least_to_most_recent.push_back(id);
        CacheInsertOutcome::Stored {
            memory_resident: true,
            persistent_resident: false,
            evicted,
        }
    }

    pub fn lookup(
        &mut self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        now_ms: u128,
    ) -> Option<PrefixCacheValue> {
        self.lookup_in_scope(
            fingerprint,
            prompt_token_ids,
            CacheOwnerScope::Unowned,
            now_ms,
        )
    }

    pub fn lookup_in_scope(
        &mut self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        owner_scope: CacheOwnerScope<'_>,
        now_ms: u128,
    ) -> Option<PrefixCacheValue> {
        let matched = self.best_match_in_scope(fingerprint, prompt_token_ids, owner_scope)?;
        self.get(&matched.id, now_ms)
    }

    #[must_use]
    pub fn best_match(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
    ) -> Option<CacheMatch> {
        self.best_match_in_scope(fingerprint, prompt_token_ids, CacheOwnerScope::Unowned)
    }

    #[must_use]
    pub fn best_match_in_scope(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        owner_scope: CacheOwnerScope<'_>,
    ) -> Option<CacheMatch> {
        self.values
            .values()
            .filter(|cached| {
                &cached.value.metadata.fingerprint == fingerprint
                    && owner_scope.matches(cached.value.metadata.owner_id.as_deref())
            })
            .map(|cached| {
                (
                    cached,
                    common_prefix_tokens(&cached.value.metadata.token_ids, prompt_token_ids),
                )
            })
            .filter(|(_, matched)| *matched > 0)
            .max_by(|(left, l), (right, r)| {
                l.cmp(r)
                    .then_with(|| left.last_used_at_ms.cmp(&right.last_used_at_ms))
                    .then_with(|| left.value.metadata.id.cmp(&right.value.metadata.id))
            })
            .map(|(cached, matched)| CacheMatch {
                id: cached.value.metadata.id.clone(),
                tier: cached.value.metadata.tier,
                matched_tokens: matched,
                exact: matched + 1 == prompt_token_ids.len(),
            })
    }

    #[must_use]
    pub fn best_match_for_owner(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        owner_id: &str,
    ) -> Option<CacheMatch> {
        self.best_match_in_scope(
            fingerprint,
            prompt_token_ids,
            CacheOwnerScope::Exact(owner_id),
        )
    }

    pub fn get(&mut self, id: &str, now_ms: u128) -> Option<PrefixCacheValue> {
        let mut value = (*self.get_shared(id, now_ms)?).clone();
        value.metadata.last_used_at_ms = now_ms;
        Some(value)
    }

    /// Returning shared immutable data does not confer owner/use authority.
    pub fn get_shared(&mut self, id: &str, now_ms: u128) -> Option<Arc<PrefixCacheValue>> {
        let stored = self.values.get_mut(id)?;
        stored.last_used_at_ms = now_ms;
        let value = Arc::clone(&stored.value);
        self.remove_from_order(id);
        self.least_to_most_recent.push_back(id.into());
        Some(value)
    }

    pub fn lookup_shared_in_scope(
        &mut self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        owner_scope: CacheOwnerScope<'_>,
        now_ms: u128,
    ) -> Option<Arc<PrefixCacheValue>> {
        let matched = self.best_match_in_scope(fingerprint, prompt_token_ids, owner_scope)?;
        self.get_shared(&matched.id, now_ms)
    }

    pub fn invalidate(&mut self, id: &str) -> bool {
        let Some(value) = self.values.remove(id) else {
            return false;
        };
        self.used_bytes -= value.memory_bytes;
        self.remove_from_order(id);
        true
    }

    pub fn invalidate_owner(&mut self, owner_id: &str) -> Vec<String> {
        let ids = self
            .values
            .values()
            .filter(|value| value.value.metadata.owner_id.as_deref() == Some(owner_id))
            .map(|value| value.value.metadata.id.clone())
            .collect::<Vec<_>>();
        for id in &ids {
            self.invalidate(id);
        }
        ids
    }

    pub fn clear(&mut self) {
        self.used_bytes = 0;
        self.values = HashMap::new();
        self.least_to_most_recent = VecDeque::new();
    }

    fn remove_from_order(&mut self, id: &str) {
        self.least_to_most_recent
            .retain(|candidate| candidate.as_ref() != id);
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
                bytes: (vec![7; bytes]).into(),
                token_ids: tokens.to_vec(),
            },
        }
    }

    fn owned_value(
        id: &str,
        owner_id: &str,
        tokens: &[i32],
        bytes: usize,
        now_ms: u128,
    ) -> PrefixCacheValue {
        let mut value = value(id, CacheTier::PersonaPack, tokens, bytes, now_ms);
        value.metadata.owner_id = Some(owner_id.to_string());
        value
    }

    #[test]
    fn shared_cache_hits_do_not_copy_payload_or_retain_owner_authority() {
        for count in [1, 16] {
            let mut cache = MemoryPrefixCache::new(count * 512 * 1024);
            let mut other_bytes = 0;
            for index in 1..count {
                cache.insert(owned_value(
                    &format!("other-{index}"),
                    "bob",
                    &[1, 2],
                    256 * 1024,
                    1,
                ));
                other_bytes = cache.used_bytes();
            }
            let original = owned_value("owned", "alice", &[1, 2], 256 * 1024, 1);
            let bytes = Arc::clone(&original.sequence.bytes);
            cache.insert(original);
            let first = cache
                .lookup_shared_in_scope(
                    &fingerprint(),
                    &[1, 2, 3],
                    CacheOwnerScope::Exact("alice"),
                    2,
                )
                .expect("first");
            let next = cache
                .lookup_shared_in_scope(
                    &fingerprint(),
                    &[1, 2, 4],
                    CacheOwnerScope::Exact("alice"),
                    3,
                )
                .expect("next");
            assert!(Arc::ptr_eq(&first, &next));
            assert!(Arc::ptr_eq(&bytes, &next.sequence.bytes));
            let mut caller_copy = first.as_ref().clone();
            Arc::make_mut(&mut caller_copy.sequence.bytes)[0] = 99;
            assert_eq!(next.sequence.bytes[0], 7);
            assert_eq!(cache.invalidate_owner("alice"), vec!["owned"]);
            assert!(
                cache
                    .lookup_shared_in_scope(
                        &fingerprint(),
                        &[1, 2, 3],
                        CacheOwnerScope::Exact("alice"),
                        4
                    )
                    .is_none()
            );
            assert_eq!(first.sequence.bytes.len(), 256 * 1024);
            assert_eq!(cache.used_bytes(), other_bytes);
            assert_eq!(cache.len(), count - 1);
        }
    }

    #[test]
    fn checked_insert_distinguishes_invalid_capacity_and_admitted() {
        let fits = value("fits", CacheTier::MemoryLru, &[1], 4, 0);
        let capacity = ValidatedPrefixCacheValue::new(fits.clone())
            .expect("valid fixture")
            .memory_bytes();
        let mut cache = MemoryPrefixCache::new(capacity);
        let mut malformed = value("bad", CacheTier::MemoryLru, &[1], 4, 0);
        malformed.sequence.token_count = 2;
        assert_eq!(cache.insert(malformed), CacheInsertOutcome::InvalidRejected);
        assert_eq!(
            cache.insert(value("big", CacheTier::MemoryLru, &[1], capacity + 1, 0)),
            CacheInsertOutcome::InvalidRejected
        );
        assert!(cache.is_empty());
        assert_eq!(
            cache.insert(fits),
            CacheInsertOutcome::Stored {
                memory_resident: true,
                persistent_resident: false,
                evicted: Vec::new(),
            }
        );
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
            "persona:alice:v1",
        )
        .expect("Alice's cache must remain independently addressable");
        assert_eq!(matched.id, "alice");
        assert!(
            longest_compatible_prefix_for_owner(
                &entries,
                &fingerprint(),
                &[1, 2, 3, 4],
                "persona:carol:v1",
            )
            .is_none()
        );
    }

    #[test]
    fn ownerless_lookup_never_admits_owned_prefixes() {
        let unowned = value("unowned", CacheTier::SessionPersistent, &[1], 3, 1).metadata;
        let owned = value("persona", CacheTier::PersonaPack, &[1, 2], 3, 2)
            .metadata
            .with_owner("persona:archived:v7");

        let matched =
            longest_compatible_prefix(&[unowned, owned.clone()], &fingerprint(), &[1, 2, 3])
                .expect("the ownerless artifact remains independently addressable");

        assert_eq!(matched.id, "unowned");
        assert_eq!(matched.matched_tokens, 1);
        assert!(
            longest_compatible_prefix(&[owned], &fingerprint(), &[1, 2, 3]).is_none(),
            "an ownerless lookup must be a miss when only owner-bound artifacts exist"
        );
    }

    #[test]
    fn revised_persona_prompt_reuses_only_its_exact_common_prefix() {
        let stale = value("persona-v1", CacheTier::PersonaPack, &[10, 20, 30], 3, 1).metadata;
        let revised = longest_compatible_prefix(
            std::slice::from_ref(&stale),
            &fingerprint(),
            &[10, 20, 31, 40],
        )
        .expect("the unchanged leading tokens remain reusable");
        assert_eq!(revised.matched_tokens, 2);
        assert!(!revised.exact);
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
    fn longest_shared_prefix_beats_a_longer_stored_entry() {
        let longer_but_divergent = value(
            "long-stale",
            CacheTier::PersonaPack,
            &[1, 2, 99, 4, 5, 6],
            3,
            10,
        )
        .metadata;
        let shorter_and_matching = value(
            "short-match",
            CacheTier::SessionPersistent,
            &[1, 2, 3, 4, 5],
            3,
            1,
        )
        .metadata;
        let matched = longest_compatible_prefix(
            &[longer_but_divergent, shorter_and_matching],
            &fingerprint(),
            &[1, 2, 3, 4, 5, 6],
        )
        .expect("a compatible cached prefix exists");
        assert_eq!(matched.id, "short-match");
        assert_eq!(matched.matched_tokens, 5);
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
        let entry_bytes =
            ValidatedPrefixCacheValue::new(value("a", CacheTier::PersonaPack, &[1], 4, 1))
                .expect("valid fixture")
                .memory_bytes();
        let mut cache = MemoryPrefixCache::new(2 * entry_bytes);
        assert!(matches!(
            cache.insert(value("a", CacheTier::PersonaPack, &[1], 4, 1)),
            CacheInsertOutcome::Stored { evicted, .. } if evicted.is_empty()
        ));
        assert!(matches!(
            cache.insert(value("b", CacheTier::PersonaPack, &[2], 4, 2)),
            CacheInsertOutcome::Stored { evicted, .. } if evicted.is_empty()
        ));
        assert!(
            cache
                .lookup(&fingerprint(), &[1, 9], 3)
                .is_some_and(|hit| hit.metadata.id == "a")
        );
        assert!(matches!(
            cache.insert(value("c", CacheTier::PersonaPack, &[3], 4, 4)),
            CacheInsertOutcome::Stored { evicted, .. } if evicted == vec!["b"]
        ));
        assert!(cache.lookup(&fingerprint(), &[2, 9], 5).is_none());
        assert!(cache.lookup(&fingerprint(), &[1, 9], 5).is_some());
        assert!(cache.lookup(&fingerprint(), &[3, 9], 5).is_some());
        assert_eq!(cache.used_bytes(), 2 * entry_bytes);
    }

    #[test]
    fn memory_owner_invalidation_is_exact_and_updates_capacity_accounting() {
        let bob_bytes = ValidatedPrefixCacheValue::new(owned_value("bob", "bob", &[3], 4, 3))
            .expect("valid fixture")
            .memory_bytes();
        let mut cache = MemoryPrefixCache::new(64 * 1024);
        cache.insert(owned_value("alice-a", "alice", &[1], 4, 1));
        cache.insert(owned_value("alice-b", "alice", &[2], 4, 2));
        cache.insert(owned_value("bob", "bob", &[3], 4, 3));

        let mut removed = cache.invalidate_owner("alice");
        removed.sort();
        assert_eq!(removed, ["alice-a".to_string(), "alice-b".to_string()]);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.used_bytes(), bob_bytes);
        assert!(cache.get("bob", 4).is_some());
        assert!(cache.get("alice-a", 4).is_none());
    }

    #[test]
    fn corrupt_metadata_or_state_never_enters_memory_cache() {
        let mut cache = MemoryPrefixCache::new(64);
        let mut corrupt = value("corrupt", CacheTier::PersonaPack, &[1, 2], 4, 1);
        corrupt.metadata.token_sha256 = "wrong".to_string();
        assert_eq!(cache.insert(corrupt), CacheInsertOutcome::InvalidRejected);
        assert!(cache.is_empty());
    }
    #[test]
    fn retained_token_and_metadata_allocations_cannot_hide_behind_tiny_state() {
        let mut cache = MemoryPrefixCache::new(4096);
        assert!(matches!(
            cache.insert(value("kept", CacheTier::MemoryLru, &[1], 1, 0)),
            CacheInsertOutcome::Stored { .. }
        ));
        let before = cache.used_bytes();
        let tokens = vec![1; 16 * 1024];
        let mut candidates = vec![value("kept", CacheTier::MemoryLru, &tokens, 1, 0)];

        let mut spare_state = value("state-capacity", CacheTier::MemoryLru, &[1], 1, 0);
        let mut bytes = Vec::with_capacity(16 * 1024);
        bytes.push(7);
        spare_state.sequence.bytes = Arc::new(bytes);
        candidates.push(spare_state);

        let mut metadata = value("metadata", CacheTier::MemoryLru, &[1], 1, 0);
        metadata.metadata.label = String::with_capacity(16 * 1024);
        metadata.metadata.label.push('x');
        candidates.push(metadata);

        let mut fingerprint_value = value("fingerprint", CacheTier::MemoryLru, &[1], 1, 0);
        fingerprint_value.metadata.fingerprint.lora_adapters_sha256 = vec!["x".repeat(16 * 1024)];
        candidates.push(fingerprint_value);

        let mut spare_tokens = value("token-capacity", CacheTier::MemoryLru, &[1], 1, 0);
        spare_tokens.sequence.token_ids.reserve(16 * 1024);
        candidates.push(spare_tokens);

        let mut reconstruction = value("reconstruction", CacheTier::MemoryLru, &[1], 1, 0);
        reconstruction.bind_reconstruction();
        reconstruction
            .metadata
            .reconstruction
            .as_mut()
            .expect("bound reconstruction")
            .header
            .reserve(16 * 1024);
        candidates.push(reconstruction);

        for candidate in candidates {
            assert!(candidate.is_valid(), "capacity is not corrupt metadata");
            assert_eq!(candidate.metadata.state_bytes, 1);
            assert_eq!(cache.insert(candidate), CacheInsertOutcome::InvalidRejected);
            assert_eq!(cache.len(), 1, "rejection cannot evict the existing entry");
            assert_eq!(cache.used_bytes(), before);
            assert_eq!(
                cache
                    .get("kept", 1)
                    .expect("old value retained")
                    .sequence
                    .token_count,
                1
            );
        }
    }

    #[test]
    fn tiny_state_entries_still_consume_a_finite_resident_budget() {
        let mut cache = MemoryPrefixCache::new(4096);
        for index in 0..100 {
            assert!(matches!(
                cache.insert(value(
                    &format!("entry-{index}"),
                    CacheTier::MemoryLru,
                    &[1],
                    1,
                    0
                )),
                CacheInsertOutcome::Stored {
                    memory_resident: true,
                    ..
                }
            ));
            assert!(cache.used_bytes() > 0);
            assert!(cache.used_bytes() <= cache.capacity_bytes());
        }
        assert!(
            cache.len() < 100,
            "tiny snapshots cannot bypass allocation accounting"
        );
        assert!(cache.get("entry-99", 1).is_some());
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.used_bytes(), 0);
    }
}
