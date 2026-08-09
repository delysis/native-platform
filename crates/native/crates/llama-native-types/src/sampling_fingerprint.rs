use crate::{SamplerKind, SamplingConfig};
use sha2::{Digest, Sha256};
use std::fmt;

/// Domain and format version for [`SamplingConfigFingerprint`].
///
/// Changing the canonical encoding requires a new domain. Digests produced
/// under different domains are deliberately incomparable.
pub const SAMPLING_CONFIG_FINGERPRINT_DOMAIN: &str = "llama-native.sampling-config-fingerprint.v1";

/// Exact-bit, canonical identity of a [`SamplingConfig`].
///
/// This type intentionally exposes no constructor and has no serde contract.
/// It can only be derived from the in-memory sampling configuration whose
/// fields it commits to.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SamplingConfigFingerprint([u8; 32]);

impl SamplingConfigFingerprint {
    /// Borrow the canonical 32-byte SHA-256 value.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Return the canonical lowercase SHA-256 representation.
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        let mut output = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in self.0 {
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        output
    }
}

pub(crate) fn sampling_float_bits(sampling: &SamplingConfig) -> [u32; 13] {
    // This is intentionally a second exhaustive projection: the controlled
    // receipt named `exact_float_bits_sha256` commits only to floats. Adding a
    // SamplingConfig field must still force an explicit decision about whether
    // that field belongs in this projection.
    let SamplingConfig {
        seed: _,
        temperature,
        dynamic_temperature_range,
        dynamic_temperature_exponent,
        top_k: _,
        top_p,
        min_p,
        typical_p,
        xtc_probability,
        xtc_threshold,
        repeat_last_n: _,
        repeat_penalty,
        frequency_penalty,
        presence_penalty,
        dry_multiplier,
        dry_base,
        dry_allowed_length: _,
        dry_penalty_last_n: _,
        sampler_order: _,
        max_tokens: _,
        stop: _,
    } = sampling;

    [
        temperature.to_bits(),
        dynamic_temperature_range.to_bits(),
        dynamic_temperature_exponent.to_bits(),
        top_p.to_bits(),
        min_p.to_bits(),
        typical_p.to_bits(),
        xtc_probability.to_bits(),
        xtc_threshold.to_bits(),
        repeat_penalty.to_bits(),
        frequency_penalty.to_bits(),
        presence_penalty.to_bits(),
        dry_multiplier.to_bits(),
        dry_base.to_bits(),
    ]
}

impl fmt::Debug for SamplingConfigFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SamplingConfigFingerprint")
            .field(&self.sha256_hex())
            .finish()
    }
}

impl fmt::Display for SamplingConfigFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.sha256_hex())
    }
}

impl SamplingConfig {
    /// Compute the versioned canonical exact-bit fingerprint of this recipe.
    ///
    /// Floating-point values are committed by IEEE bit pattern, so `0.0` and
    /// `-0.0` (and distinct NaN payloads) remain distinguishable. Vector order
    /// and every UTF-8 stop byte are significant.
    #[must_use]
    pub fn fingerprint(&self) -> SamplingConfigFingerprint {
        SamplingConfigFingerprint(canonical_digest(self))
    }
}

struct CanonicalEncoder(Sha256);

impl CanonicalEncoder {
    fn new() -> Self {
        let mut encoder = Self(Sha256::new());
        encoder.framed_bytes(SAMPLING_CONFIG_FINGERPRINT_DOMAIN.as_bytes());
        encoder.u32(1);
        encoder
    }

    fn field(&mut self, name: &str) {
        self.framed_bytes(name.as_bytes());
    }

    fn framed_bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.update(value);
    }

    fn u8(&mut self, value: u8) {
        self.0.update([value]);
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.0.update(value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_le_bytes());
    }

    fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

fn canonical_digest(sampling: &SamplingConfig) -> [u8; 32] {
    // Deliberately omit `..`: adding a SamplingConfig field must break this
    // build until the v1 contract is consciously revised or superseded.
    let SamplingConfig {
        seed,
        temperature,
        dynamic_temperature_range,
        dynamic_temperature_exponent,
        top_k,
        top_p,
        min_p,
        typical_p,
        xtc_probability,
        xtc_threshold,
        repeat_last_n,
        repeat_penalty,
        frequency_penalty,
        presence_penalty,
        dry_multiplier,
        dry_base,
        dry_allowed_length,
        dry_penalty_last_n,
        sampler_order,
        max_tokens,
        stop,
    } = sampling;

    let mut encoder = CanonicalEncoder::new();
    encoder.u32(21);

    encoder.field("seed");
    encoder.u32(*seed);
    encoder.field("temperature");
    encoder.f32(*temperature);
    encoder.field("dynamic_temperature_range");
    encoder.f32(*dynamic_temperature_range);
    encoder.field("dynamic_temperature_exponent");
    encoder.f32(*dynamic_temperature_exponent);
    encoder.field("top_k");
    encoder.i32(*top_k);
    encoder.field("top_p");
    encoder.f32(*top_p);
    encoder.field("min_p");
    encoder.f32(*min_p);
    encoder.field("typical_p");
    encoder.f32(*typical_p);
    encoder.field("xtc_probability");
    encoder.f32(*xtc_probability);
    encoder.field("xtc_threshold");
    encoder.f32(*xtc_threshold);
    encoder.field("repeat_last_n");
    encoder.i32(*repeat_last_n);
    encoder.field("repeat_penalty");
    encoder.f32(*repeat_penalty);
    encoder.field("frequency_penalty");
    encoder.f32(*frequency_penalty);
    encoder.field("presence_penalty");
    encoder.f32(*presence_penalty);
    encoder.field("dry_multiplier");
    encoder.f32(*dry_multiplier);
    encoder.field("dry_base");
    encoder.f32(*dry_base);
    encoder.field("dry_allowed_length");
    encoder.i32(*dry_allowed_length);
    encoder.field("dry_penalty_last_n");
    encoder.i32(*dry_penalty_last_n);
    encoder.field("sampler_order");
    encoder.u64(sampler_order.len() as u64);
    for sampler in sampler_order {
        encoder.u8(sampler_tag(*sampler));
    }
    encoder.field("max_tokens");
    encoder.u32(*max_tokens);
    encoder.field("stop");
    encoder.u64(stop.len() as u64);
    for value in stop {
        encoder.framed_bytes(value.as_bytes());
    }

    encoder.finish()
}

const fn sampler_tag(sampler: SamplerKind) -> u8 {
    // Exhaustive matching makes new enum variants a compile-time decision.
    match sampler {
        SamplerKind::Penalties => 0,
        SamplerKind::Dry => 1,
        SamplerKind::TopK => 2,
        SamplerKind::TypicalP => 3,
        SamplerKind::TopP => 4,
        SamplerKind::MinP => 5,
        SamplerKind::Xtc => 6,
        SamplerKind::Temperature => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sampling_fingerprint_is_stable_golden() {
        assert_eq!(
            SamplingConfig::default().fingerprint().sha256_hex(),
            "bd9646fcbcfdd85b3f3d1f8774ee871a19d6238a193a1caea20ff6d9434025fa"
        );
    }

    #[test]
    fn fingerprint_detects_field_and_utf8_stop_tampering() {
        let baseline = SamplingConfig::default();
        let mut changed_seed = baseline.clone();
        changed_seed.seed ^= 1;
        let mut changed_stop = baseline.clone();
        changed_stop.stop = vec!["until é".to_owned()];

        assert_ne!(baseline.fingerprint(), changed_seed.fingerprint());
        assert_ne!(baseline.fingerprint(), changed_stop.fingerprint());
    }

    #[test]
    fn fingerprint_preserves_positive_and_negative_zero() {
        let positive = SamplingConfig {
            temperature: 0.0,
            ..SamplingConfig::default()
        };
        let mut negative = positive.clone();
        negative.temperature = -0.0;

        assert_ne!(positive.fingerprint(), negative.fingerprint());
    }

    #[test]
    fn fingerprint_preserves_nan_payload_bits() {
        let first = SamplingConfig {
            temperature: f32::from_bits(0x7fc0_0001),
            ..SamplingConfig::default()
        };
        let mut second = first.clone();
        second.temperature = f32::from_bits(0x7fc0_0002);

        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn fingerprint_preserves_vector_order() {
        let samplers = SamplingConfig {
            sampler_order: vec![SamplerKind::TopK, SamplerKind::TopP],
            ..SamplingConfig::default()
        };
        let mut reversed_samplers = samplers.clone();
        reversed_samplers.sampler_order.reverse();

        let stops = SamplingConfig {
            stop: vec!["first".to_owned(), "second".to_owned()],
            ..SamplingConfig::default()
        };
        let mut reversed_stops = stops.clone();
        reversed_stops.stop.reverse();

        assert_ne!(samplers.fingerprint(), reversed_samplers.fingerprint());
        assert_ne!(stops.fingerprint(), reversed_stops.fingerprint());
    }

    #[test]
    fn framed_stop_values_have_no_concatenation_ambiguity() {
        let left = SamplingConfig {
            stop: vec!["a".to_owned(), "bc".to_owned()],
            ..SamplingConfig::default()
        };
        let right = SamplingConfig {
            stop: vec!["ab".to_owned(), "c".to_owned()],
            ..SamplingConfig::default()
        };

        assert_ne!(left.fingerprint(), right.fingerprint());
    }

    #[test]
    fn sha256_accessor_is_fixed_width_lowercase_hex() {
        let digest = SamplingConfig::default().fingerprint().sha256_hex();
        assert_eq!(digest.len(), 64);
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }
}
