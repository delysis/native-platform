use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod controlled_generation;
mod exact_token_budget;
mod first_word_choices;
pub mod media_identity;
mod sampling_fingerprint;

pub use controlled_generation::*;
pub use exact_token_budget::*;
pub use first_word_choices::*;
pub use sampling_fingerprint::{SAMPLING_CONFIG_FINGERPRINT_DOMAIN, SamplingConfigFingerprint};

pub const MAX_PARALLEL_SEQUENCES: u32 = 4;
pub const MAX_EMBEDDING_BATCH_INPUTS: usize = 64;
pub const MAX_EMBEDDING_INPUT_TOKENS: usize = 262_144;
pub const MAX_EMBEDDING_BATCH_TOKENS: usize = 1_048_576;
