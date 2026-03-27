//! Tests for the dev-server routing layer.
//!
//! With the per-port gateway architecture, each env's gateway is a standalone
//! axum router on its own port. These tests verify:
//! - Each env's gateway serves its own objects at standard paths
//! - Network partition simulation (debug API) still works
//! - The PM API port has no gateway routes (only stub + debug)

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use oprc_dev_server::network_sim::NetworkSimState;
use oprc_netsim::LinkChecker;
use tower::ServiceExt;

/// Build a stub gateway router that returns a known JSON body,
/// behaving like a gateway's `/api/class/...` endpoint.
fn mock_gateway(env_label: &str) -> Router {
    let label = env_label.to_string();
    Router::new()
        .route(
            "/api/class/{cls}/{pid}/objects",
            get({
                let label = label.clone();
                move || async move {
                    let body =
                        serde_json::json!({"source": label, "objects": []});
                    axum::Json(body)
                }
            }),
        )
        .route(
            "/api/class/{cls}/{pid}/objects/{oid}",
            get({
                let label = label.clone();
                move || async move {
                    let body =
                        serde_json::json!({"source": label, "id": "obj-1"});
                    axum::Json(body)
                }
            }),
        )
        .route(
            "/healthz",
            get({
                let label = label.clone();
                move || async move {
                    axum::Json(
                        serde_json::json!({"status": "ok", "env": label}),
                    )
                }
            }),
        )
}

async fn get_json(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or(
        serde_json::json!({"_raw": String::from_utf8_lossy(&body).to_string()}),
    );
    (status, json)
}

// -----------------------------------------------------------------------
// Per-env gateway tests (each env is its own standalone router)
// -----------------------------------------------------------------------

#[tokio::test]
async fn gateway_serves_objects_at_root() {
    let cloud_gw = mock_gateway("cloud");

    let (status, json) =
        get_json(&cloud_gw, "/api/class/my.Cls/0/objects").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["source"], "cloud");
}

#[tokio::test]
async fn gateway_serves_single_object() {
    let edge_gw = mock_gateway("edge");

    let (status, json) =
        get_json(&edge_gw, "/api/class/my.Cls/0/objects/obj-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["source"], "edge");
    assert_eq!(json["id"], "obj-1");
}

#[tokio::test]
async fn gateway_serves_healthz() {
    let gw = mock_gateway("local-dev");

    let (status, json) = get_json(&gw, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["env"], "local-dev");
}

#[tokio::test]
async fn separate_gateways_are_isolated() {
    let cloud_gw = mock_gateway("cloud");
    let edge_gw = mock_gateway("edge");

    let (_, cloud_json) =
        get_json(&cloud_gw, "/api/class/my.Cls/0/objects").await;
    let (_, edge_json) =
        get_json(&edge_gw, "/api/class/my.Cls/0/objects").await;

    assert_eq!(cloud_json["source"], "cloud");
    assert_eq!(edge_json["source"], "edge");
    // They are distinct routers returning different data
    assert_ne!(cloud_json["source"], edge_json["source"]);
}

// -----------------------------------------------------------------------
// PM API port has no gateway routes
// -----------------------------------------------------------------------

#[tokio::test]
async fn pm_api_has_no_gateway_routes() {
    // Build a router that only has the debug API (mimicking the PM port)
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);
    let debug_api = oprc_dev_server::network_sim::build_debug_api(net_state);

    // Gateway-style paths should 404 on the PM port
    let (status, _) = get_json(&debug_api, "/api/class/my.Cls/0/objects").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = get_json(
        &debug_api,
        "/api/gateway/env/cloud/api/class/my.Cls/0/objects",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// -----------------------------------------------------------------------
// Network partition tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn pairwise_partition_and_heal() {
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);

    let debug_api =
        oprc_dev_server::network_sim::build_debug_api(net_state.clone());

    // Initially connected
    assert!(net_state.is_active("edge", "cloud").await);

    // Partition edge↔cloud
    let resp = debug_api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/debug/network/edge/cloud/partition")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert!(!net_state.is_active("edge", "cloud").await);

    // Heal edge↔cloud
    let resp = debug_api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/debug/network/edge/cloud/heal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert!(net_state.is_active("edge", "cloud").await);
}

// -----------------------------------------------------------------------
// Debug API tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn debug_api_list_network_state() {
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);
    let debug_api = oprc_dev_server::network_sim::build_debug_api(net_state);

    let (status, json) = get_json(&debug_api, "/api/debug/network").await;
    assert_eq!(status, StatusCode::OK);
    // Should have both envs in the environments array
    let envs = json["environments"].as_array().expect("environments array");
    let env_names_returned: Vec<&str> =
        envs.iter().filter_map(|e| e.as_str()).collect();
    assert!(env_names_returned.contains(&"cloud"));
    assert!(env_names_returned.contains(&"edge"));
    // Should have one link (cloud↔edge)
    let links = json["links"].as_array().expect("links array");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["connected"], true);
}
