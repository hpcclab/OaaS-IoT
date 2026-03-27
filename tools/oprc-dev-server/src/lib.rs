pub mod config;
mod frontend;
pub mod network_sim;
pub mod stub_api;
pub mod zenoh_bridge;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use config::{DevServerConfig, create_requests_for_env, extract_env_names};
use network_sim::NetworkSimState;
use oprc_netsim::LinkChecker;
use oprc_odgm::{EventPipelineConfig, ObjectDataGridManager, OdgmConfig};
use oprc_zenoh::pool::Pool;
use std::time::Duration as StdDuration;
use tower_http::cors::CorsLayer;
use tracing::info;
use zenoh_bridge::{TransportProxy, find_free_port};

/// A running simulated environment (gateway + ODGM).
pub struct DevEnvironment {
    pub name: String,
    pub odgm: Arc<ObjectDataGridManager>,
    #[allow(dead_code)]
    pub session_pool: Pool,
    pub gateway_router: Router,
    /// The dedicated port this environment's gateway listens on.
    pub gateway_port: u16,
    /// Transport proxies connecting this env to others (kept alive).
    #[allow(dead_code)]
    _proxies: Vec<TransportProxy>,
}

/// Start the dev server with the given configuration.
pub async fn start(config: DevServerConfig) -> anyhow::Result<()> {
    let port = config.port;
    let env_names = extract_env_names(&config.package);
    let multi_env = env_names.len() > 1;

    // Network simulation state (shared across all envs)
    let net_state = NetworkSimState::new(&env_names);

    // ── Environment initialisation ──────────────────────────────────
    //
    // Each environment runs as an independent single-node ODGM cluster
    // with its own Zenoh session. When multiple environments exist,
    // their Zenoh sessions are connected through TCP transport proxies
    // so that MST replication (for READ_YOUR_WRITE / Strong consistency)
    // can sync data across environments. The proxies also enable
    // network partition simulation via the debug API.
    let envs = start_environments_with_net(&config, &net_state).await?;

    // Build the PM / management HTTP router (no gateway routes)
    let router =
        build_dev_router(&config, &envs, multi_env, &net_state).await?;

    // Collect ODGMs for graceful shutdown
    let odgms: Vec<Arc<ObjectDataGridManager>> =
        envs.iter().map(|e| e.odgm.clone()).collect();

    // Start a dedicated gateway listener per environment
    for env in &envs {
        let gw_router = env.gateway_router.clone().layer(
            CorsLayer::very_permissive().max_age(StdDuration::from_secs(3600)),
        );
        let gw_socket = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            env.gateway_port,
        );
        let gw_listener = tokio::net::TcpListener::bind(gw_socket).await?;
        let env_name = env.name.clone();
        tokio::spawn(async move {
            info!(env = %env_name, port = gw_socket.port(), "Gateway listening");
            if let Err(e) = axum::serve(gw_listener, gw_router).await {
                tracing::error!(env = %env_name, error = %e, "Gateway server error");
            }
        });
    }

    // Serve the PM / management API
    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let listener = tokio::net::TcpListener::bind(socket).await?;
    print_banner(&config, port, &envs);
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal(odgms))
        .await?;

    Ok(())
}

/// Initialise a single simulated environment: Zenoh session → ODGM → Gateway.
async fn init_environment(
    config: &DevServerConfig,
    env_name: &str,
    node_id: u64,
    gateway_port: u16,
    listen_port: u16,
    peers: &[String],
) -> anyhow::Result<DevEnvironment> {
    // Each env gets its own Zenoh session.
    // auto_connect is disabled to prevent the Pool's GLOBAL_PORTS
    // mechanism from connecting sessions across environments — instead,
    // cross-env connectivity is handled via explicit transport proxies
    // that allow network partition simulation.
    // scouting/gossip disabled so envs only communicate through the
    // configured peer endpoints (proxy ports).
    let mut z_config = oprc_zenoh::OprcZenohConfig::default();
    z_config.auto_connect = false;
    z_config.scouting_multicast_enabled = Some(false);
    z_config.gossip_enabled = Some(false);
    z_config.zenoh_port = listen_port;
    if !peers.is_empty() {
        z_config.peers = Some(peers.join(","));
    }

    // Enable ODGM → Zenoh event publishing so the Gateway WebSocket bridge
    // receives state-change events.  ODGM and Gateway share the same process,
    // so SessionLocal locality keeps events in-process without network overhead.
    let event_pipeline_config = EventPipelineConfig::with_local_publish();

    // Each env is a single-node ODGM cluster (members = just itself).
    // This matches production where each environment's ODGM cluster is
    // independent.
    let odgm_config = OdgmConfig {
        http_port: config.port,
        node_id: Some(node_id),
        members: Some(node_id.to_string()),
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

    // Build gateway router for this env's Zenoh session
    let z_session = session_pool
        .get_session()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let gateway_router = oprc_gateway::build_router(
        z_session,
        Duration::from_secs(30),
        true, // Enable WebSocket event subscriptions
    );

    Ok(DevEnvironment {
        name: env_name.to_string(),
        odgm,
        session_pool,
        gateway_router,
        gateway_port,
        _proxies: Vec::new(),
    })
}

/// Start all environments with transport proxies for cross-env Zenoh
/// connectivity and network partition simulation.
///
/// This is the internal implementation used by [`start`]. For tests that
/// don't need a custom [`NetworkSimState`], use [`start_environments`].
async fn start_environments_with_net(
    config: &DevServerConfig,
    net_state: &NetworkSimState,
) -> anyhow::Result<Vec<DevEnvironment>> {
    let env_names = extract_env_names(&config.package);
    let multi_env = env_names.len() > 1;

    // Allocate a dedicated Zenoh listen port for each environment.
    let listen_ports: Vec<u16> =
        env_names.iter().map(|_| find_free_port()).collect();

    // Start transport proxies between every pair of environments.
    // For pair (i, j) where i < j, a proxy forwards connections to env i's
    // listen port. Env j will connect to the proxy, establishing a
    // bidirectional Zenoh transport through the proxy.
    //
    // proxy_peers[k] = list of proxy endpoints that env k should connect to.
    let mut proxy_peers: Vec<Vec<String>> = vec![Vec::new(); env_names.len()];
    let mut all_proxies: Vec<Vec<TransportProxy>> =
        env_names.iter().map(|_| Vec::new()).collect();

    if multi_env {
        let checker: Arc<dyn LinkChecker> = Arc::new(net_state.clone());
        for i in 0..env_names.len() {
            for j in (i + 1)..env_names.len() {
                let proxy = TransportProxy::start_local(
                    env_names[i].clone(),
                    env_names[j].clone(),
                    listen_ports[i],
                    checker.clone(),
                )
                .await?;
                // Env j connects to this proxy to reach env i.
                // The Zenoh transport is bidirectional once established.
                proxy_peers[j]
                    .push(format!("tcp/127.0.0.1:{}", proxy.listen_port));
                all_proxies[j].push(proxy);
            }
        }
    }

    // Create environments in order. Earlier envs are already listening
    // when later envs connect through the proxies.
    let mut envs: Vec<DevEnvironment> = Vec::new();
    for (idx, env_name) in env_names.iter().enumerate() {
        let gateway_port = config.port + 1 + idx as u16;
        info!(env = %env_name, gateway_port, zenoh_port = listen_ports[idx],
              peers = ?proxy_peers[idx], "Initialising environment");
        let mut dev_env = init_environment(
            config,
            env_name,
            idx as u64 + 1,
            gateway_port,
            listen_ports[idx],
            &proxy_peers[idx],
        )
        .await?;
        // Attach proxies to keep them alive for the lifetime of the env.
        dev_env._proxies = std::mem::take(&mut all_proxies[idx]);
        create_collections_and_wait(
            config,
            env_name,
            &dev_env.odgm,
            idx as u64 + 1,
        )
        .await?;
        envs.push(dev_env);
    }

    // Wait for Zenoh transports to establish through the proxies.
    if multi_env {
        info!("Waiting for cross-env Zenoh transports to establish...");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Ok(envs)
}

/// Public entry point for tests: start all environments with cross-env
/// Zenoh connectivity via transport proxies.
/// Returns the environments and the network simulation state for partition control.
pub async fn start_environments(
    config: &DevServerConfig,
) -> anyhow::Result<(Vec<DevEnvironment>, NetworkSimState)> {
    let env_names = extract_env_names(&config.package);
    let net_state = NetworkSimState::new(&env_names);
    let envs = start_environments_with_net(config, &net_state).await?;
    Ok((envs, net_state))
}

/// Create collections in the given ODGM and wait for shards to become ready.
///
/// `node_id` is used to generate globally-unique shard IDs so MST replication
/// can distinguish publications from different environments.
async fn create_collections_and_wait(
    config: &DevServerConfig,
    env_name: &str,
    odgm: &ObjectDataGridManager,
    node_id: u64,
) -> anyhow::Result<()> {
    let create_requests = create_requests_for_env(&config.package, env_name);
    for req in &create_requests {
        // Inject explicit shard_assignments so each env's shards have unique
        // IDs across environments.  MST uses shard_id as the owner identity
        // in the "skip self" check — if two independent ODGMs both generate
        // shard_id=1 they will ignore each other's publications.
        let mut req = req.clone();
        if req.shard_assignments.is_empty() {
            let partition_count = req.partition_count.max(1) as usize;
            let mut assignments = Vec::with_capacity(partition_count);
            for _ in 0..partition_count {
                assignments.push(oprc_grpc::ShardAssignment {
                    primary: Some(node_id),
                    replica: vec![node_id],
                    shard_ids: vec![node_id],
                });
            }
            req.shard_assignments = assignments;
        }
        info!(
            env = %env_name,
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

    // Verify shard readiness
    for req in &create_requests {
        let shards = odgm
            .shard_manager
            .get_shards_for_collection(&req.name)
            .await;
        if let Some(shard) = shards.first() {
            let mut ready_attempts = 0;
            while ready_attempts < 100 {
                if *shard.watch_readiness().borrow() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                ready_attempts += 1;
            }
            info!(
                env = %env_name,
                collection = %req.name,
                shard_id = shard.meta().id,
                ready = *shard.watch_readiness().borrow(),
                "Shard ready"
            );
        } else {
            tracing::warn!(
                env = %env_name,
                collection = %req.name,
                "Shard not found after creation"
            );
        }
    }

    Ok(())
}

pub async fn build_dev_router(
    config: &DevServerConfig,
    envs: &[DevEnvironment],
    _multi_env: bool,
    net_state: &NetworkSimState,
) -> anyhow::Result<Router> {
    let mut router = Router::new();

    // Gateways now listen on their own dedicated ports — no gateway
    // routes are mounted on the PM API port.

    // Debug network API (always available, even in single-env mode)
    let debug_api = network_sim::build_debug_api(net_state.clone());
    let v1_api = network_sim::build_v1_api(net_state.clone());
    router = router.merge(debug_api).merge(v1_api);

    // Stub PM API for frontend (/api/v1/deployments, etc.)
    // Include gateway port mapping so the frontend can discover per-env URLs.
    let env_ports: Vec<(String, u16)> = envs
        .iter()
        .map(|e| (e.name.clone(), e.gateway_port))
        .collect();
    let stub = stub_api::build_stub_api(&config.package, &env_ports);
    router = router.merge(stub);

    // CORS
    router = router.layer(
        CorsLayer::very_permissive()
            // Cache preflight for 1 hour — prevents Chrome from sending
            // an OPTIONS request before every single API call.
            .max_age(StdDuration::from_secs(3600)),
    );

    // Frontend fallback (embedded static files)
    #[cfg(feature = "frontend")]
    {
        router = router.fallback(axum::routing::get(frontend::serve_frontend));
    }

    Ok(router)
}

fn print_banner(config: &DevServerConfig, port: u16, envs: &[DevEnvironment]) {
    println!("╔══════════════════════════════════════════╗");
    println!("║       OaaS Local Dev Server              ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  PM API:  http://localhost:{:<14}║", port);
    for env in envs {
        println!(
            "║  {:7}  http://localhost:{:<14}║",
            env.name, env.gateway_port
        );
    }
    println!("╚══════════════════════════════════════════╝");
    println!("  Package: {}", config.package.name);

    if envs.len() > 1 {
        println!("  Network debug: /api/debug/network");
    }

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

async fn shutdown_signal(odgms: Vec<Arc<ObjectDataGridManager>>) {
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
    for odgm in &odgms {
        odgm.close().await;
    }
}
