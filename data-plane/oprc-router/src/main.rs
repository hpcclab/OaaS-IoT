use std::error::Error;

use envconfig::Envconfig;

#[cfg(feature = "network-sim")]
mod netsim;

pub fn init_log() {
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::{
        EnvFilter, layer::SubscriberExt, util::SubscriberInitExt,
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .with_env_var("OPRC_LOG")
                .from_env_lossy(),
        )
        .init();
}

fn main() {
    let cpus = num_cpus::get();
    let worker_threads = std::cmp::max(1, cpus);
    init_log();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { start().await.unwrap() });
}

async fn start() -> Result<(), Box<dyn Error>> {
    let mut z_conf = oprc_zenoh::OprcZenohConfig::init_from_env()?;
    z_conf.mode = zenoh_config::WhatAmI::Router;

    // Network simulation: intercept peer connections through local proxies.
    #[cfg(feature = "network-sim")]
    let netsim_pre = netsim::pre_session_setup(&z_conf).await?;

    #[cfg(feature = "network-sim")]
    let z_conf = if let Some(ref pre) = netsim_pre {
        let mut patched = z_conf.clone();
        patched.peers = Some(pre.rewritten_peers.join(","));
        patched
    } else {
        z_conf
    };

    let conf = z_conf.create_zenoh();
    #[allow(unused_variables)]
    let session = match zenoh::open(conf).await {
        Ok(runtime) => runtime,
        Err(e) => {
            println!("{e}. Exiting...");
            std::process::exit(-1);
        }
    };

    // Finish netsim setup (ZRPC queryable requires the open session).
    #[cfg(feature = "network-sim")]
    let _netsim = if let Some(pre) = netsim_pre {
        Some(netsim::post_session_setup(pre, &session).await?)
    } else {
        None
    };

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

    Ok(())
}
