pub mod config;
mod frontend;
pub mod stub_api;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use config::{DevServerConfig, package_to_create_requests};
use oprc_odgm::{EventPipelineConfig, ObjectDataGridManager, OdgmConfig};
use oprc_zenoh::pool::Pool;
use std::time::Duration as StdDuration;
use tower_http::cors::CorsLayer;
use tracing::info;

/// Start the dev server with the given configuration.
pub async fn start(config: DevServerConfig) -> anyhow::Result<()> {
    let port = config.port;

    // 1. Create Zenoh session in peer mode (loopback, no external peers)
    let z_config = oprc_zenoh::OprcZenohConfig::default();

    // Enable ODGM → Zenoh event publishing so the Gateway WebSocket bridge
    // receives state-change events.  ODGM and Gateway share the same process,
    // so SessionLocal locality keeps events in-process without network overhead.
    let event_pipeline_config = EventPipelineConfig::with_local_publish();

    // 2. Start ODGM (creates Pool, MetaManager, ShardManager, watch stream)
    let odgm_config = OdgmConfig {
        http_port: port,
        node_id: Some(1),
        members: Some("1".into()),
        ..Default::default()
    };
    let (odgm, session_pool) = oprc_odgm::start_raw_server_with_pipeline(
        &odgm_config,
        Some(z_config),
        Some(event_pipeline_config),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{}", e))?;
    let odgm = Arc::new(odgm);

    // 3. Create collections from OPackage classes
    let create_requests = package_to_create_requests(&config.package);
    for req in &create_requests {
        info!(
            collection = %req.name,
            functions = req.invocations.as_ref().map_or(0, |inv| inv.fn_routes.len()),
            "Creating collection"
        );
        odgm.metadata_manager.create_collection(req.clone()).await?;
    }

    // Wait for shards to be created (poll with timeout).
    // WASM compilation of large modules can take 10+ seconds.
    let expected_shards = create_requests.len() as u32;
    let mut attempts = 0;
    while attempts < 600 {
        let stats = odgm.shard_manager.get_stats().await;
        if stats.total_shards_created >= expected_shards {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        attempts += 1;
    }

    // Verify shards are created and ready
    for cls in &config.package.classes {
        let shards =
            odgm.shard_manager.get_shards_for_collection(&cls.key).await;
        if let Some(shard) = shards.first() {
            // Wait for shard readiness (WASM module loading, etc.)
            let mut ready_attempts = 0;
            while ready_attempts < 100 {
                if *shard.watch_readiness().borrow() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                ready_attempts += 1;
            }
            info!(
                collection = %cls.key,
                shard_id = shard.meta().id,
                ready = *shard.watch_readiness().borrow(),
                "Shard ready"
            );
        } else {
            tracing::warn!(
                collection = %cls.key,
                "Shard not found after creation"
            );
        }
    }

    // 4. Build the HTTP router
    let router = build_dev_router(&config, &session_pool).await?;

    // 5. Serve
    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let listener = tokio::net::TcpListener::bind(socket).await?;
    print_banner(&config, port);
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal(odgm))
        .await?;

    Ok(())
}

async fn build_dev_router(
    config: &DevServerConfig,
    session_pool: &Pool,
) -> anyhow::Result<Router> {
    let z_session = session_pool
        .get_session()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Gateway REST/gRPC routes at /api/class/...
    let gateway = oprc_gateway::build_router(
        z_session.clone(),
        Duration::from_secs(30),
        true, // Enable WebSocket event subscriptions
    );

    // Strip /api/gateway prefix for frontend compatibility:
    // Frontend calls /api/gateway/api/class/... → forward to /api/class/...
    let gateway_proxy = Router::new().nest("/api/gateway", gateway.clone());

    // Stub PM API for frontend (/api/v1/deployments, etc.)
    let stub = stub_api::build_stub_api(&config.package);

    let router = gateway.merge(gateway_proxy).merge(stub).layer(
        CorsLayer::very_permissive()
            // Cache preflight for 1 hour — prevents Chrome from sending
            // an OPTIONS request before every single API call.
            .max_age(StdDuration::from_secs(3600)),
    );

    // Frontend fallback (embedded static files)
    #[cfg(feature = "frontend")]
    let router = router.fallback(axum::routing::get(frontend::serve_frontend));

    Ok(router)
}

fn print_banner(config: &DevServerConfig, port: u16) {
    println!("╔══════════════════════════════════════════╗");
    println!("║       OaaS Local Dev Server              ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  http://localhost:{:<23}║", port);
    println!("╚══════════════════════════════════════════╝");
    println!("  Package: {}", config.package.name);
    for cls in &config.package.classes {
        let fn_names: Vec<&str> = cls
            .function_bindings
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        if fn_names.is_empty() {
            println!("  Class: {} (no functions)", cls.key);
        } else {
            println!("  Class: {} → [{}]", cls.key, fn_names.join(", "));
        }
    }
}

async fn shutdown_signal(odgm: Arc<ObjectDataGridManager>) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
        .expect("failed to install signal handler")
        .recv()
        .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutting down...");
    odgm.close().await;
}
