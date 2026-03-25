//! Network partition simulation for multi-environment dev server.
//!
//! Simulates **inter-environment link breakage** — not individual env outages.
//! When two environments are partitioned, their Zenoh data sync (MST
//! replication) stops, but each environment continues serving local requests
//! normally.
//!
//! The state is a **pairwise connectivity matrix**: `(env_a, env_b) → LinkState`.
//! Partition control supports both pairwise and per-env (isolate from all) modes.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

/// Ordered pair key for the connectivity matrix.
/// Always stored as `(min, max)` so `(a,b)` and `(b,a)` map to the same entry.
fn link_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// State of a single inter-environment link.
#[derive(Debug, Clone, Serialize)]
pub struct LinkState {
    pub env_a: String,
    pub env_b: String,
    pub connected: bool,
    /// Latency injection in milliseconds (0 = no delay). Applied only when
    /// `connected` is true.
    pub latency_ms: u64,
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
        // Create a link entry for every unordered pair
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

    /// Check whether the link between two environments is active.
    /// Returns `true` if connected (or if env_a == env_b).
    pub async fn is_link_active(&self, env_a: &str, env_b: &str) -> bool {
        if env_a == env_b {
            return true;
        }
        let key = link_key(env_a, env_b);
        let links = self.links.read().await;
        links.get(&key).is_some_and(|s| s.connected)
    }

    /// Get the configured latency for a link (0 if disconnected or unknown).
    pub async fn link_latency_ms(&self, env_a: &str, env_b: &str) -> u64 {
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

// ---------------------------------------------------------------------------
// Request / response types for debug API
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct NetworkOverview {
    environments: Vec<String>,
    links: Vec<LinkState>,
}

#[derive(Deserialize)]
struct PartitionRequest {
    /// Optional latency to inject (milliseconds).
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
// Debug router
// ---------------------------------------------------------------------------

/// Build debug REST API routes for network partition simulation.
///
/// Pairwise link control:
/// - `POST /api/debug/network/{env_a}/{env_b}/partition` — break link
/// - `POST /api/debug/network/{env_a}/{env_b}/heal`      — restore link
/// - `POST /api/debug/network/{env_a}/{env_b}/latency`   — set latency on connected link
///
/// Per-env shortcuts (isolate/restore from ALL others):
/// - `POST /api/debug/network/{env}/partition` — partition env from all
/// - `POST /api/debug/network/{env}/heal`      — heal env to all
///
/// Bulk:
/// - `GET  /api/debug/network`              — overview
/// - `POST /api/debug/network/partition-all` — partition all links
/// - `POST /api/debug/network/heal-all`      — heal all links
pub fn build_debug_api(state: NetworkSimState) -> Router {
    Router::new()
        .route("/api/debug/network", get(list_network_state))
        .route("/api/debug/network/partition-all", post(partition_all))
        .route("/api/debug/network/heal-all", post(heal_all))
        // Pairwise routes
        .route(
            "/api/debug/network/{env_a}/{env_b}/partition",
            post(partition_link),
        )
        .route("/api/debug/network/{env_a}/{env_b}/heal", post(heal_link))
        .route(
            "/api/debug/network/{env_a}/{env_b}/latency",
            post(set_link_latency),
        )
        // Per-env shortcuts
        .route("/api/debug/network/{env}/partition", post(partition_env))
        .route("/api/debug/network/{env}/heal", post(heal_env))
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

/// Partition a single link between two environments.
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

/// Heal a single link between two environments.
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

/// Set latency on a connected link without changing its connectivity.
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

/// Partition a single env from ALL other environments.
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

/// Heal a single env back to ALL other environments.
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
