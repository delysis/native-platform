//! Hosted provider implementations for the canonical gateway.
//!
//! This crate deliberately has no Tauri dependency and never translates a
//! Responses or Anthropic request through the legacy Chat Completions shape.

mod hosted;

pub use fte_types::GatewayBackend;
pub use hosted::{
    HostedAuth, HostedEndpoints, HostedProtocol, HostedProviderBackend, HostedProviderConfig,
};
