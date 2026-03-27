use anyhow::Result;
use std::sync::Arc;

use crate::{
    config::AppConfig,
    crm::CrmManager,
    server::ApiServer,
    services::{
        DeploymentService, PackageService, ScriptService,
        artifact::FsArtifactStore,
        compiler::{CompilerClient, CompilerConfig},
    },
    storage::create_storage_factory,
};
use oprc_cp_storage::traits::StorageFactory;
use tracing::info;

/// Build a fully-wired ApiServer from environment variables.
/// Mirrors the logic in bin/main and is useful for tests and embedding.
pub async fn build_api_server_from_env() -> Result<ApiServer> {
    let config = AppConfig::load_from_env()?;

    // Storage factory and storages
    let storage_config = config.storage();
    let storage_factory = create_storage_factory(&storage_config).await?;
    let package_storage = Arc::new(storage_factory.create_package_storage());
    let deployment_storage =
        Arc::new(storage_factory.create_deployment_storage());

    // CRM
    let crm_manager = Arc::new(CrmManager::new(config.crm())?);

    // Services
    let deployment_service = Arc::new(DeploymentService::new(
        deployment_storage.clone(),
        crm_manager.clone(),
        config.deployment_policy(),
    ));

    let package_service = Arc::new(PackageService::new(
        package_storage.clone(),
        deployment_service.clone(),
        config.deployment_policy(),
    ));

    // Script service (optional — requires OPRC_COMPILER_URL)
    let (artifact_store, source_store, script_service) =
        if let Some(compiler_cfg) = config.compiler() {
            let artifact_cfg = config.artifact();
            info!(
                compiler_url = %compiler_cfg.url,
                artifact_dir = %artifact_cfg.dir,
                "Script service enabled"
            );

            let fs_store =
                Arc::new(FsArtifactStore::new(&artifact_cfg.dir).await?);
            let compiler = Arc::new(CompilerClient::new(CompilerConfig {
                url: compiler_cfg.url,
                timeout_seconds: compiler_cfg.timeout_seconds,
                max_retries: compiler_cfg.max_retries,
            }));

            let script_svc = Arc::new(ScriptService::new(
                compiler,
                fs_store.clone(),
                fs_store.clone(),
                package_service.clone(),
                deployment_service.clone(),
                artifact_cfg.base_url,
            ));

            (
                Some(fs_store.clone()
                    as Arc<dyn crate::services::artifact::ArtifactStore>),
                Some(
                    fs_store as Arc<dyn crate::services::artifact::SourceStore>,
                ),
                Some(script_svc),
            )
        } else {
            info!("Script service disabled (OPRC_COMPILER_URL not set)");
            (None, None, None)
        };

    // Server
    let server_config = config.server();
    let gateway_config = config.gateway();
    #[cfg(feature = "network-sim")]
    let crm_manager_for_netsim = crm_manager.clone();

    #[allow(unused_mut)]
    let mut server = ApiServer::with_all(
        package_service,
        deployment_service,
        crm_manager,
        server_config,
        gateway_config,
        artifact_store,
        source_store,
        script_service,
    );

    // Network simulation (optional — requires feature + OPRC_NETSIM_ENABLED)
    #[cfg(feature = "network-sim")]
    {
        server = setup_netsim(server, crm_manager_for_netsim).await?;
    }

    Ok(server)
}

/// Conditionally wire up network simulation if `OPRC_NETSIM_ENABLED=true`.
///
/// Uses the existing CRM gRPC connections — no separate Zenoh session needed.
/// Each CRM forwards netsim commands to its co-deployed router via ZRPC.
///
/// Reads env vars:
/// - `OPRC_NETSIM_ENABLED` — `"true"` to activate
#[cfg(feature = "network-sim")]
async fn setup_netsim(
    server: ApiServer,
    crm_manager: Arc<CrmManager>,
) -> Result<ApiServer> {
    let enabled = std::env::var("OPRC_NETSIM_ENABLED")
        .unwrap_or_default()
        .eq_ignore_ascii_case("true");
    if !enabled {
        return Ok(server);
    }

    let clusters = crm_manager.list_clusters().await;
    info!(clusters = ?clusters, "Setting up network simulation control via CRM gRPC");

    let manager =
        Arc::new(crate::services::netsim::NetsimManager::new(crm_manager));

    Ok(server.merge_netsim(manager))
}
