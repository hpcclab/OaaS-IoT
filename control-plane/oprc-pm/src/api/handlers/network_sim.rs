//! REST handlers for network simulation (PM → routers via ZRPC).
//!
//! These handlers match the same JSON response format as the dev-server's
//! `/api/debug/network/*` and `/api/v1/network-sim/*` endpoints.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{get, post},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::services::netsim::NetsimManager;

// ---------------------------------------------------------------------------
// Response types (matching dev-server format)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct NetworkOverview {
    environments: Vec<String>,
    links: Vec<oprc_netsim::types::LinkState>,
}

#[derive(Serialize)]
struct LinkActionResponse {
    env_a: String,
    env_b: String,
    action: String,
}

#[derive(Deserialize)]
struct LatencyRequest {
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
// Router builder
// ---------------------------------------------------------------------------

/// Build netsim REST routes under `/api/v1/network-sim`.
pub fn build_netsim_api(manager: Arc<NetsimManager>) -> Router {
    Router::new()
        .route("/api/v1/network-sim", get(list_state))
        .route("/api/v1/network-sim/partition-all", post(partition_all))
        .route("/api/v1/network-sim/heal-all", post(heal_all))
        .route(
            "/api/v1/network-sim/{env_a}/{env_b}/partition",
            post(partition_link),
        )
        .route(
            "/api/v1/network-sim/{env_a}/{env_b}/heal",
            post(heal_link),
        )
        .route(
            "/api/v1/network-sim/{env_a}/{env_b}/latency",
            post(set_link_latency),
        )
        .route(
            "/api/v1/network-sim/{env}/partition",
            post(partition_env),
        )
        .route("/api/v1/network-sim/{env}/heal", post(heal_env))
        .with_state(manager)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_state(
    State(mgr): State<Arc<NetsimManager>>,
) -> Result<Json<NetworkOverview>, StatusCode> {
    let (envs, links) = mgr
        .get_network_state()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(NetworkOverview {
        environments: envs,
        links,
    }))
}

async fn partition_link(
    State(mgr): State<Arc<NetsimManager>>,
    Path((env_a, env_b)): Path<(String, String)>,
) -> Result<Json<LinkActionResponse>, StatusCode> {
    mgr.partition_link(&env_a, &env_b)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(LinkActionResponse {
        env_a,
        env_b,
        action: "partitioned".into(),
    }))
}

async fn heal_link(
    State(mgr): State<Arc<NetsimManager>>,
    Path((env_a, env_b)): Path<(String, String)>,
) -> Result<Json<LinkActionResponse>, StatusCode> {
    mgr.heal_link(&env_a, &env_b)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(LinkActionResponse {
        env_a,
        env_b,
        action: "healed".into(),
    }))
}

async fn set_link_latency(
    State(mgr): State<Arc<NetsimManager>>,
    Path((env_a, env_b)): Path<(String, String)>,
    Json(req): Json<LatencyRequest>,
) -> Result<Json<LinkActionResponse>, StatusCode> {
    mgr.set_link_latency(&env_a, &env_b, req.latency_ms)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(LinkActionResponse {
        env_a,
        env_b,
        action: "latency-updated".into(),
    }))
}

async fn partition_env(
    State(mgr): State<Arc<NetsimManager>>,
    Path(env): Path<String>,
) -> Result<Json<EnvActionResponse>, StatusCode> {
    let affected = mgr
        .partition_env(&env)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(EnvActionResponse {
        env,
        action: "partitioned-from-all".into(),
        affected_links: affected,
    }))
}

async fn heal_env(
    State(mgr): State<Arc<NetsimManager>>,
    Path(env): Path<String>,
) -> Result<Json<EnvActionResponse>, StatusCode> {
    let affected = mgr
        .heal_env(&env)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(EnvActionResponse {
        env,
        action: "healed-to-all".into(),
        affected_links: affected,
    }))
}

async fn partition_all(
    State(mgr): State<Arc<NetsimManager>>,
) -> Result<Json<BulkActionResponse>, StatusCode> {
    let envs = mgr
        .partition_all()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(BulkActionResponse {
        action: "partitioned-all".into(),
        environments: envs,
    }))
}

async fn heal_all(
    State(mgr): State<Arc<NetsimManager>>,
) -> Result<Json<BulkActionResponse>, StatusCode> {
    let envs = mgr
        .heal_all()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(BulkActionResponse {
        action: "healed-all".into(),
        environments: envs,
    }))
}
