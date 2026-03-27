use std::collections::HashMap;

use crate::enums::DeploymentCondition;
use crate::nfr::{NfrRequirements, QosRequirement};
use crate::telemetry::TelemetryConfig;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Validate)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
pub struct OClassDeployment {
    #[validate(length(min = 1, message = "Deployment key cannot be empty"))]
    pub key: String,
    #[validate(length(min = 1, message = "Package name cannot be empty"))]
    pub package_name: String,
    #[validate(length(min = 1, message = "Class key cannot be empty"))]
    pub class_key: String,
    /// Explicit target environments to deploy to. If empty, the system will
    /// select environments automatically based on availability and NFRs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_envs: Vec<String>,
    /// Optional allow-list of environments that are eligible for automatic
    /// selection. If empty, all known environments are considered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_envs: Vec<String>,
    #[validate(nested)]
    #[serde(default)]
    pub nfr_requirements: NfrRequirements,
    /// Per-environment template overrides. Key = environment/cluster name,
    /// Value = template name or alias. Highest precedence for that env.
    #[serde(
        default,
        skip_serializing_if = "std::collections::HashMap::is_empty"
    )]
    pub env_templates: HashMap<String, String>,
    #[validate(nested)]
    #[serde(default)]
    pub functions: Vec<FunctionDeploymentSpec>,
    #[serde(default)]
    pub condition: DeploymentCondition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub odgm: Option<OdgmDataSpec>,
    /// Per-deployment telemetry/observability configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryConfig>,
    /// Optional runtime status summary populated by the Package Manager.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<DeploymentStatusSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Validate, JsonSchema,
)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
pub struct FunctionDeploymentSpec {
    #[validate(length(min = 1, message = "Function key cannot be empty"))]
    pub function_key: String,
    /// Short human-readable description for the function
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional available location / environment name where this function may run (e.g., "edge", "cloud")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_location: Option<String>,
    /// Per-function QoS requirements (inherited from package metadata or analyzer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos_requirement: Option<QosRequirement>,
    /// Optional provision configuration copied from package (container image, ports, knative hints)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision_config: Option<crate::nfr::ProvisionConfig>,
    /// Arbitrary config key/value pairs from package metadata (injected as ENV to runtimes)
    #[serde(
        default,
        skip_serializing_if = "std::collections::HashMap::is_empty"
    )]
    pub config: std::collections::HashMap<String, String>,
}

impl Default for OClassDeployment {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            key: String::new(),
            package_name: String::new(),
            class_key: String::new(),
            target_envs: Vec::new(),
            available_envs: Vec::new(),
            nfr_requirements: NfrRequirements::default(),
            env_templates: HashMap::new(),
            functions: Vec::new(),
            condition: DeploymentCondition::Pending,
            odgm: None,
            telemetry: None,
            status: None,
            created_at: Some(now),
            updated_at: Some(now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
#[derive(Default)]
pub struct DeploymentFilter {
    pub package_name: Option<String>,
    pub class_key: Option<String>,
    pub target_env: Option<String>,
    pub condition: Option<DeploymentCondition>,
}

/// Concrete infrastructure configuration for a specific deployment instance.
///
/// This is the **per-deployment, low-level** counterpart to `OClass.options`:
///
/// | Scope | Field | Purpose |
/// |---|---|---|
/// | Class (`OClass.options`) | `zenoh_event_publish`, etc. | Semantic / behavioral — *what the class does* |
/// | Deployment (`OdgmDataSpec`) | `partition_count`, `shard_type`, `log`, … | Concrete / infrastructure — *how it is deployed* |
///
/// Different deployments of the same class can have different `OdgmDataSpec`
/// values (e.g., one env with `shard_type = "raft"` for strong consistency,
/// another with `shard_type = "mst"` for eventual).
///
/// **`shard_type` note**: This field is intentionally low-level. In most cases
/// users should express their intent via `OClassDeployment.nfr_requirements`
/// (e.g., `consistency = STRONG`) and let the PM derive the appropriate shard
/// type during deployment scheduling.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Validate, Default,
)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
pub struct OdgmDataSpec {
    /// Logical ODGM collection names to materialize. A minimal CreateCollectionRequest will
    /// be generated per name with uniform partition/replica/shard settings.
    /// Defaults to `["{package_name}.{class_key}"]` when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    /// Desired partition count per collection (>=1). Partitions drive parallelism and hash space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_count: Option<u32>,
    /// Desired replica count per partition (>=1). PM selects based on availability NFRs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica_count: Option<u32>,
    /// Low-level shard implementation / consistency strategy (e.g. "mst", "raft").
    /// Prefer using `OClass.state_spec.consistency_model`; the PM will derive this
    /// automatically. Set explicitly only as an advanced override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard_type: Option<String>,
    /// Mapping of environment (target_env) -> list of ODGM node ids assigned for that env.
    /// Populated by the PM during deployment scheduling.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env_node_ids: HashMap<String, Vec<u64>>,
    /// Optional ODGM log env filter (maps to ODGM_LOG), e.g. "info,openraft=info,zenoh=warn".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Per-deployment shard behavior overrides. Keys here have higher precedence than
    /// `OClass.options` and are merged on top at deployment time. Use this for
    /// capacity/performance tuning that varies by environment:
    /// - `batch_size` — operation batch size (default 1000)
    /// - `timeout_ms` — operation timeout in ms (default 5000)
    /// - `offload_max_pool_size` — max function invocation pool size (default 64)
    /// - `pool_max_idle_lifetime` — pool idle timeout in ms (default 30000)
    /// - `pool_max_lifetime` — pool connection refresh interval in ms (default 600000)
    /// - `enable_metrics` — collect shard-level metrics (default "true")
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub options: HashMap<String, String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zenoh_mode: Option<String>,
}

#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema,
)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(export))]
pub struct DeploymentStatusSummary {
    /// Chosen replication factor (number of environments) for this deployment.
    pub replication_factor: u32,
    /// The concrete environments where the deployment is (or will be) placed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_envs: Vec<String>,
    /// Best-effort achieved quorum availability for the selected environments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub achieved_quorum_availability: Option<f64>,
    /// Optional last error observed during scheduling or rollout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}
