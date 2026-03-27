use crate::{
    api::{GatewayProxy, create_middleware_stack, handlers},
    config::{GatewayProxyConfig, ServerConfig},
    crm::CrmManager,
    services::{
        DeploymentService, PackageService, ScriptService,
        artifact::{ArtifactStore, SourceStore},
    },
};
use axum::http::StatusCode;
use axum::{
    Router,
    extract::Extension,
    routing::{delete, get, post},
};
use oprc_observability::{OtelMetrics, otel_metrics_middleware};
use std::{net::SocketAddr, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub package_service: Arc<PackageService>,
    pub deployment_service: Arc<DeploymentService>,
    pub crm_manager: Arc<CrmManager>,
    pub gateway_proxy: Option<Arc<GatewayProxy>>,
    pub artifact_store: Option<Arc<dyn ArtifactStore>>,
    pub source_store: Option<Arc<dyn SourceStore>>,
    pub script_service: Option<Arc<ScriptService>>,
    #[cfg(feature = "network-sim")]
    pub netsim_manager: Option<Arc<crate::services::netsim::NetsimManager>>,
}

pub struct ApiServer {
    app: Router,
    config: ServerConfig,
}

impl ApiServer {
    pub fn new(
        package_service: Arc<PackageService>,
        deployment_service: Arc<DeploymentService>,
        crm_manager: Arc<CrmManager>,
        config: ServerConfig,
    ) -> Self {
        Self::with_all(
            package_service,
            deployment_service,
            crm_manager,
            config,
            None,
            None,
            None,
            None,
        )
    }

    pub fn with_gateway(
        package_service: Arc<PackageService>,
        deployment_service: Arc<DeploymentService>,
        crm_manager: Arc<CrmManager>,
        config: ServerConfig,
        gateway_config: Option<GatewayProxyConfig>,
    ) -> Self {
        Self::with_all(
            package_service,
            deployment_service,
            crm_manager,
            config,
            gateway_config,
            None,
            None,
            None,
        )
    }

    /// Create a fully-featured server with script support.
    pub fn with_all(
        package_service: Arc<PackageService>,
        deployment_service: Arc<DeploymentService>,
        crm_manager: Arc<CrmManager>,
        config: ServerConfig,
        gateway_config: Option<GatewayProxyConfig>,
        artifact_store: Option<Arc<dyn ArtifactStore>>,
        source_store: Option<Arc<dyn SourceStore>>,
        script_service: Option<Arc<ScriptService>>,
    ) -> Self {
        // Initialize OTEL metrics
        let otel_metrics = Arc::new(OtelMetrics::new("oprc-pm"));

        // Create gateway proxy if configured
        let (gateway_proxy, gateway_max_payload) = match gateway_config {
            Some(cfg) => {
                info!(
                    "Gateway proxy enabled: {} (max payload: {} bytes, env_gateways: {:?})",
                    cfg.url,
                    cfg.max_payload_bytes,
                    cfg.env_urls.keys().collect::<Vec<_>>()
                );
                (
                    Some(Arc::new(GatewayProxy::with_env_gateways(
                        cfg.url,
                        cfg.env_urls,
                        cfg.timeout_seconds,
                    ))),
                    cfg.max_payload_bytes,
                )
            }
            None => (None, 50 * 1024 * 1024),
        };

        let state = AppState {
            package_service,
            deployment_service,
            crm_manager,
            gateway_proxy,
            artifact_store,
            source_store,
            script_service,
            #[cfg(feature = "network-sim")]
            netsim_manager: None,
        };

        let app = Self::build_router(
            state,
            otel_metrics,
            gateway_max_payload,
        );

        Self { app, config }
    }

    fn build_router(
        state: AppState,
        otel_metrics: Arc<OtelMetrics>,
        gateway_max_payload: usize,
    ) -> Router {
        Router::new()
            // Package Management APIs
            .route("/api/v1/packages", post(handlers::create_package))
            .route("/api/v1/packages", get(handlers::list_packages))
            .route("/api/v1/packages/{name}", get(handlers::get_package))
            .route("/api/v1/packages/{name}", post(handlers::update_package))
            .route("/api/v1/packages/{name}", delete(handlers::delete_package))
            // Deployment Management APIs
            .route("/api/v1/deployments", get(handlers::list_deployments))
            .route("/api/v1/deployments", post(handlers::create_deployment))
            .route("/api/v1/deployments/{key}", get(handlers::get_deployment))
            .route(
                "/api/v1/deployments/{key}",
                delete(handlers::delete_deployment),
            )
            .route("/api/v1/class-runtimes", get(handlers::list_class_runtimes))
            .route(
                "/api/v1/class-runtimes/{id}",
                get(handlers::get_class_runtime),
            )
            .route(
                "/api/v1/deployment-status/{id}",
                get(handlers::get_deployment_status),
            )
            .route(
                "/api/v1/deployments/{key}/cluster-mappings",
                get(handlers::get_deployment_mappings),
            )
            .route(
                "/api/v1/deployments/{key}/env-mappings",
                get(handlers::get_deployment_mappings),
            )
            .route("/api/v1/envs", get(handlers::list_clusters))
            .route("/api/v1/envs/health", get(handlers::list_clusters_health))
            .route(
                "/api/v1/envs/{name}/health",
                get(handlers::get_cluster_health),
            )
            .route("/api/v1/topology", get(handlers::get_topology))
            .route("/api/v1/classes", get(handlers::list_classes))
            .route("/api/v1/functions", get(handlers::list_functions))
            // Artifact Storage API
            .route("/api/v1/artifacts", get(handlers::list_artifacts))
            .route("/api/v1/artifacts/{id}", get(handlers::get_artifact))
            // Script APIs
            .route("/api/v1/scripts/compile", post(handlers::compile_script))
            .route("/api/v1/scripts/build", post(handlers::build_script))
            .route("/api/v1/scripts/test", post(handlers::test_script))
            .route("/api/v1/scripts/deploy", post(handlers::deploy_script))
            .route(
                "/api/v1/scripts/{package}/{function}",
                get(handlers::get_script_source),
            )
            // Gateway reverse proxy (default — no env)
            .route("/api/gateway/{*path}", get(handlers::gateway_proxy))
            .route("/api/gateway/{*path}", post(handlers::gateway_proxy))
            .route(
                "/api/gateway/{*path}",
                axum::routing::put(handlers::gateway_proxy),
            )
            .route("/api/gateway/{*path}", delete(handlers::gateway_proxy))
            // Gateway reverse proxy (per-env)
            .route(
                "/api/gateway/env/{env}/{*path}",
                get(handlers::gateway_proxy_env),
            )
            .route(
                "/api/gateway/env/{env}/{*path}",
                post(handlers::gateway_proxy_env),
            )
            .route(
                "/api/gateway/env/{env}/{*path}",
                axum::routing::put(handlers::gateway_proxy_env),
            )
            .route(
                "/api/gateway/env/{env}/{*path}",
                delete(handlers::gateway_proxy_env),
            )
            // Health check endpoint
            .route("/health", get(health_check))
            .layer(axum::extract::DefaultBodyLimit::max(gateway_max_payload))
            .layer(axum::middleware::from_fn(otel_metrics_middleware))
            .layer(Extension(otel_metrics))
            .layer(create_middleware_stack())
            .with_state(state)
    }

    /// Apply the SPA fallback service. Must be called **after** all routes
    /// (including optional netsim routes) have been merged.
    fn finalize(self) -> Router {
        self.app.fallback_service(
            ServeDir::new(&self.config.static_dir).not_found_service(
                ServeFile::new(format!(
                    "{}/index.html",
                    self.config.static_dir
                )),
            ),
        )
    }

    pub async fn serve(self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from(([0, 0, 0, 0], self.config.port));
        let listener = tokio::net::TcpListener::bind(addr).await?;

        info!("Package Manager API server listening on {}", addr);
        info!("Health check available at: http://{}/health", addr);
        info!("API documentation: http://{}/api/v1", addr);

        let app = self.finalize();
        axum::serve(listener, app).await?;

        Ok(())
    }

    /// Merge additional routes into this server's router.
    #[cfg(feature = "network-sim")]
    pub fn merge_netsim(
        mut self,
        manager: Arc<crate::services::netsim::NetsimManager>,
    ) -> Self {
        let netsim_routes =
            crate::api::handlers::network_sim::build_netsim_api(manager);
        self.app = self.app.merge(netsim_routes);
        self
    }

    /// Consume and return the underlying Axum Router so callers can serve it themselves
    /// (e.g., on an ephemeral port in tests) and discover the bound address.
    pub fn into_router(self) -> Router {
        self.finalize()
    }
}

async fn health_check(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    // We use package storage as the representative backend check.
    match state.package_service.health().await {
        Ok(()) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "status": "healthy",
                "service": "oprc-pm",
                "version": env!("CARGO_PKG_VERSION"),
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "storage": {"status": "ok"}
            })),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "status": "unhealthy",
                "service": "oprc-pm",
                "version": env!("CARGO_PKG_VERSION"),
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "storage": {"status": "error", "message": e.to_string()}
            })),
        ),
    }
}
