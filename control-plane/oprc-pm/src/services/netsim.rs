//! Network simulation manager — ZRPC client for router netsim queryables.
//!
//! The PM opens a Zenoh session and uses ZRPC to send commands to each
//! router's netsim queryable (key: `oprc/netsim/{env}`).
//!
//! Entity model: The PM knows which *environments* (routers) exist and
//! can issue pairwise partition/heal/latency commands.

use std::collections::HashMap;

use oprc_netsim::zrpc_types::{
    NETSIM_KEY_PREFIX, NetsimCommand, NetsimResponse, NetsimZrpcType,
};
use oprc_netsim::types::LinkState;
use oprc_zrpc::ZrpcClient;
use tokio::sync::RwLock;
use tracing::{error, info};

/// Manages netsim state by proxying REST calls to ZRPC on each router.
pub struct NetsimManager {
    /// ZRPC clients keyed by environment name.
    clients: RwLock<HashMap<String, ZrpcClient<NetsimZrpcType>>>,
    /// Zenoh session kept alive for the ZRPC clients.
    _session: zenoh::Session,
}

impl NetsimManager {
    /// Create a new manager, connecting to the given environment routers.
    ///
    /// `env_names` — names of environments whose routers have netsim queryables.
    pub async fn new(
        session: zenoh::Session,
        env_names: &[String],
    ) -> anyhow::Result<Self> {
        let mut clients = HashMap::new();
        for env in env_names {
            let service_id = format!("{NETSIM_KEY_PREFIX}/{env}");
            let client =
                ZrpcClient::<NetsimZrpcType>::new(service_id, session.clone())
                    .await;
            clients.insert(env.clone(), client);
        }
        info!(envs = ?env_names, "NetsimManager initialized");
        Ok(Self {
            clients: RwLock::new(clients),
            _session: session,
        })
    }

    // ---------------------------------------------------------------
    // Pairwise operations
    // ---------------------------------------------------------------

    /// Partition link between env_a and env_b (sends to BOTH routers).
    pub async fn partition_link(
        &self,
        env_a: &str,
        env_b: &str,
    ) -> anyhow::Result<Vec<NetsimResponse>> {
        let clients = self.clients.read().await;
        let mut responses = Vec::new();

        // Tell router A to partition from B
        if let Some(client) = clients.get(env_a) {
            match client.call(&NetsimCommand::Partition { peer: env_b.to_string() }).await {
                Ok(resp) => responses.push(resp),
                Err(e) => error!(env = env_a, "ZRPC partition call failed: {e:?}"),
            }
        }
        // Tell router B to partition from A
        if let Some(client) = clients.get(env_b) {
            match client.call(&NetsimCommand::Partition { peer: env_a.to_string() }).await {
                Ok(resp) => responses.push(resp),
                Err(e) => error!(env = env_b, "ZRPC partition call failed: {e:?}"),
            }
        }
        Ok(responses)
    }

    /// Heal link between env_a and env_b (sends to BOTH routers).
    pub async fn heal_link(
        &self,
        env_a: &str,
        env_b: &str,
    ) -> anyhow::Result<Vec<NetsimResponse>> {
        let clients = self.clients.read().await;
        let mut responses = Vec::new();

        if let Some(client) = clients.get(env_a) {
            match client.call(&NetsimCommand::Heal { peer: env_b.to_string() }).await {
                Ok(resp) => responses.push(resp),
                Err(e) => error!(env = env_a, "ZRPC heal call failed: {e:?}"),
            }
        }
        if let Some(client) = clients.get(env_b) {
            match client.call(&NetsimCommand::Heal { peer: env_a.to_string() }).await {
                Ok(resp) => responses.push(resp),
                Err(e) => error!(env = env_b, "ZRPC heal call failed: {e:?}"),
            }
        }
        Ok(responses)
    }

    /// Set latency on the link between env_a and env_b (both directions).
    pub async fn set_link_latency(
        &self,
        env_a: &str,
        env_b: &str,
        latency_ms: u64,
    ) -> anyhow::Result<Vec<NetsimResponse>> {
        let clients = self.clients.read().await;
        let mut responses = Vec::new();

        if let Some(client) = clients.get(env_a) {
            match client.call(&NetsimCommand::SetLatency {
                peer: env_b.to_string(),
                latency_ms,
            }).await {
                Ok(resp) => responses.push(resp),
                Err(e) => error!(env = env_a, "ZRPC set_latency failed: {e:?}"),
            }
        }
        if let Some(client) = clients.get(env_b) {
            match client.call(&NetsimCommand::SetLatency {
                peer: env_a.to_string(),
                latency_ms,
            }).await {
                Ok(resp) => responses.push(resp),
                Err(e) => error!(env = env_b, "ZRPC set_latency failed: {e:?}"),
            }
        }
        Ok(responses)
    }

    // ---------------------------------------------------------------
    // Per-env operations
    // ---------------------------------------------------------------

    /// Partition env from ALL other envs.
    pub async fn partition_env(&self, env: &str) -> anyhow::Result<Vec<String>> {
        let clients = self.clients.read().await;
        let others: Vec<String> = clients
            .keys()
            .filter(|k| k.as_str() != env)
            .cloned()
            .collect();
        drop(clients);

        let mut affected = Vec::new();
        for other in &others {
            self.partition_link(env, other).await?;
            affected.push(other.clone());
        }
        Ok(affected)
    }

    /// Heal env to ALL other envs.
    pub async fn heal_env(&self, env: &str) -> anyhow::Result<Vec<String>> {
        let clients = self.clients.read().await;
        let others: Vec<String> = clients
            .keys()
            .filter(|k| k.as_str() != env)
            .cloned()
            .collect();
        drop(clients);

        let mut affected = Vec::new();
        for other in &others {
            self.heal_link(env, other).await?;
            affected.push(other.clone());
        }
        Ok(affected)
    }

    // ---------------------------------------------------------------
    // Bulk operations
    // ---------------------------------------------------------------

    /// Partition ALL links.
    pub async fn partition_all(&self) -> anyhow::Result<Vec<String>> {
        let clients = self.clients.read().await;
        let envs: Vec<String> = clients.keys().cloned().collect();
        drop(clients);

        for i in 0..envs.len() {
            for j in (i + 1)..envs.len() {
                self.partition_link(&envs[i], &envs[j]).await?;
            }
        }
        Ok(envs)
    }

    /// Heal ALL links.
    pub async fn heal_all(&self) -> anyhow::Result<Vec<String>> {
        let clients = self.clients.read().await;
        let envs: Vec<String> = clients.keys().cloned().collect();
        drop(clients);

        for i in 0..envs.len() {
            for j in (i + 1)..envs.len() {
                self.heal_link(&envs[i], &envs[j]).await?;
            }
        }
        Ok(envs)
    }

    // ---------------------------------------------------------------
    // Query
    // ---------------------------------------------------------------

    /// Get network state from all routers.
    pub async fn get_network_state(&self) -> anyhow::Result<(Vec<String>, Vec<LinkState>)> {
        let clients = self.clients.read().await;
        let envs: Vec<String> = clients.keys().cloned().collect();

        let mut all_links: Vec<LinkState> = Vec::new();
        for (env, client) in clients.iter() {
            match client.call(&NetsimCommand::GetStatus).await {
                Ok(resp) => {
                    all_links.extend(resp.into_link_states());
                }
                Err(e) => {
                    error!(env = %env, "ZRPC GetStatus failed: {e:?}");
                }
            }
        }

        // Deduplicate: keep only one direction per pair (env_a < env_b).
        let mut seen = std::collections::HashSet::new();
        all_links.retain(|l| {
            let key = if l.env_a <= l.env_b {
                (l.env_a.clone(), l.env_b.clone())
            } else {
                (l.env_b.clone(), l.env_a.clone())
            };
            seen.insert(key)
        });
        all_links.sort_by(|a, b| (&a.env_a, &a.env_b).cmp(&(&b.env_a, &b.env_b)));

        Ok((envs, all_links))
    }

    /// List known environment names.
    pub async fn env_names(&self) -> Vec<String> {
        self.clients.read().await.keys().cloned().collect()
    }
}
