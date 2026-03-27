//! WASM module fetching, compilation, and caching.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use wasmtime::Engine;
use wasmtime::component::Component;

/// Which WIT world a compiled WASM component targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldType {
    /// Procedural world: guest exports `guest-function` (invoke-fn / invoke-obj).
    Legacy,
    /// OOP world: guest exports `guest-object` (on-invoke with object-proxy).
    ObjectOriented,
}

/// Compiled WASM component with metadata
pub struct CompiledModule {
    pub component: Component,
    pub source_url: String,
    pub world_type: WorldType,
}

/// Detect the world type by inspecting the component's exported interface names.
fn detect_world_type(engine: &Engine, component: &Component) -> WorldType {
    let ct = component.component_type();
    for (name, _) in ct.exports(engine) {
        if name.contains("guest-object") {
            return WorldType::ObjectOriented;
        }
    }
    WorldType::Legacy
}

/// Fetches, compiles, and caches WASM components by function ID.
pub struct WasmModuleStore {
    engine: Engine,
    modules: Arc<RwLock<HashMap<String, Arc<CompiledModule>>>>,
}

impl WasmModuleStore {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            modules: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Load a WASM component from a URL (HTTP/HTTPS/file://), compile, and cache it.
    pub async fn load(
        &self,
        fn_id: &str,
        url: &str,
    ) -> Result<Arc<CompiledModule>> {
        info!(fn_id, url, "loading WASM module");
        let bytes = fetch_module(url).await?;
        let component = Component::from_binary(&self.engine, &bytes)?;
        let world_type = detect_world_type(&self.engine, &component);
        let module = Arc::new(CompiledModule {
            component,
            source_url: url.to_string(),
            world_type,
        });
        self.modules
            .write()
            .await
            .insert(fn_id.to_string(), module.clone());
        info!(fn_id, url, ?world_type, "WASM module loaded and cached");
        Ok(module)
    }

    /// Load a WASM component directly from bytes (useful for testing).
    pub async fn load_from_bytes(
        &self,
        fn_id: &str,
        bytes: &[u8],
    ) -> Result<Arc<CompiledModule>> {
        debug!(fn_id, len = bytes.len(), "loading WASM module from bytes");
        let component = Component::from_binary(&self.engine, bytes)?;
        let world_type = detect_world_type(&self.engine, &component);
        let module = Arc::new(CompiledModule {
            component,
            source_url: "bytes://".to_string(),
            world_type,
        });
        self.modules
            .write()
            .await
            .insert(fn_id.to_string(), module.clone());
        Ok(module)
    }

    /// Get a previously loaded module by function ID.
    pub async fn get(&self, fn_id: &str) -> Option<Arc<CompiledModule>> {
        self.modules.read().await.get(fn_id).cloned()
    }

    /// Insert a pre-compiled module under a function ID (for deduplication).
    pub async fn insert(&self, fn_id: &str, module: Arc<CompiledModule>) {
        self.modules.write().await.insert(fn_id.to_string(), module);
    }

    /// Remove a cached module.
    pub async fn remove(&self, fn_id: &str) -> bool {
        let removed = self.modules.write().await.remove(fn_id).is_some();
        if removed {
            debug!(fn_id, "removed WASM module from cache");
        }
        removed
    }

    /// Number of cached modules.
    pub async fn len(&self) -> usize {
        self.modules.read().await.len()
    }

    /// Whether the cache is empty.
    pub async fn is_empty(&self) -> bool {
        self.modules.read().await.is_empty()
    }

    /// Reference to the wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// Fetch module bytes from a URL.
///
/// Supported schemes:
/// - `file://path` — local filesystem
/// - `http://` / `https://` — plain HTTP fetch
/// - `oci://registry/repo:tag` — OCI registry pull (HTTPS)
/// - `oci+http://registry/repo:tag` — OCI registry pull (plain HTTP, for dev registries)
///
/// OCI authentication: set `OCI_USERNAME` + `OCI_PASSWORD` env vars for private registries.
/// TLS verification: set `OCI_INSECURE=true` to skip certificate verification (dev only).
async fn fetch_module(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        debug!(path, "reading WASM module from filesystem");
        tokio::fs::read(path).await.with_context(|| {
            format!("failed to read WASM module from {}", path)
        })
    } else if url.starts_with("http://") || url.starts_with("https://") {
        debug!(url, "fetching WASM module over HTTP");
        let resp = reqwest::get(url).await.with_context(|| {
            format!("failed to fetch WASM module from {}", url)
        })?;
        if !resp.status().is_success() {
            bail!("HTTP {} fetching WASM module from {}", resp.status(), url);
        }
        let bytes = resp.bytes().await.with_context(|| {
            format!("failed to read response body from {}", url)
        })?;
        Ok(bytes.to_vec())
    } else if url.starts_with("oci://") || url.starts_with("oci+http://") {
        fetch_oci_module(url).await
    } else {
        bail!("unsupported WASM module URL scheme: {}", url);
    }
}

// ─── OCI Registry Pull ─────────────────────────────────────

/// Pull a single-artifact WASM layer from an OCI registry.
///
/// URL formats:
/// - `oci://registry/repo:tag`
/// - `oci://registry/repo@sha256:<digest>`
/// - `oci+http://registry/repo:tag`  (plain HTTP, for dev)
async fn fetch_oci_module(url: &str) -> Result<Vec<u8>> {
    let (proto, reference) = if let Some(r) = url.strip_prefix("oci+http://") {
        ("http", r)
    } else {
        (
            "https",
            url.strip_prefix("oci://")
                .context("expected oci:// scheme")?,
        )
    };

    // Split registry from the rest: "registry/repo:tag"
    let slash = reference
        .find('/')
        .context("OCI reference must be registry/repo:tag")?;
    let registry = &reference[..slash];
    let rest = &reference[slash + 1..];

    // Split repo from tag or digest
    let (repo, ref_str) = if let Some(at) = rest.find('@') {
        // digest reference: repo@sha256:abc  →  ref = "sha256:abc"
        (&rest[..at], &rest[at + 1..])
    } else if let Some(colon) = rest.rfind(':') {
        (&rest[..colon], &rest[colon + 1..])
    } else {
        (rest, "latest")
    };

    info!(
        registry,
        repo, ref_str, proto, "pulling WASM module from OCI registry"
    );

    let insecure = std::env::var("OCI_INSECURE")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(insecure)
        .build()?;

    let credentials_owned = {
        let user = std::env::var("OCI_USERNAME").ok();
        let pass = std::env::var("OCI_PASSWORD").ok();
        user.zip(pass)
    };
    let credentials = credentials_owned
        .as_ref()
        .map(|(u, p)| (u.as_str(), p.as_str()));

    let base = format!("{}://{}/v2/{}", proto, registry, repo);

    // 1. Fetch manifest to find layer digest
    let manifest_bytes = oci_get(
        &client,
        &format!("{}/manifests/{}", base, ref_str),
        &[
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        ],
        credentials,
        registry,
        repo,
        proto,
    )
    .await
    .context("failed to fetch OCI manifest")?;

    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes)
            .context("OCI manifest is not valid JSON")?;

    // 2. Find the WASM layer (prefer application/wasm, fallback to first layer)
    let layers = manifest["layers"]
        .as_array()
        .context("OCI manifest missing 'layers' array")?;

    let layer = layers
        .iter()
        .find(|l| l["mediaType"].as_str() == Some("application/wasm"))
        .or_else(|| layers.first())
        .context("OCI manifest has no layers")?;

    let digest = layer["digest"]
        .as_str()
        .context("OCI layer missing 'digest'")?;
    let size = layer["size"].as_u64().unwrap_or(0);

    debug!(digest, size, "pulling OCI blob");

    // 3. Pull the blob
    let blob = oci_get(
        &client,
        &format!("{}/blobs/{}", base, digest),
        &[],
        credentials,
        registry,
        repo,
        proto,
    )
    .await
    .context("failed to fetch OCI blob")?;

    info!(
        registry,
        repo,
        ref_str,
        bytes = blob.len(),
        "OCI WASM module fetched"
    );
    Ok(blob)
}

/// Perform an OCI registry GET request, handling Bearer token auth automatically.
async fn oci_get(
    client: &reqwest::Client,
    url: &str,
    accept_types: &[&str],
    credentials: Option<(&str, &str)>,
    registry: &str,
    repo: &str,
    proto: &str,
) -> Result<Vec<u8>> {
    let resp = {
        let mut req = client.get(url);
        for &mt in accept_types {
            req = req.header(reqwest::header::ACCEPT, mt);
        }
        req.send().await.with_context(|| format!("GET {url}"))?
    };

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        let www_auth = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let token = if let Some(ref header) = www_auth {
            oci_bearer_token(client, header, credentials).await?
        } else if let Some((user, pass)) = credentials {
            // Fallback: basic auth embedded in retry
            let mut req = client.get(url).basic_auth(user, Some(pass));
            for &mt in accept_types {
                req = req.header(reqwest::header::ACCEPT, mt);
            }
            let r = req
                .send()
                .await
                .with_context(|| format!("GET {url} (basic auth)"))?;
            if !r.status().is_success() {
                bail!("OCI {} {url}", r.status());
            }
            return Ok(r.bytes().await?.to_vec());
        } else {
            // No www-authenticate and no credentials — try a scope-free token
            let token_url = format!(
                "{}://{}/service/token?scope=repository:{}:pull",
                proto, registry, repo
            );
            let r = client.get(&token_url).send().await?;
            if r.status().is_success() {
                let body: serde_json::Value =
                    serde_json::from_slice(&r.bytes().await?)
                        .context("OCI anonymous token response not JSON")?;
                body["token"]
                    .as_str()
                    .or_else(|| body["access_token"].as_str())
                    .map(|s| s.to_string())
                    .context("OCI anonymous token missing 'token' field")?
            } else {
                bail!(
                    "OCI registry requires authentication and no credentials provided"
                );
            }
        };

        let mut req = client.get(url).bearer_auth(&token);
        for &mt in accept_types {
            req = req.header(reqwest::header::ACCEPT, mt);
        }
        let r = req
            .send()
            .await
            .with_context(|| format!("GET {url} (bearer)"))?;
        if !r.status().is_success() {
            bail!("OCI {} {url}", r.status());
        }
        return Ok(r.bytes().await?.to_vec());
    }

    if !resp.status().is_success() {
        bail!("OCI {} {url}", resp.status());
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Exchange a `Www-Authenticate: Bearer` challenge for a token.
async fn oci_bearer_token(
    client: &reqwest::Client,
    www_auth: &str,
    credentials: Option<(&str, &str)>,
) -> Result<String> {
    let realm = parse_bearer_param(www_auth, "realm")
        .context("Www-Authenticate missing 'realm'")?;
    let service = parse_bearer_param(www_auth, "service").ok();
    let scope = parse_bearer_param(www_auth, "scope").ok();

    // Build the token URL manually — avoids needing reqwest's `query` feature
    let mut token_url = realm.clone();
    if let Some(s) = &service {
        oci_append_query(&mut token_url, "service", s);
    }
    if let Some(s) = &scope {
        oci_append_query(&mut token_url, "scope", s);
    }

    let mut req = client.get(&token_url);
    if let Some((user, pass)) = credentials {
        req = req.basic_auth(user, Some(pass));
    }

    let resp = req
        .send()
        .await
        .context("OCI token endpoint request failed")?;
    if !resp.status().is_success() {
        bail!("OCI token endpoint {} {}", resp.status(), realm);
    }

    let body: serde_json::Value = serde_json::from_slice(&resp.bytes().await?)
        .context("OCI token response not JSON")?;
    body["token"]
        .as_str()
        .or_else(|| body["access_token"].as_str())
        .map(|s| s.to_string())
        .context("OCI token response missing 'token' / 'access_token' field")
}

/// Append a query parameter to a URL string (handles existing `?` vs first param).
fn oci_append_query(url: &mut String, key: &str, val: &str) {
    url.push(if url.contains('?') { '&' } else { '?' });
    url.push_str(key);
    url.push('=');
    url.push_str(val);
}

/// Extract a named parameter from a `Bearer ...` challenge header.
/// e.g. `Bearer realm="https://host/auth",service="harbor"`  →  `realm` → `"https://host/auth"`
fn parse_bearer_param(header: &str, param: &str) -> Result<String> {
    let needle = format!("{}=\"", param);
    let start = header
        .find(needle.as_str())
        .with_context(|| format!("Www-Authenticate missing '{param}'"))?;
    let value_start = start + needle.len();
    let value_end = header[value_start..]
        .find('"')
        .context("unterminated quoted value in Www-Authenticate")?;
    Ok(header[value_start..value_start + value_end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> Engine {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        Engine::new(&config).unwrap()
    }

    // Minimal valid WASM component (empty component)
    fn minimal_component_bytes() -> Vec<u8> {
        // A minimal valid wasm component (component section magic + layer)
        // Using wat to produce a minimal component
        wat::parse_str("(component)").expect("valid minimal component WAT")
    }

    #[tokio::test]
    async fn load_from_bytes_and_get() {
        let store = WasmModuleStore::new(test_engine());
        let bytes = minimal_component_bytes();
        let module = store.load_from_bytes("test-fn", &bytes).await.unwrap();
        assert_eq!(module.source_url, "bytes://");

        let cached = store.get("test-fn").await;
        assert!(cached.is_some());
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let store = WasmModuleStore::new(test_engine());
        assert!(store.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn remove_drops_module() {
        let store = WasmModuleStore::new(test_engine());
        let bytes = minimal_component_bytes();
        store.load_from_bytes("fn1", &bytes).await.unwrap();
        assert_eq!(store.len().await, 1);

        assert!(store.remove("fn1").await);
        assert_eq!(store.len().await, 0);
        assert!(store.get("fn1").await.is_none());
    }

    #[tokio::test]
    async fn remove_nonexistent_returns_false() {
        let store = WasmModuleStore::new(test_engine());
        assert!(!store.remove("nope").await);
    }

    #[tokio::test]
    async fn load_replaces_existing() {
        let store = WasmModuleStore::new(test_engine());
        let bytes = minimal_component_bytes();
        store.load_from_bytes("fn1", &bytes).await.unwrap();
        store.load_from_bytes("fn1", &bytes).await.unwrap();
        assert_eq!(store.len().await, 1);
    }

    #[tokio::test]
    async fn fetch_module_unsupported_scheme() {
        let result = fetch_module("ftp://example.com/fn.wasm").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported"));
    }

    // ─── OCI helpers ───────────────────────────────────────

    #[test]
    fn parse_bearer_param_realm() {
        let header = r#"Bearer realm="https://harbor.example.com/service/token",service="harbor",scope="repository:oaas/app:pull""#;
        assert_eq!(
            parse_bearer_param(header, "realm").unwrap(),
            "https://harbor.example.com/service/token"
        );
        assert_eq!(parse_bearer_param(header, "service").unwrap(), "harbor");
        assert_eq!(
            parse_bearer_param(header, "scope").unwrap(),
            "repository:oaas/app:pull"
        );
    }

    #[test]
    fn parse_bearer_param_missing_returns_err() {
        let result =
            parse_bearer_param("Bearer realm=\"https://host\"", "scope");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fetch_module_oci_scheme_routes_to_oci() {
        // Just verifies routing — the actual network call will fail (no real registry),
        // but the error must NOT be "unsupported URL scheme".
        let err = fetch_module("oci://registry.example.com/repo:tag")
            .await
            .unwrap_err();
        assert!(
            !err.to_string().contains("unsupported"),
            "oci:// should be routed, got: {err}"
        );
    }

    #[tokio::test]
    async fn fetch_module_oci_http_scheme_routes_to_oci() {
        let err = fetch_module("oci+http://registry.example.com/repo:tag")
            .await
            .unwrap_err();
        assert!(
            !err.to_string().contains("unsupported"),
            "oci+http:// should be routed, got: {err}"
        );
    }

    // ─── WorldType detection ────────────────────────────────

    #[tokio::test]
    async fn minimal_component_is_legacy() {
        let store = WasmModuleStore::new(test_engine());
        let bytes = minimal_component_bytes();
        let module = store.load_from_bytes("empty", &bytes).await.unwrap();
        // A bare component with no exports should be detected as Legacy
        assert_eq!(module.world_type, WorldType::Legacy);
    }

    #[test]
    fn detect_world_type_empty_component() {
        let engine = test_engine();
        let bytes = minimal_component_bytes();
        let component = Component::from_binary(&engine, &bytes).unwrap();
        assert_eq!(detect_world_type(&engine, &component), WorldType::Legacy);
    }
}
