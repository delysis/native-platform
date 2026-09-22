use crate::CacheFingerprint;
use llama_native_types::{PromptForm, PromptTokenPolicy};
use sha2::{Digest, Sha256};

impl CacheFingerprint {
    /// SHA-256 of the version-two canonical fingerprint encoding.
    ///
    /// This intentionally changes IDs from the ambiguous legacy concatenation.
    /// Persisted IDs are opaque; an ID alone never authorizes cache reuse.
    /// Callers must still compare the complete fingerprint and owner scope.
    #[must_use]
    pub fn stable_id(&self) -> String {
        // No `..`: adding a field must also update the identity encoding.
        let Self {
            prompt_form,
            prompt_token_policy,
            model_sha256,
            binding_version,
            build_id,
            tokenizer_sha256,
            chat_template_sha256,
            multimodal_projector_sha256,
            lora_adapters_sha256,
            context_tokens,
            batch_tokens,
            max_sequences,
            device,
            rope_config_sha256,
            kv_layout_sha256,
        } = self;
        let mut hasher = Sha256::new();
        hasher.update(b"llama-native.cache-fingerprint.v2\0");
        // Explicit wire tags, independent of Rust Debug or enum layout.
        hasher.update([match prompt_form {
            PromptForm::Chat => 0,
            PromptForm::Completion => 1,
            PromptForm::FillInMiddle => 2,
        }]);
        hasher.update([match prompt_token_policy {
            PromptTokenPolicy::ChatTemplate => 0,
            PromptTokenPolicy::NoBosParseSpecial => 1,
            PromptTokenPolicy::AddBosParseSpecial => 2,
            PromptTokenPolicy::ExactTokenIds => 3,
            PromptTokenPolicy::FillInMiddleModelTokens => 4,
        }]);
        for value in [
            model_sha256,
            binding_version,
            build_id,
            tokenizer_sha256,
            chat_template_sha256,
        ] {
            frame(&mut hasher, value);
        }
        match multimodal_projector_sha256 {
            None => hasher.update([0]),
            Some(projector) => {
                hasher.update([1]);
                frame(&mut hasher, projector);
            }
        }
        hasher.update((lora_adapters_sha256.len() as u64).to_le_bytes());
        for adapter in lora_adapters_sha256 {
            frame(&mut hasher, adapter);
        }
        hasher.update(context_tokens.to_le_bytes());
        hasher.update(batch_tokens.to_le_bytes());
        hasher.update(max_sequences.to_le_bytes());
        for value in [device, rope_config_sha256, kv_layout_sha256] {
            frame(&mut hasher, value);
        }
        format!("{:x}", hasher.finalize())
    }
}

// UTF-8 byte length, not character count; zero bytes cannot act as delimiters.
fn frame(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use crate::{CacheFingerprint, CacheTier, PrefixCacheMetadata, longest_compatible_prefix};
    use llama_native_types::{PromptForm, PromptTokenPolicy};

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

    #[test]
    fn adjacent_string_fields_cannot_alias() {
        let left = fingerprint();
        let mut right = left.clone();
        right.device.push('r');
        right.rope_config_sha256 = "ope".to_string();
        assert_ne!(left, right);
        assert_ne!(left.stable_id(), right.stable_id());
    }

    #[test]
    fn embedded_nul_cannot_move_a_field_boundary() {
        let mut left = fingerprint();
        left.binding_version = "a\0b".to_string();
        left.build_id = "c".to_string();
        let mut right = left.clone();
        right.binding_version = "a".to_string();
        right.build_id = "b\0c".to_string();
        assert_ne!(left.stable_id(), right.stable_id());
    }

    #[test]
    fn absent_and_present_empty_projectors_remain_distinct() {
        let absent = fingerprint();
        let mut present = absent.clone();
        present.multimodal_projector_sha256 = Some(String::new());
        assert_ne!(absent.stable_id(), present.stable_id());
    }

    #[test]
    fn adapter_boundaries_and_order_are_identity_bearing() {
        let mut joined = fingerprint();
        joined.lora_adapters_sha256 = vec!["a\0b".to_string()];
        let mut split = joined.clone();
        split.lora_adapters_sha256 = vec!["a".to_string(), "b".to_string()];
        assert_ne!(joined.stable_id(), split.stable_id());
        let mut reversed = split.clone();
        reversed.lora_adapters_sha256.reverse();
        assert_ne!(split.stable_id(), reversed.stable_id());
    }

    #[test]
    fn every_field_changes_the_fingerprint_id() {
        let base = fingerprint();
        let changes: [fn(&mut CacheFingerprint); 15] = [
            |v| v.prompt_form = PromptForm::Completion,
            |v| v.prompt_token_policy = PromptTokenPolicy::ExactTokenIds,
            |v| v.model_sha256.push('x'),
            |v| v.binding_version.push('x'),
            |v| v.build_id.push('x'),
            |v| v.tokenizer_sha256.push('x'),
            |v| v.chat_template_sha256.push('x'),
            |v| v.multimodal_projector_sha256 = Some("projector".to_string()),
            |v| v.lora_adapters_sha256.push("adapter".to_string()),
            |v| v.context_tokens += 1,
            |v| v.batch_tokens += 1,
            |v| v.max_sequences += 1,
            |v| v.device.push('x'),
            |v| v.rope_config_sha256.push('x'),
            |v| v.kv_layout_sha256.push('x'),
        ];
        for change in changes {
            let mut changed = base.clone();
            change(&mut changed);
            assert_ne!(base.stable_id(), changed.stable_id(), "{changed:?}");
        }
    }

    #[test]
    fn cache_admission_still_requires_full_fingerprint_equality() {
        let original = fingerprint();
        let entry = PrefixCacheMetadata::new(
            "opaque-id",
            CacheTier::SessionPersistent,
            original.clone(),
            vec![1, 2],
            4,
            0,
        );
        assert!(
            longest_compatible_prefix(std::slice::from_ref(&entry), &original, &[1, 2, 3])
                .is_some()
        );
        let mut changed = original;
        changed.device.push('r');
        changed.rope_config_sha256 = "ope".to_string();
        assert!(longest_compatible_prefix(&[entry], &changed, &[1, 2, 3]).is_none());
    }

    #[test]
    fn version_two_vectors_are_stable_for_ascii_and_utf8_bytes() {
        let base = fingerprint();
        assert_eq!(
            base.stable_id(),
            "8159bbaa27af0cfdfee64bb9c29976cb15c244e5edf5c6f2546fb64712b17a1e"
        );
        let mut unicode = base;
        unicode.build_id = "🌱\0β".to_string();
        assert_eq!(
            unicode.stable_id(),
            "d6d0877f2ec238955c0c287cb82d650400e2a19712852bd6f8389959e5923575"
        );
        assert_eq!(unicode.stable_id(), unicode.clone().stable_id());
    }

    #[test]
    fn all_prompt_form_and_token_policy_tags_are_distinct() {
        let mut identities = std::collections::BTreeSet::new();
        for form in [
            PromptForm::Chat,
            PromptForm::Completion,
            PromptForm::FillInMiddle,
        ] {
            for policy in [
                PromptTokenPolicy::ChatTemplate,
                PromptTokenPolicy::NoBosParseSpecial,
                PromptTokenPolicy::AddBosParseSpecial,
                PromptTokenPolicy::ExactTokenIds,
                PromptTokenPolicy::FillInMiddleModelTokens,
            ] {
                let mut value = fingerprint();
                value.prompt_form = form;
                value.prompt_token_policy = policy;
                assert!(identities.insert(value.stable_id()));
            }
        }
        assert_eq!(identities.len(), 15);
    }
}
