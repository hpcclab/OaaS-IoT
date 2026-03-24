use std::collections::HashMap;
use std::path::Path;

use oprc_models::package::{OClass, OPackage};

/// Dev server runtime configuration: port + the standard OPackage definition.
#[derive(Debug)]
pub struct DevServerConfig {
    pub port: u16,
    pub package: OPackage,
}

impl DevServerConfig {
    /// Load an OPackage YAML file and wrap it with the given port.
    pub fn from_file(path: &Path, port: u16) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let package: OPackage = serde_yaml::from_str(&contents)?;
        Ok(Self { port, package })
    }
}

/// Convert an OPackage into ODGM `CreateCollectionRequest`s.
/// Each OClass becomes one collection; function bindings are resolved
/// against the package's function list to find WASM module URLs.
pub fn package_to_create_requests(
    pkg: &OPackage,
) -> Vec<oprc_grpc::CreateCollectionRequest> {
    pkg.classes
        .iter()
        .map(|cls| class_to_request(cls, pkg))
        .collect()
}

fn class_to_request(
    cls: &OClass,
    pkg: &OPackage,
) -> oprc_grpc::CreateCollectionRequest {
    let mut fn_routes = HashMap::new();

    for binding in &cls.function_bindings {
        // Look up the OFunction by key to get provision_config
        let wasm_url = pkg
            .functions
            .iter()
            .find(|f| f.key == binding.function_key)
            .and_then(|f| f.provision_config.as_ref())
            .and_then(|pc| pc.wasm_module_url.as_deref())
            .map(|url| resolve_wasm_url(url));

        let wasm_fuel = pkg
            .functions
            .iter()
            .find(|f| f.key == binding.function_key)
            .and_then(|f| f.provision_config.as_ref())
            .and_then(|pc| pc.wasm_fuel);

        fn_routes.insert(
            binding.name.clone(),
            oprc_grpc::FuncInvokeRoute {
                url: format!("wasm://{}", binding.name),
                wasm_module_url: wasm_url,
                wasm_fuel,
                stateless: binding.stateless,
                standby: false,
                active_group: vec![],
            },
        );
    }

    let invocations = if fn_routes.is_empty() {
        None
    } else {
        Some(oprc_grpc::InvocationRoute {
            fn_routes,
            disabled_fn: vec![],
        })
    };

    oprc_grpc::CreateCollectionRequest {
        // Use fully-qualified name to match the PM production flow:
        // PM sets collection name to "{package_name}.{class_key}" so the
        // gateway URL segment /api/class/{cls}/... uses the same FQ key.
        name: format!("{}.{}", pkg.name, cls.key),
        partition_count: 1,
        replica_count: 1,
        shard_assignments: vec![],
        shard_type: "none".to_string(),
        options: cls.options.clone(),
        invocations,
    }
}

/// Resolve a WASM URL: if it looks like a relative file path, convert to
/// an absolute `file://` URL. Otherwise return as-is (http://, oci://, etc.).
fn resolve_wasm_url(url: &str) -> String {
    // Already a full URL scheme
    if url.contains("://") {
        return url.to_string();
    }
    // Treat as a file path
    let path = Path::new(url);
    if path.is_absolute() {
        format!("file://{}", path.display())
    } else {
        let abs = std::env::current_dir().unwrap_or_default().join(path);
        format!("file://{}", abs.display())
    }
}
