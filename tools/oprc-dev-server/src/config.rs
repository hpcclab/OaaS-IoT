use std::collections::HashMap;
use std::path::Path;

use oprc_models::deployment::OClassDeployment;
use oprc_models::enums::ConsistencyModel;
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
///
/// **Primary path** (when `pkg.deployments` is non-empty): mirrors how the
/// real PM handles `POST /api/v1/deployments` — one collection per deployment
/// entry, using the deployment's ODGM infra config and the referenced class's
/// semantic options.
///
/// **Fallback** (when `pkg.deployments` is empty): one collection per class,
/// using defaults. Useful for packages that are published before a deployment
/// is created, or for unit tests.
pub fn package_to_create_requests(
    pkg: &OPackage,
) -> Vec<oprc_grpc::CreateCollectionRequest> {
    if !pkg.deployments.is_empty() {
        pkg.deployments
            .iter()
            .filter_map(|dep| {
                let cls =
                    pkg.classes.iter().find(|c| c.key == dep.class_key)?;
                Some(deployment_to_request(dep, cls, pkg))
            })
            .collect()
    } else {
        // Fallback: iterate classes directly (no deployment section present)
        pkg.classes
            .iter()
            .map(|cls| class_to_request(cls, pkg))
            .collect()
    }
}

/// Build a `CreateCollectionRequest` from a deployment entry.
///
/// Options are merged the same way as the real PM:
///   - `class.options` provides the class-invariant semantic base
///   - `deployment.odgm.options` overlays per-deployment capacity/perf tuning
///   - `invoke_only_primary` is derived from `state_spec.consistency_model`
///   - `shard_type` is derived from `consistency_model` when not explicitly set
///     (dev mode uses "none" as the fallback when `odgm.shard_type` is absent
///     and no consistency model is declared — in-process ODGM doesn't need Raft)
///
/// Note: `odgm.log` and `odgm.env_node_ids` are not applicable in dev mode
/// (single in-process ODGM node); they are silently ignored here.
fn deployment_to_request(
    dep: &OClassDeployment,
    cls: &OClass,
    pkg: &OPackage,
) -> oprc_grpc::CreateCollectionRequest {
    let odgm = dep.odgm.as_ref();

    // Collection name: use first explicit name from deployment, or default FQ key.
    // This matches the PM default: `odgm.collections = ["{package}.{class}"]`.
    let name = odgm
        .and_then(|o| o.collections.first())
        .cloned()
        .unwrap_or_else(|| format!("{}.{}", pkg.name, cls.key));

    // Infrastructure dimensions from the deployment spec; fall back to dev defaults.
    // CreateCollectionRequest uses i32; OdgmDataSpec uses u32, so cast here.
    let partition_count =
        odgm.and_then(|o| o.partition_count).unwrap_or(1) as i32;
    // In dev mode there is always exactly one node, so cap replicas at 1 regardless
    // of what the deployment requests.
    let replica_count = 1;

    // Derive shard_type from consistency_model when not explicitly set,
    // matching the PM derivation. Dev mode falls back to "none" (not "mst"/"raft")
    // when there is no consistency declaration, since the in-process ODGM
    // does not need cluster replication.
    let consistency = cls.state_spec.as_ref().map(|s| &s.consistency_model);
    let is_strong = matches!(consistency, Some(ConsistencyModel::Strong));
    let shard_type = odgm
        .and_then(|o| o.shard_type.clone())
        .unwrap_or_else(|| {
            match consistency {
                Some(ConsistencyModel::Strong) => "raft",
                Some(_) => "mst",
                None => "none", // dev default: no cluster replication needed
            }
            .to_string()
        });

    // Merge options: class base + deployment overlay (deployment wins on conflict),
    // then inject invoke_only_primary from consistency model if not set explicitly.
    let mut merged_options = cls.options.clone();
    if let Some(o) = odgm {
        merged_options.extend(o.options.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if !merged_options.contains_key("invoke_only_primary") {
        merged_options.insert(
            "invoke_only_primary".into(),
            if is_strong { "true" } else { "false" }.into(),
        );
    }

    let invocations = build_invocations(cls, pkg);

    oprc_grpc::CreateCollectionRequest {
        name,
        partition_count,
        replica_count,
        shard_assignments: vec![],
        shard_type,
        options: merged_options,
        invocations,
    }
}

/// Build a `CreateCollectionRequest` from a class directly (fallback path).
fn class_to_request(
    cls: &OClass,
    pkg: &OPackage,
) -> oprc_grpc::CreateCollectionRequest {
    let invocations = build_invocations(cls, pkg);

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

/// Resolve function bindings on a class into an `InvocationRoute` map.
fn build_invocations(
    cls: &OClass,
    pkg: &OPackage,
) -> Option<oprc_grpc::InvocationRoute> {
    let mut fn_routes = HashMap::new();

    for binding in &cls.function_bindings {
        // Look up the OFunction by key to get provision_config
        let wasm_url = pkg
            .functions
            .iter()
            .find(|f| f.key == binding.function_key)
            .and_then(|f| f.provision_config.as_ref())
            .and_then(|pc| pc.wasm_module_url.as_deref())
            .map(resolve_wasm_url);

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

    if fn_routes.is_empty() {
        None
    } else {
        Some(oprc_grpc::InvocationRoute {
            fn_routes,
            disabled_fn: vec![],
        })
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
