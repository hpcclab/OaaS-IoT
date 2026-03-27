//! Network simulation manager — gRPC client to CRM netsim services.
//!
//! The PM sends netsim commands to each CRM via gRPC. Each CRM forwards
//! the command to its co-deployed router via ZRPC. This avoids the need
//! for PM to maintain a direct Zenoh session to each router.

use std::sync::Arc;

use oprc_grpc::proto::netsim::{NetsimAction, NetsimControlRequest};
use oprc_netsim::types::LinkState;
use tracing::error;

use crate::crm::manager::CrmManager;

/// Manages network simulation via gRPC to CRM(s).
pub struct NetsimManager {
    crm_manager: Arc<CrmManager>,
}

impl NetsimManager {
    pub fn new(crm_manager: Arc<CrmManager>) -> Self {
        Self { crm_manager }
    }

    /// Helper: send a netsim command to a specific cluster's CRM.
    async fn send_cmd(
        &self,
        cluster: &str,
        action: NetsimAction,
        peer: &str,
        latency_ms: u64,
    ) -> anyhow::Result<oprc_grpc::proto::netsim::NetsimControlResponse> {
        let client = self.crm_manager.get_client(cluster).await?;
        let resp = client
            .netsim_control(NetsimControlRequest {
                action: action.into(),
                peer: peer.to_string(),
                latency_ms,
            })
            .await?;
        Ok(resp)
    }

    // ---------------------------------------------------------------
    // Pairwise operations
    // ---------------------------------------------------------------

    /// Partition link between env_a and env_b (sends to BOTH CRMs).
    pub async fn partition_link(
        &self,
        env_a: &str,
        env_b: &str,
    ) -> anyhow::Result<()> {
        // Tell CRM-A to partition from B
        if let Err(e) = self
            .send_cmd(env_a, NetsimAction::Partition, env_b, 0)
            .await
        {
            error!(env = env_a, "netsim partition call failed: {e:?}");
        }
        // Tell CRM-B to partition from A
        if let Err(e) = self
            .send_cmd(env_b, NetsimAction::Partition, env_a, 0)
            .await
        {
            error!(env = env_b, "netsim partition call failed: {e:?}");
        }
        Ok(())
    }

    /// Heal link between env_a and env_b (sends to BOTH CRMs).
    pub async fn heal_link(
        &self,
        env_a: &str,
        env_b: &str,
    ) -> anyhow::Result<()> {
        if let Err(e) = self.send_cmd(env_a, NetsimAction::Heal, env_b, 0).await
        {
            error!(env = env_a, "netsim heal call failed: {e:?}");
        }
        if let Err(e) = self.send_cmd(env_b, NetsimAction::Heal, env_a, 0).await
        {
            error!(env = env_b, "netsim heal call failed: {e:?}");
        }
        Ok(())
    }

    /// Set latency on the link between env_a and env_b (both directions).
    pub async fn set_link_latency(
        &self,
        env_a: &str,
        env_b: &str,
        latency_ms: u64,
    ) -> anyhow::Result<()> {
        if let Err(e) = self
            .send_cmd(env_a, NetsimAction::SetLatency, env_b, latency_ms)
            .await
        {
            error!(env = env_a, "netsim set_latency failed: {e:?}");
        }
        if let Err(e) = self
            .send_cmd(env_b, NetsimAction::SetLatency, env_a, latency_ms)
            .await
        {
            error!(env = env_b, "netsim set_latency failed: {e:?}");
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // Per-env operations
    // ---------------------------------------------------------------

    /// Partition env from ALL other envs.
    pub async fn partition_env(
        &self,
        env: &str,
    ) -> anyhow::Result<Vec<String>> {
        let clusters = self.crm_manager.list_clusters().await;
        let others: Vec<String> =
            clusters.into_iter().filter(|k| k.as_str() != env).collect();
        for other in &others {
            self.partition_link(env, other).await?;
        }
        Ok(others)
    }

    /// Heal env to ALL other envs.
    pub async fn heal_env(&self, env: &str) -> anyhow::Result<Vec<String>> {
        let clusters = self.crm_manager.list_clusters().await;
        let others: Vec<String> =
            clusters.into_iter().filter(|k| k.as_str() != env).collect();
        for other in &others {
            self.heal_link(env, other).await?;
        }
        Ok(others)
    }

    // ---------------------------------------------------------------
    // Bulk operations
    // ---------------------------------------------------------------

    /// Partition ALL links.
    pub async fn partition_all(&self) -> anyhow::Result<Vec<String>> {
        let envs = self.crm_manager.list_clusters().await;
        for i in 0..envs.len() {
            for j in (i + 1)..envs.len() {
                self.partition_link(&envs[i], &envs[j]).await?;
            }
        }
        Ok(envs)
    }

    /// Heal ALL links.
    pub async fn heal_all(&self) -> anyhow::Result<Vec<String>> {
        let envs = self.crm_manager.list_clusters().await;
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

    /// Get network state from all CRMs/routers.
    pub async fn get_network_state(
        &self,
    ) -> anyhow::Result<(Vec<String>, Vec<LinkState>)> {
        let envs = self.crm_manager.list_clusters().await;

        let mut all_links: Vec<LinkState> = Vec::new();
        for env in &envs {
            match self.send_cmd(env, NetsimAction::GetStatus, "", 0).await {
                Ok(resp) => {
                    for link in resp.links {
                        all_links.push(LinkState {
                            env_a: resp.env_name.clone(),
                            env_b: link.peer_env,
                            connected: link.connected,
                            latency_ms: link.latency_ms,
                        });
                    }
                }
                Err(e) => {
                    error!(env = %env, "netsim GetStatus failed: {e:?}");
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
        all_links
            .sort_by(|a, b| (&a.env_a, &a.env_b).cmp(&(&b.env_a, &b.env_b)));

        Ok((envs, all_links))
    }

    /// List known environment names (= cluster names from CRM manager).
    pub async fn env_names(&self) -> Vec<String> {
        self.crm_manager.list_clusters().await
    }
}
