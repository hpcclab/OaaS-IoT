//! Transport-level TCP proxy between dev-server environment Zenoh sessions.
//!
//! Works like [toxiproxy](https://github.com/Shopify/toxiproxy): sits between
//! two Zenoh sessions at the TCP layer and can inject faults (partition, latency)
//! to simulate real network conditions. Because it operates on raw bytes,
//! **all** Zenoh traffic (pub/sub, queryable/ZRPC, admin, etc.) is captured
//! automatically — no application-level knowledge of message types is needed.
//!
//! For each environment pair (A, B) one [`TransportProxy`] is created:
//! - A's Zenoh session listens on a known TCP port.
//! - The proxy listens on a separate port and forwards connections to A.
//! - B's Zenoh session is configured to connect to the proxy port.
//! - All A↔B traffic flows through the proxy.
//!
//! Partition and latency injection are controlled via [`NetworkSimState`].

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::network_sim::NetworkSimState;

/// A TCP proxy sitting between two environment Zenoh sessions.
///
/// The proxy listens on [`listen_port`](Self::listen_port) and forwards every
/// accepted connection to `target_port` (the "listener" environment's Zenoh
/// endpoint). The connection is kept alive as long as the link is healthy;
/// when a partition is detected the connection is torn down, causing Zenoh to
/// notice the lost transport and retry later.
pub struct TransportProxy {
    pub env_a: String,
    pub env_b: String,
    /// Port the proxy listens on (the "connector" env connects here).
    pub listen_port: u16,
    cancel: CancellationToken,
}

impl TransportProxy {
    /// Start the TCP proxy.
    ///
    /// * `env_a` / `env_b` — environment names (for [`NetworkSimState`] lookup).
    /// * `target_port` — the Zenoh listen port of the "listener" env.
    /// * `net_state` — shared partition/latency state.
    pub async fn start(
        env_a: String,
        env_b: String,
        target_port: u16,
        net_state: NetworkSimState,
    ) -> anyhow::Result<Self> {
        let cancel = CancellationToken::new();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let listen_port = listener.local_addr()?.port();

        tokio::spawn(accept_loop(
            env_a.clone(),
            env_b.clone(),
            listener,
            target_port,
            net_state,
            cancel.clone(),
        ));

        info!(
            env_a = %env_a, env_b = %env_b,
            proxy = listen_port, target = target_port,
            "Transport proxy started"
        );

        Ok(Self {
            env_a,
            env_b,
            listen_port,
            cancel,
        })
    }

    /// Shut down the proxy and all active connections.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for TransportProxy {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

// ---------------------------------------------------------------------------
// Accept loop
// ---------------------------------------------------------------------------

async fn accept_loop(
    env_a: String,
    env_b: String,
    listener: TcpListener,
    target_port: u16,
    net_state: NetworkSimState,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = listener.accept() => {
                let (client, _addr) = match result {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(env_a = %env_a, env_b = %env_b, "proxy accept error: {e}");
                        continue;
                    }
                };

                // If the link is currently partitioned, refuse the connection
                // immediately so Zenoh notices the transport is down.
                if !net_state.is_link_active(&env_a, &env_b).await {
                    drop(client);
                    continue;
                }

                // Connect to the target Zenoh listener.
                let target = match TcpStream::connect(("127.0.0.1", target_port)).await {
                    Ok(t) => t,
                    Err(e) => {
                        warn!(target = target_port, "proxy → target connect failed: {e}");
                        continue;
                    }
                };

                let env_a = env_a.clone();
                let env_b = env_b.clone();
                let net_state = net_state.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    handle_connection(
                        client,
                        target,
                        &env_a,
                        &env_b,
                        &net_state,
                        &cancel,
                    )
                    .await;
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-connection handler
// ---------------------------------------------------------------------------

/// Pipe bytes between `client` and `target` while monitoring for partition.
async fn handle_connection(
    client: TcpStream,
    target: TcpStream,
    env_a: &str,
    env_b: &str,
    net_state: &NetworkSimState,
    cancel: &CancellationToken,
) {
    let (client_r, client_w) = client.into_split();
    let (target_r, target_w) = target.into_split();

    let conn_cancel = CancellationToken::new();

    // Partition monitor: poll link state and tear down if partitioned.
    let monitor = {
        let env_a = env_a.to_string();
        let env_b = env_b.to_string();
        let ns = net_state.clone();
        let cc = conn_cancel.clone();
        let gc = cancel.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    _ = gc.cancelled() => { cc.cancel(); break; }
                    _ = cc.cancelled() => break,
                    _ = interval.tick() => {
                        if !ns.is_link_active(&env_a, &env_b).await {
                            debug!(env_a = %env_a, env_b = %env_b, "partition detected — tearing down proxy connection");
                            cc.cancel();
                            break;
                        }
                    }
                }
            }
        })
    };

    // Bidirectional byte pump.
    let pump_c2t =
        pump(client_r, target_w, env_a, env_b, net_state, &conn_cancel);
    let pump_t2c =
        pump(target_r, client_w, env_a, env_b, net_state, &conn_cancel);

    tokio::select! {
        _ = pump_c2t => {}
        _ = pump_t2c => {}
        _ = conn_cancel.cancelled() => {}
    }

    conn_cancel.cancel();
    monitor.abort();
}

// ---------------------------------------------------------------------------
// Byte pump with latency injection
// ---------------------------------------------------------------------------

async fn pump(
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    env_a: &str,
    env_b: &str,
    net_state: &NetworkSimState,
    cancel: &CancellationToken,
) {
    let mut buf = vec![0u8; 16_384];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = reader.read(&mut buf) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let latency = net_state.link_latency_ms(env_a, env_b).await;
                        if latency > 0 {
                            tokio::time::sleep(Duration::from_millis(latency)).await;
                        }
                        if writer.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// Find an available TCP port on localhost (bind + release).
pub fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind to find free port");
    listener.local_addr().expect("local_addr").port()
}
