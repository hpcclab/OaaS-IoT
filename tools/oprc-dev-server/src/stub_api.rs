use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::routing::{delete, get, post};
use http::StatusCode;
use serde::{Deserialize, Serialize};

use oprc_models::deployment::OClassDeployment;
use oprc_models::enums::DeploymentCondition;
use oprc_models::package::OPackage;

// ---------------------------------------------------------------------------
// Shared state for stub handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct StubState {
    packages: Arc<Vec<OPackage>>,
    deployments: Arc<Vec<OClassDeployment>>,
    /// (env_name, gateway_port) pairs.
    env_ports: Arc<Vec<(String, u16)>>,
}

// ---------------------------------------------------------------------------
// Response helpers (match real PM shapes)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusResponse {
    id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Serialize)]
struct DeleteDeploymentResponse {
    message: String,
    id: String,
    deleted_envs: Vec<String>,
}

#[derive(Serialize)]
struct ClusterInfoStub {
    name: String,
    health: ClusterHealthStub,
    /// The URL the frontend should use to reach this env's gateway.
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_url: Option<String>,
}

#[derive(Serialize)]
struct ClusterHealthStub {
    status: String,
}

#[derive(Serialize)]
struct TopologySnapshot {
    nodes: Vec<TopologyNode>,
    edges: Vec<TopologyEdge>,
}

#[derive(Serialize)]
struct TopologyNode {
    id: String,
    node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<std::collections::HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deployed_classes: Option<Vec<String>>,
}

#[derive(Serialize)]
struct TopologyEdge {
    from_id: String,
    to_id: String,
}

#[derive(Serialize)]
struct ScriptErrorResponse {
    success: bool,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct TestScriptErrorResponse {
    success: bool,
    error: Option<String>,
    logs: Vec<()>,
    duration_ms: f64,
}

#[derive(Deserialize)]
struct TopologyQuery {
    #[serde(default = "default_topo_source")]
    #[allow(dead_code)]
    source: String,
}

fn default_topo_source() -> String {
    "deployments".into()
}

// ---------------------------------------------------------------------------
// Build all stub PM API routes
// ---------------------------------------------------------------------------

/// Build stub PM API routes so the oprc-next frontend can populate its UI.
///
/// Returns real `OPackage` / `OClassDeployment` JSON (matching the PM contract)
/// plus stub endpoints for write operations and scripts (not available in dev mode).
///
/// **Primary path** (when `pkg.deployments` is non-empty): uses the deployment
/// entries from the loaded package directly, mirroring what the real PM stores
/// after `POST /api/v1/deployments`. Each entry is returned as-is with
/// `condition = Running` and `target_envs` defaulted to `["local-dev"]` when
/// empty.
///
/// **Fallback** (when `pkg.deployments` is empty): synthesises one stub
/// deployment per class (useful for packages that omit the `deployments:` section).
pub fn build_stub_api(pkg: &OPackage, env_ports: &[(String, u16)]) -> Router {
    let env_names: Vec<String> =
        env_ports.iter().map(|(n, _)| n.clone()).collect();
    let deployments: Vec<OClassDeployment> = if !pkg.deployments.is_empty() {
        // Mirror the real PM: use the actual deployment entries from the package.
        pkg.deployments
            .iter()
            .map(|dep| {
                let mut d = dep.clone();
                // Ensure package_name is populated (mirrors CLI apply behaviour).
                if d.package_name.is_empty() {
                    d.package_name = pkg.name.clone();
                }
                // Mark as running (dev mode has no actual scheduling phase).
                d.condition = DeploymentCondition::Running;
                // Default target env when none is specified.
                if d.target_envs.is_empty() {
                    d.target_envs = env_names.clone();
                }
                d
            })
            .collect()
    } else {
        // Fallback: synthesise one stub deployment per class.
        pkg.classes
            .iter()
            .map(|cls| {
                let mut dep = OClassDeployment {
                    key: format!("{}.{}", pkg.name, cls.key),
                    package_name: pkg.name.clone(),
                    class_key: cls.key.clone(),
                    condition: DeploymentCondition::Running,
                    ..Default::default()
                };
                dep.target_envs = env_names.clone();
                dep
            })
            .collect()
    };

    let state = StubState {
        packages: Arc::new(vec![pkg.clone()]),
        deployments: Arc::new(deployments),
        env_ports: Arc::new(env_ports.to_vec()),
    };

    Router::new()
        // --- Packages ---
        .route("/api/v1/packages", get(list_packages).post(create_package))
        .route(
            "/api/v1/packages/{name}",
            get(get_package).delete(delete_package),
        )
        // --- Deployments ---
        .route(
            "/api/v1/deployments",
            get(list_deployments).post(create_deployment),
        )
        .route("/api/v1/deployments/{key}", delete(delete_deployment))
        // --- Environments ---
        .route("/api/v1/envs", get(list_envs))
        // --- Topology ---
        .route("/api/v1/topology", get(get_topology))
        // --- Artifacts ---
        .route("/api/v1/artifacts", get(list_artifacts))
        // --- Script stubs (not available in dev mode) ---
        .route("/api/v1/scripts/compile", post(script_not_available))
        .route("/api/v1/scripts/build", post(script_not_available))
        .route("/api/v1/scripts/deploy", post(script_not_available))
        .route("/api/v1/scripts/test", post(script_test_not_available))
        .route(
            "/api/v1/scripts/{package}/{function}",
            get(script_source_not_available),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Package handlers
// ---------------------------------------------------------------------------

async fn list_packages(
    axum::extract::State(state): axum::extract::State<StubState>,
) -> Json<Vec<OPackage>> {
    Json(state.packages.as_ref().clone())
}

async fn get_package(
    axum::extract::State(state): axum::extract::State<StubState>,
    Path(name): Path<String>,
) -> Result<Json<OPackage>, StatusCode> {
    state
        .packages
        .iter()
        .find(|p| p.name == name)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn create_package(
    Json(pkg): Json<OPackage>,
) -> (StatusCode, Json<StatusResponse>) {
    (
        StatusCode::CREATED,
        Json(StatusResponse {
            id: pkg.name,
            status: "created".into(),
            message: Some(
                "Dev server: package accepted but not persisted".into(),
            ),
        }),
    )
}

async fn delete_package(Path(_name): Path<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------
// Deployment handlers
// ---------------------------------------------------------------------------

async fn list_deployments(
    axum::extract::State(state): axum::extract::State<StubState>,
) -> Json<Vec<OClassDeployment>> {
    Json(state.deployments.as_ref().clone())
}

async fn create_deployment(
    Json(dep): Json<serde_json::Value>,
) -> (StatusCode, Json<StatusResponse>) {
    let key = dep
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    (
        StatusCode::CREATED,
        Json(StatusResponse {
            id: key,
            status: "created".into(),
            message: Some(
                "Dev server: deployment accepted but not persisted".into(),
            ),
        }),
    )
}

async fn delete_deployment(
    Path(key): Path<String>,
) -> Json<DeleteDeploymentResponse> {
    Json(DeleteDeploymentResponse {
        message: "deleted".into(),
        id: key,
        deleted_envs: vec![],
    })
}

// ---------------------------------------------------------------------------
// Environment handlers
// ---------------------------------------------------------------------------

async fn list_envs(
    axum::extract::State(state): axum::extract::State<StubState>,
) -> Json<Vec<ClusterInfoStub>> {
    Json(
        state
            .env_ports
            .iter()
            .map(|(name, port)| ClusterInfoStub {
                name: name.clone(),
                health: ClusterHealthStub {
                    status: "Healthy".into(),
                },
                gateway_url: Some(format!("http://localhost:{}", port)),
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Topology handler
// ---------------------------------------------------------------------------

async fn get_topology(
    axum::extract::State(state): axum::extract::State<StubState>,
    axum::extract::Query(_q): axum::extract::Query<TopologyQuery>,
) -> Json<TopologySnapshot> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    // Environment nodes (one per simulated env)
    for (env_name, _) in state.env_ports.iter() {
        nodes.push(TopologyNode {
            id: format!("env:{}", env_name),
            node_type: "environment".into(),
            status: Some("Healthy".into()),
            metadata: None,
            deployed_classes: None,
        });
    }

    for pkg in state.packages.iter() {
        let pkg_id = format!("pkg:{}", pkg.name);
        nodes.push(TopologyNode {
            id: pkg_id.clone(),
            node_type: "package".into(),
            status: None,
            metadata: None,
            deployed_classes: None,
        });

        for cls in &pkg.classes {
            let cls_id = format!("cls:{}.{}", pkg.name, cls.key);
            nodes.push(TopologyNode {
                id: cls_id.clone(),
                node_type: "class".into(),
                status: None,
                metadata: None,
                deployed_classes: None,
            });
            edges.push(TopologyEdge {
                from_id: pkg_id.clone(),
                to_id: cls_id.clone(),
            });

            // Find which envs this class is deployed to
            let target_envs: Vec<&str> = state
                .deployments
                .iter()
                .filter(|d| d.class_key == cls.key)
                .flat_map(|d| d.target_envs.iter().map(|s| s.as_str()))
                .collect();
            let envs_for_class = if target_envs.is_empty() {
                state
                    .env_ports
                    .iter()
                    .map(|(s, _)| s.as_str())
                    .collect::<Vec<_>>()
            } else {
                target_envs
            };

            for env_name in &envs_for_class {
                edges.push(TopologyEdge {
                    from_id: cls_id.clone(),
                    to_id: format!("env:{}", env_name),
                });
            }

            for binding in &cls.function_bindings {
                let fn_id =
                    format!("fn:{}.{}:{}", pkg.name, cls.key, binding.name);
                nodes.push(TopologyNode {
                    id: fn_id.clone(),
                    node_type: "function".into(),
                    status: None,
                    metadata: None,
                    deployed_classes: None,
                });
                edges.push(TopologyEdge {
                    from_id: cls_id.clone(),
                    to_id: fn_id,
                });
            }
        }
    }

    Json(TopologySnapshot { nodes, edges })
}

// ---------------------------------------------------------------------------
// Artifact handler
// ---------------------------------------------------------------------------

async fn list_artifacts() -> Json<Vec<serde_json::Value>> {
    Json(vec![])
}

// ---------------------------------------------------------------------------
// Script stubs (not available in dev server mode)
// ---------------------------------------------------------------------------

const SCRIPT_UNAVAILABLE_MSG: &str =
    "Script compilation is not available in dev server mode";

async fn script_not_available() -> (StatusCode, Json<ScriptErrorResponse>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ScriptErrorResponse {
            success: false,
            errors: vec![SCRIPT_UNAVAILABLE_MSG.into()],
        }),
    )
}

async fn script_test_not_available()
-> (StatusCode, Json<TestScriptErrorResponse>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(TestScriptErrorResponse {
            success: false,
            error: Some(SCRIPT_UNAVAILABLE_MSG.into()),
            logs: vec![],
            duration_ms: 0.0,
        }),
    )
}

async fn script_source_not_available(
    Path((_package, _function)): Path<(String, String)>,
) -> StatusCode {
    StatusCode::NOT_FOUND
}
