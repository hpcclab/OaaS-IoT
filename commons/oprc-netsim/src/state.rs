//! Pairwise network simulation state with Axum REST debug API.
//!
//! Used by `oprc-dev-server` for local multi-environment simulation.
//! The PM uses a different approach (ZRPC to routers).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::types::{LinkChecker, LinkState};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Ordered pair key for the connectivity matrix.
fn link_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// Shared network simulation registry with pairwise connectivity.
#[derive(Debug, Clone)]
pub struct NetworkSimState {
    env_names: Arc<Vec<String>>,
    links: Arc<RwLock<HashMap<(String, String), LinkState>>>,
}

impl NetworkSimState {
    pub fn new(env_names: &[String]) -> Self {
        let mut links = HashMap::new();
        for (i, a) in env_names.iter().enumerate() {
            for b in env_names.iter().skip(i + 1) {
                let key = link_key(a, b);
                links.insert(
                    key,
                    LinkState {
                        env_a: a.clone(),
                        env_b: b.clone(),
                        connected: true,
                        latency_ms: 0,
                    },
                );
            }
        }
        Self {
            env_names: Arc::new(env_names.to_vec()),
            links: Arc::new(RwLock::new(links)),
        }
    }

    /// Return all registered environment names.
    pub fn env_names(&self) -> &[String] {
        &self.env_names
    }

    /// Direct mutable access to the links map (for tests).
    pub async fn links_for_test(
        &self,
    ) -> tokio::sync::RwLockWriteGuard<'_, HashMap<(String, String), LinkState>>
    {
        self.links.write().await
    }
}

#[async_trait::async_trait]
impl LinkChecker for NetworkSimState {
    async fn is_active(&self, env_a: &str, env_b: &str) -> bool {
        if env_a == env_b {
            return true;
        }
        let key = link_key(env_a, env_b);
        let links = self.links.read().await;
        links.get(&key).is_some_and(|s| s.connected)
    }

    async fn latency_ms(&self, env_a: &str, env_b: &str) -> u64 {
        if env_a == env_b {
            return 0;
        }
        let key = link_key(env_a, env_b);
        let links = self.links.read().await;
        links
            .get(&key)
            .filter(|s| s.connected)
            .map_or(0, |s| s.latency_ms)
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct NetworkOverview {
    environments: Vec<String>,
    links: Vec<LinkState>,
}

#[derive(Deserialize)]
struct PartitionRequest {
    #[serde(default)]
    latency_ms: Option<u64>,
}

#[derive(Serialize)]
struct LinkActionResponse {
    env_a: String,
    env_b: String,
    action: String,
    connected: bool,
    latency_ms: u64,
}

#[derive(Serialize)]
struct EnvActionResponse {
    env: String,
    action: String,
    affected_links: Vec<String>,
}

#[derive(Serialize)]
struct BulkActionResponse {
    action: String,
    environments: Vec<String>,
}

// ---------------------------------------------------------------------------
// Router builders — both path prefixes share the same handlers
// ---------------------------------------------------------------------------

/// Build REST API routes under `/api/debug/network` (dev-server compat).
pub fn build_debug_api(state: NetworkSimState) -> Router {
    build_netsim_routes("/api/debug/network", state)
}

/// Build REST API routes under `/api/v1/network-sim` (PM compat).
pub fn build_v1_api(state: NetworkSimState) -> Router {
    build_netsim_routes("/api/v1/network-sim", state)
}

fn build_netsim_routes(prefix: &str, state: NetworkSimState) -> Router {
    Router::new()
        .route(prefix, get(list_network_state))
        .route(&format!("{prefix}/partition-all"), post(partition_all))
        .route(&format!("{prefix}/heal-all"), post(heal_all))
        // Pairwise routes
        .route(
            &format!("{prefix}/{{env_a}}/{{env_b}}/partition"),
            post(partition_link),
        )
        .route(
            &format!("{prefix}/{{env_a}}/{{env_b}}/heal"),
            post(heal_link),
        )
        .route(
            &format!("{prefix}/{{env_a}}/{{env_b}}/latency"),
            post(set_link_latency),
        )
        // Per-env shortcuts
        .route(&format!("{prefix}/{{env}}/partition"), post(partition_env))
        .route(&format!("{prefix}/{{env}}/heal"), post(heal_env))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_network_state(
    State(state): State<NetworkSimState>,
) -> Json<NetworkOverview> {
    let links_map = state.links.read().await;
    let mut links: Vec<LinkState> = links_map.values().cloned().collect();
    links.sort_by(|a, b| (&a.env_a, &a.env_b).cmp(&(&b.env_a, &b.env_b)));
    Json(NetworkOverview {
        environments: state.env_names.as_ref().clone(),
        links,
    })
}

async fn partition_link(
    State(state): State<NetworkSimState>,
    Path((env_a, env_b)): Path<(String, String)>,
    body: Option<Json<PartitionRequest>>,
) -> Result<Json<LinkActionResponse>, StatusCode> {
    let key = link_key(&env_a, &env_b);
    let mut links = state.links.write().await;
    let entry = links.get_mut(&key).ok_or(StatusCode::NOT_FOUND)?;
    entry.connected = false;
    if let Some(Json(req)) = body {
        entry.latency_ms = req.latency_ms.unwrap_or(0);
    }
    Ok(Json(LinkActionResponse {
        env_a,
        env_b,
        action: "partitioned".into(),
        connected: false,
        latency_ms: entry.latency_ms,
    }))
}

async fn heal_link(
    State(state): State<NetworkSimState>,
    Path((env_a, env_b)): Path<(String, String)>,
) -> Result<Json<LinkActionResponse>, StatusCode> {
    let key = link_key(&env_a, &env_b);
    let mut links = state.links.write().await;
    let entry = links.get_mut(&key).ok_or(StatusCode::NOT_FOUND)?;
    entry.connected = true;
    entry.latency_ms = 0;
    Ok(Json(LinkActionResponse {
        env_a,
        env_b,
        action: "healed".into(),
        connected: true,
        latency_ms: 0,
    }))
}

#[derive(Deserialize)]
struct LatencyRequest {
    latency_ms: u64,
}

async fn set_link_latency(
    State(state): State<NetworkSimState>,
    Path((env_a, env_b)): Path<(String, String)>,
    Json(req): Json<LatencyRequest>,
) -> Result<Json<LinkActionResponse>, StatusCode> {
    let key = link_key(&env_a, &env_b);
    let mut links = state.links.write().await;
    let entry = links.get_mut(&key).ok_or(StatusCode::NOT_FOUND)?;
    entry.latency_ms = req.latency_ms;
    Ok(Json(LinkActionResponse {
        env_a,
        env_b,
        action: "latency-updated".into(),
        connected: entry.connected,
        latency_ms: entry.latency_ms,
    }))
}

async fn partition_env(
    State(state): State<NetworkSimState>,
    Path(env): Path<String>,
) -> Result<Json<EnvActionResponse>, StatusCode> {
    if !state.env_names.contains(&env) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut links = state.links.write().await;
    let mut affected = BTreeSet::new();
    for link in links.values_mut() {
        if link.env_a == env || link.env_b == env {
            link.connected = false;
            let other = if link.env_a == env {
                &link.env_b
            } else {
                &link.env_a
            };
            affected.insert(other.clone());
        }
    }
    Ok(Json(EnvActionResponse {
        env,
        action: "partitioned-from-all".into(),
        affected_links: affected.into_iter().collect(),
    }))
}

async fn heal_env(
    State(state): State<NetworkSimState>,
    Path(env): Path<String>,
) -> Result<Json<EnvActionResponse>, StatusCode> {
    if !state.env_names.contains(&env) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut links = state.links.write().await;
    let mut affected = BTreeSet::new();
    for link in links.values_mut() {
        if link.env_a == env || link.env_b == env {
            link.connected = true;
            link.latency_ms = 0;
            let other = if link.env_a == env {
                &link.env_b
            } else {
                &link.env_a
            };
            affected.insert(other.clone());
        }
    }
    Ok(Json(EnvActionResponse {
        env,
        action: "healed-to-all".into(),
        affected_links: affected.into_iter().collect(),
    }))
}

async fn partition_all(
    State(state): State<NetworkSimState>,
) -> Json<BulkActionResponse> {
    let mut links = state.links.write().await;
    for link in links.values_mut() {
        link.connected = false;
    }
    Json(BulkActionResponse {
        action: "partitioned-all".into(),
        environments: state.env_names.as_ref().clone(),
    })
}

async fn heal_all(
    State(state): State<NetworkSimState>,
) -> Json<BulkActionResponse> {
    let mut links = state.links.write().await;
    for link in links.values_mut() {
        link.connected = true;
        link.latency_ms = 0;
    }
    Json(BulkActionResponse {
        action: "healed-all".into(),
        environments: state.env_names.as_ref().clone(),
    })
}
