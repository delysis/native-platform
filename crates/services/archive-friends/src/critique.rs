use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CritiqueReceipt {
    pub model_id: String,
    pub basis_sha256: String,
    pub accepted: bool,
    pub reason: String,
    pub notes: Vec<String>,
    pub before_ids: Vec<String>,
    pub after_ids: Vec<String>,
}
