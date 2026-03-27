use serde::{Deserialize, Serialize};

/// State of a single inter-environment link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkState {
    pub env_a: String,
    pub env_b: String,
    pub connected: bool,
    /// Latency injection in milliseconds (0 = no delay). Applied only when
    /// `connected` is true.
    pub latency_ms: u64,
}

/// Trait for checking the health/latency of a link between two environments.
/// Implementors: `NetworkSimState` (dev-server pairwise matrix) and
/// `RouterNetsimState` (per-peer map in the router).
#[async_trait::async_trait]
pub trait LinkChecker: Send + Sync {
    /// Returns `true` if the link between `env_a` and `env_b` is active.
    async fn is_active(&self, env_a: &str, env_b: &str) -> bool;
    /// Returns the configured latency in ms (0 if disconnected or unknown).
    async fn latency_ms(&self, env_a: &str, env_b: &str) -> u64;
}
