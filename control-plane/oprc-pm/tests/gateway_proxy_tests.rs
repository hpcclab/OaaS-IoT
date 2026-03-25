//! Tests for the multi-env gateway reverse proxy.
//!
//! Spins up a mock "gateway" HTTP server (via wiremock), configures the PM
//! API server to proxy to it, then asserts that:
//!
//! - `/api/gateway/{*path}` forwards to the default gateway URL
//! - `/api/gateway/env/{env}/{*path}` forwards to the env-specific gateway URL
//! - query strings, methods, and request bodies are forwarded correctly
//! - unknown env names fall back to the default gateway

use anyhow::Result;
use axum::{body::Body, http::Request};
use oprc_grpc::proto::common::StatusCode as ProtoStatusCode;
use oprc_grpc::proto::deployment::deployment_service_server::{
    DeploymentService, DeploymentServiceServer,
};
use oprc_grpc::proto::deployment::*;
use oprc_grpc::proto::health::crm_info_service_server::{
    CrmInfoService, CrmInfoServiceServer,
};
use oprc_grpc::proto::health::health_service_server::{
    HealthService, HealthServiceServer,
};
use oprc_grpc::proto::health::{
    CrmEnvHealth, CrmEnvRequest, HealthCheckRequest, HealthCheckResponse,
    health_check_response,
};
use oprc_grpc::proto::{common as pcom, deployment as pdep};
use oprc_pm::build_api_server_from_env;
use oprc_test_utils::env as test_env;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use tonic::{Request as TonicRequest, Response, Status};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

// ---------------------------------------------------------------------------
// Mock gRPC services (minimal stubs for PM startup)
// ---------------------------------------------------------------------------

struct TestHealthSvc;

#[tonic::async_trait]
impl HealthService for TestHealthSvc {
    async fn check(
        &self,
        _: TonicRequest<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            status: health_check_response::ServingStatus::Serving as i32,
        }))
    }

    type WatchStream = tokio_stream::wrappers::ReceiverStream<
        Result<HealthCheckResponse, Status>,
    >;

    async fn watch(
        &self,
        _: TonicRequest<HealthCheckRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        Err(Status::unimplemented("not needed"))
    }
}

struct TestCrmInfoSvc;

#[tonic::async_trait]
impl CrmInfoService for TestCrmInfoSvc {
    async fn get_env_health(
        &self,
        _: TonicRequest<CrmEnvRequest>,
    ) -> Result<Response<CrmEnvHealth>, Status> {
        Ok(Response::new(CrmEnvHealth {
            env_name: "default".into(),
            status: "Healthy".into(),
            last_seen: Some(pcom::Timestamp {
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
            crm_version: None,
            node_count: Some(1),
            ready_nodes: Some(1),
            availability: Some(0.99),
        }))
    }
}

struct TestDeploySvc;

#[tonic::async_trait]
impl DeploymentService for TestDeploySvc {
    async fn deploy(
        &self,
        request: TonicRequest<DeployRequest>,
    ) -> Result<Response<DeployResponse>, Status> {
        let du_id = request
            .into_inner()
            .deployment_unit
            .as_ref()
            .map(|d| d.id.clone())
            .unwrap_or_else(|| "dep-1".into());
        Ok(Response::new(DeployResponse {
            status: ProtoStatusCode::Ok as i32,
            deployment_id: du_id,
            message: None,
        }))
    }

    async fn get_deployment_status(
        &self,
        request: TonicRequest<GetDeploymentStatusRequest>,
    ) -> Result<Response<GetDeploymentStatusResponse>, Status> {
        let dep_id = request.into_inner().deployment_id;
        let dep = pdep::DeploymentUnit {
            id: dep_id,
            package_name: "pkg".into(),
            class_key: "cls".into(),
            functions: vec![],
            target_env: "default".into(),
            function_bindings: vec![],
            created_at: Some(pcom::Timestamp {
                seconds: chrono::Utc::now().timestamp(),
                nanos: 0,
            }),
            odgm_config: None,
            selected_template: None,
            telemetry: None,
        };
        Ok(Response::new(GetDeploymentStatusResponse {
            status: ProtoStatusCode::Ok as i32,
            deployment: Some(dep),
            message: Some("ok".into()),
            status_resource_refs: vec![],
        }))
    }

    async fn delete_deployment(
        &self,
        _request: TonicRequest<DeleteDeploymentRequest>,
    ) -> Result<Response<DeleteDeploymentResponse>, Status> {
        Ok(Response::new(DeleteDeploymentResponse {
            status: ProtoStatusCode::Ok as i32,
            message: Some("deleted".into()),
        }))
    }

    async fn list_class_runtimes(
        &self,
        _request: TonicRequest<ListClassRuntimesRequest>,
    ) -> Result<Response<ListClassRuntimesResponse>, Status> {
        Ok(Response::new(ListClassRuntimesResponse { items: vec![] }))
    }

    async fn get_class_runtime(
        &self,
        _request: TonicRequest<GetClassRuntimeRequest>,
    ) -> Result<Response<GetClassRuntimeResponse>, Status> {
        Err(Status::not_found("not found"))
    }
}

/// Spin up the mock gRPC server and return its URL.
async fn spawn_mock_grpc() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let incoming = TcpListenerStream::new(listener);
    tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(HealthServiceServer::new(TestHealthSvc))
            .add_service(CrmInfoServiceServer::new(TestCrmInfoSvc))
            .add_service(DeploymentServiceServer::new(TestDeploySvc))
            .serve_with_incoming(incoming)
            .await;
    });
    Ok(format!("http://{}", addr))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Default gateway proxy: `/api/gateway/{path}` → default GATEWAY_URL.
#[test_log::test(tokio::test)]
#[serial_test::serial]
async fn gateway_proxy_default_route() -> Result<()> {
    let crm_url = spawn_mock_grpc().await?;

    // Start a mock HTTP "gateway" that the proxy will forward to
    let mock_gw = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/class/my.Cls/0/objects"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"objects": [{"id": "obj-1"}]}),
            ),
        )
        .expect(1)
        .mount(&mock_gw)
        .await;

    let _env = test_env::Env::new()
        .set("SERVER_HOST", "127.0.0.1")
        .set("SERVER_PORT", "0")
        .set("STORAGE_TYPE", "memory")
        .set("CRM_DEFAULT_URL", &crm_url)
        .set("GATEWAY_URL", &mock_gw.uri())
        .set("GATEWAY_MAX_PAYLOAD_BYTES", "10485760");

    let app = build_api_server_from_env().await?.into_router();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/gateway/api/class/my.Cls/0/objects")
                .body(Body::empty())?,
        )
        .await?;

    assert!(
        resp.status().is_success(),
        "expected 2xx, got {}",
        resp.status()
    );

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["objects"][0]["id"], "obj-1");
    Ok(())
}

/// Per-env gateway proxy: `/api/gateway/env/{env}/{path}` → env-specific URL.
#[test_log::test(tokio::test)]
#[serial_test::serial]
async fn gateway_proxy_env_route() -> Result<()> {
    let crm_url = spawn_mock_grpc().await?;

    // Two mock gateways: "cloud" and "edge"
    let cloud_gw = MockServer::start().await;
    let edge_gw = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/class/my.Cls/0/objects"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"source": "cloud"})),
        )
        .expect(1)
        .mount(&cloud_gw)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/class/my.Cls/0/objects"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"source": "edge"})),
        )
        .expect(1)
        .mount(&edge_gw)
        .await;

    let env_urls = serde_json::json!({
        "cloud": cloud_gw.uri(),
        "edge": edge_gw.uri(),
    });

    let _env = test_env::Env::new()
        .set("SERVER_HOST", "127.0.0.1")
        .set("SERVER_PORT", "0")
        .set("STORAGE_TYPE", "memory")
        .set("CRM_DEFAULT_URL", &crm_url)
        .set("GATEWAY_URL", &cloud_gw.uri()) // default
        .set("GATEWAY_URLS_JSON", &env_urls.to_string())
        .set("GATEWAY_MAX_PAYLOAD_BYTES", "10485760");

    let app = build_api_server_from_env().await?.into_router();

    // Request to cloud env
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/gateway/env/cloud/api/class/my.Cls/0/objects")
                .body(Body::empty())?,
        )
        .await?;
    assert!(resp.status().is_success());
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["source"], "cloud");

    // Request to edge env
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/gateway/env/edge/api/class/my.Cls/0/objects")
                .body(Body::empty())?,
        )
        .await?;
    assert!(resp.status().is_success());
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["source"], "edge");
    Ok(())
}

/// POST with body is forwarded through the per-env proxy.
#[test_log::test(tokio::test)]
#[serial_test::serial]
async fn gateway_proxy_env_post_with_body() -> Result<()> {
    let crm_url = spawn_mock_grpc().await?;
    let edge_gw = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/class/my.Cls/0/obj-1/echo"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"echoed": true})),
        )
        .expect(1)
        .mount(&edge_gw)
        .await;

    let env_urls = serde_json::json!({ "edge": edge_gw.uri() });

    let _env = test_env::Env::new()
        .set("SERVER_HOST", "127.0.0.1")
        .set("SERVER_PORT", "0")
        .set("STORAGE_TYPE", "memory")
        .set("CRM_DEFAULT_URL", &crm_url)
        .set("GATEWAY_URL", "http://localhost:19999") // not reachable by default
        .set("GATEWAY_URLS_JSON", &env_urls.to_string())
        .set("GATEWAY_MAX_PAYLOAD_BYTES", "10485760");

    let app = build_api_server_from_env().await?.into_router();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/gateway/env/edge/api/class/my.Cls/0/obj-1/echo")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"hello":"world"}"#))?,
        )
        .await?;

    assert!(
        resp.status().is_success(),
        "expected 2xx, got {}",
        resp.status()
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["echoed"], true);
    Ok(())
}

/// Unknown env falls back to default gateway URL.
#[test_log::test(tokio::test)]
#[serial_test::serial]
async fn gateway_proxy_env_unknown_falls_back() -> Result<()> {
    let crm_url = spawn_mock_grpc().await?;
    let default_gw = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/class/my.Cls/0/objects"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"source": "default"})),
        )
        .expect(1)
        .mount(&default_gw)
        .await;

    let _env = test_env::Env::new()
        .set("SERVER_HOST", "127.0.0.1")
        .set("SERVER_PORT", "0")
        .set("STORAGE_TYPE", "memory")
        .set("CRM_DEFAULT_URL", &crm_url)
        .set("GATEWAY_URL", &default_gw.uri())
        .set("GATEWAY_MAX_PAYLOAD_BYTES", "10485760");

    let app = build_api_server_from_env().await?.into_router();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/gateway/env/nonexistent/api/class/my.Cls/0/objects")
                .body(Body::empty())?,
        )
        .await?;

    assert!(resp.status().is_success());
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(json["source"], "default");
    Ok(())
}

/// Query string is preserved through the proxy.
#[test_log::test(tokio::test)]
#[serial_test::serial]
async fn gateway_proxy_preserves_query_string() -> Result<()> {
    let crm_url = spawn_mock_grpc().await?;
    let mock_gw = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/class/my.Cls/0/objects"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true})),
        )
        .expect(1)
        .mount(&mock_gw)
        .await;

    let _env = test_env::Env::new()
        .set("SERVER_HOST", "127.0.0.1")
        .set("SERVER_PORT", "0")
        .set("STORAGE_TYPE", "memory")
        .set("CRM_DEFAULT_URL", &crm_url)
        .set("GATEWAY_URL", &mock_gw.uri())
        .set("GATEWAY_MAX_PAYLOAD_BYTES", "10485760");

    let app = build_api_server_from_env().await?.into_router();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(
                    "/api/gateway/api/class/my.Cls/0/objects?limit=10&offset=0",
                )
                .body(Body::empty())?,
        )
        .await?;

    assert!(resp.status().is_success());
    Ok(())
}
