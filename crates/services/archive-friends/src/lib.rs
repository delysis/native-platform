#![forbid(unsafe_code)]
//! Bounded read-only archive context for native hosts. No model, network,
//! ingestion, database migration or manuscript-writing authority.
mod archive;
mod compiler;
mod config;
mod critique;
mod native_context;
mod prompting;
mod retrieval;
mod sampling;
mod terms;
pub use compiler::*;
pub use config::*;
pub use critique::*;
pub use native_context::*;
pub use prompting::*;
pub use retrieval::*;
pub use sampling::*;
use sha2::{Digest, Sha256};
#[derive(Debug, thiserror::Error)]
pub enum FriendsError {
    #[error("{0}")]
    Invalid(String),
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("file: {0}")]
    Io(#[from] std::io::Error),
    #[error("dotfile: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}
fn fingerprint(value: &impl serde::Serialize) -> Result<String, FriendsError> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn invalid(message: impl Into<String>) -> FriendsError {
    FriendsError::Invalid(message.into())
}
