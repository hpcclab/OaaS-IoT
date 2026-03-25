//! Generic Gateway reverse proxy handler.
//!
//! Forwards all requests under `/api/gateway/*` to the Gateway service,
//! similar to nginx reverse proxy. This allows the PM to act as a single
//! entry point for the GUI without defining specific routes for each Gateway API.

use crate::{errors::ApiError, server::AppState};
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{Method, StatusCode},
    response::Response,
};
use reqwest::Client;
use std::error::Error as StdError;
use std::time::Duration;
use tracing::{debug, error};

/// Configuration for the Gateway proxy client.
///
/// Supports both single-gateway mode (`GATEWAY_URL`) and multi-gateway mode
/// (`GATEWAY_URLS_JSON` — a JSON object mapping env names to gateway URLs).
#[derive(Clone)]
pub struct GatewayProxy {
    client: Client,
    /// Default gateway URL (used when no env is specified).
    base_url: String,
    /// Per-environment gateway URLs. Key = env name, value = base URL.
    env_urls: std::collections::HashMap<String, String>,
}

impl GatewayProxy {
    pub fn new(base_url: String, timeout_seconds: u64) -> Self {
        Self::with_env_gateways(
            base_url,
            std::collections::HashMap::new(),
            timeout_seconds,
        )
    }

    /// Create a proxy that can route to multiple gateways, one per env.
    pub fn with_env_gateways(
        default_url: String,
        env_urls: std::collections::HashMap<String, String>,
        timeout_seconds: u64,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_seconds))
            .connect_timeout(Duration::from_secs(10))
            // TCP optimizations
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(60))
            // Allow reasonable connection pool for parallel requests
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .expect("Failed to create HTTP client");

        let base_url = default_url.trim_end_matches('/').to_string();
        let env_urls = env_urls
            .into_iter()
            .map(|(k, v)| (k, v.trim_end_matches('/').to_string()))
            .collect();

        Self {
            client,
            base_url,
            env_urls,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Resolve the gateway base URL for a given environment.
    /// Falls back to `base_url` if no env-specific URL is configured.
    pub fn url_for_env(&self, env: Option<&str>) -> &str {
        match env {
            Some(name) => self
                .env_urls
                .get(name)
                .map(|s| s.as_str())
                .unwrap_or(&self.base_url),
            None => &self.base_url,
        }
    }

    /// List available environment names.
    pub fn env_names(&self) -> Vec<&str> {
        self.env_urls.keys().map(|s| s.as_str()).collect()
    }
}

/// Reverse proxy handler that forwards all requests to the Gateway.
///
/// Maps `/api/gateway/{path}` -> `{GATEWAY_URL}/{path}`
///
/// Example:
/// - `/api/gateway/api/class/pkg.cls/0/objects` -> `http://gateway:8080/api/class/pkg.cls/0/objects`
#[tracing::instrument(skip(state, request), fields(method = %request.method(), uri = %request.uri()))]
pub async fn gateway_proxy(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, ApiError> {
    let proxy = state.gateway_proxy.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Gateway proxy not configured. Set GATEWAY_URL environment variable."
                .to_string(),
        )
    })?;

    // Extract the path after /api/gateway/
    let path = request.uri().path().to_owned();
    let target_path = path
        .strip_prefix("/api/gateway/")
        .or_else(|| path.strip_prefix("/api/gateway"))
        .unwrap_or("");

    forward_to_gateway(proxy, None, target_path, request).await
}

/// Per-environment gateway proxy handler.
///
/// Maps `/api/gateway/env/{env}/{path}` -> `{GATEWAY_URL_for_env}/{path}`
#[tracing::instrument(skip(state, request), fields(method = %request.method(), uri = %request.uri()))]
pub async fn gateway_proxy_env(
    State(state): State<AppState>,
    Path((env_name, rest)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let proxy = state.gateway_proxy.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "Gateway proxy not configured. Set GATEWAY_URL environment variable."
                .to_string(),
        )
    })?;

    forward_to_gateway(proxy, Some(&env_name), &rest, request).await
}

/// Shared forwarding logic for both default and per-env gateway proxies.
async fn forward_to_gateway(
    proxy: &GatewayProxy,
    env_name: Option<&str>,
    target_path: &str,
    request: Request,
) -> Result<Response, ApiError> {
    // Build the target URL
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let gw_base = proxy.url_for_env(env_name);
    let target_url = format!("{}/{}{}", gw_base, target_path, query);

    debug!("Proxying request to: {}", target_url);

    // Forward the request
    let method = request.method().clone();
    let headers = request.headers().clone();
    let body_bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .map_err(|e| {
            error!("Failed to read request body: {}", e);
            ApiError::InternalServerError(format!(
                "Failed to read request body: {}",
                e
            ))
        })?;

    // Build the proxied request
    let mut req_builder = match method {
        Method::GET => proxy.client.get(&target_url),
        Method::POST => proxy.client.post(&target_url),
        Method::PUT => proxy.client.put(&target_url),
        Method::DELETE => proxy.client.delete(&target_url),
        Method::PATCH => proxy.client.patch(&target_url),
        Method::HEAD => proxy.client.head(&target_url),
        _ => {
            return Err(ApiError::BadRequest(format!(
                "Unsupported method: {}",
                method
            )));
        }
    };

    // Copy relevant headers (skip hop-by-hop headers)
    for (name, value) in headers.iter() {
        let name_str = name.as_str().to_lowercase();
        // Skip hop-by-hop headers
        if matches!(
            name_str.as_str(),
            "host"
                | "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "te"
                | "trailer"
        ) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req_builder = req_builder.header(name.as_str(), v);
        }
    }

    // Add body for methods that support it
    if !body_bytes.is_empty() {
        req_builder = req_builder.body(body_bytes.to_vec());
    }

    // Send the request
    let response = req_builder.send().await.map_err(|e| {
        // Log detailed error information for debugging
        let is_timeout = e.is_timeout();
        let is_connect = e.is_connect();
        let is_request = e.is_request();
        error!(
            "Failed to proxy request to Gateway: {} (timeout={}, connect={}, request={}, source={:?})",
            e, is_timeout, is_connect, is_request, e.source()
        );
        ApiError::ServiceUnavailable(format!("Gateway request failed: {}", e))
    })?;

    // Convert reqwest response to axum response
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let resp_headers = response.headers().clone();
    let body_bytes = response.bytes().await.map_err(|e| {
        error!("Failed to read Gateway response body: {}", e);
        ApiError::InternalServerError(format!(
            "Failed to read Gateway response: {}",
            e
        ))
    })?;

    // Build response
    let mut builder = Response::builder().status(status);

    // Copy response headers (skip hop-by-hop headers)
    for (name, value) in resp_headers.iter() {
        let name_str = name.as_str().to_lowercase();
        if matches!(
            name_str.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "te"
                | "trailer"
        ) {
            continue;
        }
        builder = builder.header(name, value);
    }

    builder.body(Body::from(body_bytes.to_vec())).map_err(|e| {
        ApiError::InternalServerError(format!(
            "Failed to build response: {}",
            e
        ))
    })
}
