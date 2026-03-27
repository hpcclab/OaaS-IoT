//! Network simulation support for the router.
//!
//! When enabled (feature `network-sim` + env `OPRC_NETSIM_ENABLED=true`),
//! the router:
//!
//! 1. Reads peer list from `OPRC_NETSIM_PEERS` (comma-separated `env=host:port`).
//! 2. Starts a [`TransportProxy`] per peer, inserting itself between the Zenoh
//!    session and the remote peer's TCP endpoint.
//! 3. Registers a ZRPC queryable so the PM can send partition/heal/latency
//!    commands.
//!
//! The router rewrites its own Zenoh connect endpoints to go through the
//! local proxies instead of directly to the remote routers.

use std::collections::HashMap;
use std::sync::Arc;

use oprc_netsim::proxy::TransportProxy;
use oprc_netsim::types::LinkChecker;
use oprc_netsim::zrpc_types::{
    NETSIM_KEY_PREFIX, NetsimCommand, NetsimResponse, NetsimZrpcType,
    PeerLinkInfo,
};
use oprc_zrpc::server::ServerConfig;
use oprc_zrpc::{ZrpcService, ZrpcServiceHander};
use tokio::sync::RwLock;
use tracing::info;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Parsed netsim peer entry: `env_name=host:port`.
#[derive(Debug, Clone)]
pub struct NetsimPeer {
    pub env_name: String,
    pub addr: String,
}

/// Parse `OPRC_NETSIM_PEERS` value.
///
/// Format: `env1=host1:port1,env2=host2:port2,...`
pub fn parse_peers(raw: &str) -> Vec<NetsimPeer> {
    raw.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            let (name, addr) = entry.split_once('=')?;
            Some(NetsimPeer {
                env_name: name.trim().to_string(),
                addr: addr.trim().to_string(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Per‑peer link state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PeerLink {
    connected: bool,
    latency_ms: u64,
}

/// Router-local netsim state keyed by peer env name.
#[derive(Debug, Clone)]
pub struct RouterNetsimState {
    env_name: String,
    links: Arc<RwLock<HashMap<String, PeerLink>>>,
}

impl RouterNetsimState {
    pub fn new(env_name: String, peers: &[NetsimPeer]) -> Self {
        let mut links = HashMap::new();
        for p in peers {
            links.insert(
                p.env_name.clone(),
                PeerLink {
                    connected: true,
                    latency_ms: 0,
                },
            );
        }
        Self {
            env_name,
            links: Arc::new(RwLock::new(links)),
        }
    }
}

#[async_trait::async_trait]
impl LinkChecker for RouterNetsimState {
    async fn is_active(&self, _env_a: &str, env_b: &str) -> bool {
        let links = self.links.read().await;
        links.get(env_b).is_some_and(|l| l.connected)
    }

    async fn latency_ms(&self, _env_a: &str, env_b: &str) -> u64 {
        let links = self.links.read().await;
        links
            .get(env_b)
            .filter(|l| l.connected)
            .map_or(0, |l| l.latency_ms)
    }
}

// ---------------------------------------------------------------------------
// ZRPC handler
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl ZrpcServiceHander<NetsimZrpcType> for RouterNetsimState {
    async fn handle(
        &self,
        req: NetsimCommand,
    ) -> Result<NetsimResponse, String> {
        let mut links = self.links.write().await;
        match req {
            NetsimCommand::Partition { peer } => {
                if let Some(link) = links.get_mut(&peer) {
                    link.connected = false;
                    info!(env = %self.env_name, peer = %peer, "Partition applied");
                }
            }
            NetsimCommand::Heal { peer } => {
                if let Some(link) = links.get_mut(&peer) {
                    link.connected = true;
                    link.latency_ms = 0;
                    info!(env = %self.env_name, peer = %peer, "Heal applied");
                }
            }
            NetsimCommand::SetLatency { peer, latency_ms } => {
                if let Some(link) = links.get_mut(&peer) {
                    link.latency_ms = latency_ms;
                    info!(env = %self.env_name, peer = %peer, latency_ms, "Latency set");
                }
            }
            NetsimCommand::GetStatus => { /* just return current state */ }
        }

        let peer_links: Vec<PeerLinkInfo> = links
            .iter()
            .map(|(name, link)| PeerLinkInfo {
                peer_env: name.clone(),
                connected: link.connected,
                latency_ms: link.latency_ms,
            })
            .collect();

        Ok(NetsimResponse::Ok {
            env_name: self.env_name.clone(),
            links: peer_links,
        })
    }
}

// ---------------------------------------------------------------------------
// Bootstrap: two phases (before and after zenoh::open)
// ---------------------------------------------------------------------------

/// Pre-session output: proxy handles + rewritten endpoints.
pub struct PreSessionSetup {
    pub env_name: String,
    pub rewritten_peers: Vec<String>,
    pub state: RouterNetsimState,
    _proxies: Vec<TransportProxy>,
}

/// Handles that must stay alive for the duration of the process.
pub struct NetsimHandles {
    pub _pre: PreSessionSetup,
    pub _zrpc_service: ZrpcService<RouterNetsimState, NetsimZrpcType>,
}

/// **Phase 1** — before `zenoh::open`.
///
/// Reads env vars, starts TCP proxies, and returns rewritten connect
/// endpoints that should replace `z_conf.peers`.
///
/// Returns `None` if `OPRC_NETSIM_ENABLED` is not `"true"`.
pub async fn pre_session_setup(
    _z_conf: &oprc_zenoh::OprcZenohConfig,
) -> anyhow::Result<Option<PreSessionSetup>> {
    let enabled = std::env::var("OPRC_NETSIM_ENABLED")
        .unwrap_or_default()
        .eq_ignore_ascii_case("true");
    if !enabled {
        return Ok(None);
    }

    let env_name = std::env::var("OPRC_NETSIM_ENV_NAME")
        .unwrap_or_else(|_| "unknown".to_string());
    let raw_peers = std::env::var("OPRC_NETSIM_PEERS").unwrap_or_default();
    let peers = parse_peers(&raw_peers);
    if peers.is_empty() {
        tracing::warn!(
            "OPRC_NETSIM_ENABLED=true but OPRC_NETSIM_PEERS is empty"
        );
        return Ok(None);
    }

    info!(env = %env_name, peers = ?peers.iter().map(|p| &p.env_name).collect::<Vec<_>>(),
          "Netsim enabled — starting proxies");

    let state = RouterNetsimState::new(env_name.clone(), &peers);
    let checker: Arc<dyn LinkChecker> = Arc::new(state.clone());

    let mut proxies = Vec::new();
    let mut rewritten = Vec::new();
    for peer in &peers {
        let proxy = TransportProxy::start(
            env_name.clone(),
            peer.env_name.clone(),
            peer.addr.clone(),
            checker.clone(),
        )
        .await?;
        rewritten.push(format!("tcp/127.0.0.1:{}", proxy.listen_port));
        proxies.push(proxy);
    }

    Ok(Some(PreSessionSetup {
        env_name,
        rewritten_peers: rewritten,
        state,
        _proxies: proxies,
    }))
}

/// **Phase 2** — after `zenoh::open`.
///
/// Starts the ZRPC queryable on the open session so the PM can control
/// this router's netsim state.
pub async fn post_session_setup(
    pre: PreSessionSetup,
    session: &zenoh::Session,
) -> anyhow::Result<NetsimHandles> {
    let service_id = format!("{NETSIM_KEY_PREFIX}/{}", pre.env_name);
    let server_config = ServerConfig {
        service_id: service_id.clone(),
        concurrency: 4,
        ..Default::default()
    };
    let mut zrpc_service =
        ZrpcService::<RouterNetsimState, NetsimZrpcType>::new(
            session.clone(),
            server_config,
            pre.state.clone(),
        );
    zrpc_service
        .start()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    info!(env = %pre.env_name, key = %service_id, "Netsim ZRPC service started");

    Ok(NetsimHandles {
        _pre: pre,
        _zrpc_service: zrpc_service,
    })
}
