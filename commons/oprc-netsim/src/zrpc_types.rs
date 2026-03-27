//! ZRPC protocol types for router netsim control.
//!
//! The PM sends [`NetsimCommand`]s to each router's ZRPC queryable
//! (key: `oprc/netsim/{env_name}`). The router responds with [`NetsimResponse`].

use serde::{Deserialize, Serialize};

use crate::types::LinkState;

/// Key expression prefix for netsim ZRPC queryables.
pub const NETSIM_KEY_PREFIX: &str = "oprc/netsim";

/// Command sent from PM to a router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetsimCommand {
    /// Partition this router from the named peer.
    Partition { peer: String },
    /// Heal the link to the named peer.
    Heal { peer: String },
    /// Set latency (in ms) on the link to the named peer.
    SetLatency { peer: String, latency_ms: u64 },
    /// Get the current link status for all peers on this router.
    GetStatus,
}

/// Info about one peer link on a router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerLinkInfo {
    pub peer_env: String,
    pub connected: bool,
    pub latency_ms: u64,
}

/// Response from the router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetsimResponse {
    Ok {
        env_name: String,
        links: Vec<PeerLinkInfo>,
    },
    Error {
        message: String,
    },
}

impl NetsimResponse {
    /// Convert all peer link infos into full `LinkState`s.
    pub fn into_link_states(self) -> Vec<LinkState> {
        match self {
            NetsimResponse::Ok { env_name, links } => links
                .into_iter()
                .map(|p| LinkState {
                    env_a: env_name.clone(),
                    env_b: p.peer_env,
                    connected: p.connected,
                    latency_ms: p.latency_ms,
                })
                .collect(),
            NetsimResponse::Error { .. } => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// ZRPC type config (postcard serialization)
// ---------------------------------------------------------------------------

pub type NetsimZrpcType = oprc_zrpc::postcard::PostcardZrpcType<
    NetsimCommand,
    NetsimResponse,
    String,
>;
