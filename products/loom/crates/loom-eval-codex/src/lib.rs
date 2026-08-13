#![forbid(unsafe_code)]

//! Optional, isolated frontier-critic adapter for the official Codex CLI.
//!
//! Both caller-pinned and exact ChatGPT-bundled executions produce cloneable,
//! immutable diagnostic receipts. The adapter checks local executable identity,
//! `ChatGPT` authentication, exact arguments and packets, tool-free complete
//! JSONL, private-workspace integrity, schema validity, and byte-exact evidence.
//! The current CLI does not attest the serving model/configuration, so
//! `observed_model` remains explicitly unavailable and this crate exports no
//! writer, evaluator, store, promotion, campaign, manuscript-edit, or sealed
//! benchmark authority.
//!
//! The former authority-shaped API is deliberately absent:
//!
//! ```compile_fail
//! use loom_eval_codex::{
//!     ApprovedFrontierCritic, ApprovedFrontierCriticReceipt,
//!     VerifiedFrontierBlindPairJudgment, VerifiedFrontierCriterionObservation,
//!     approve_chatgpt_bundled_frontier_critic,
//! };
//! ```

mod blind_pair;
mod criterion;
mod jsonl;
mod prompt_policy;
mod runner;

pub use blind_pair::*;
pub use criterion::*;
pub use jsonl::*;
pub use prompt_policy::PromptPolicyError;
pub use runner::*;

#[cfg(test)]
mod authority_boundary_tests {
    use super::*;

    fn assert_clone<T: Clone>() {}
    fn assert_serialize<T: serde::Serialize>() {}

    #[test]
    fn every_public_execution_result_is_cloneable_diagnostic_data() {
        assert_clone::<DiagnosticFrontierCriticReceipt>();
        assert_clone::<FrontierBlindPairDiagnostic>();
        assert_clone::<FrontierCriterionDiagnostic>();
        assert_serialize::<DiagnosticFrontierCriticReceipt>();
    }
}
