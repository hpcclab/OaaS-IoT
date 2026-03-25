//! Integration tests for the transport-level TCP proxy.
//!
//! These tests use real Zenoh sessions connected through a [`TransportProxy`]
//! to verify that:
//! - Pub/sub messages flow through the proxy when the link is healthy.
//! - Messages stop flowing when the link is partitioned.
//! - Messages resume after the link is healed.
//! - Latency injection delays message delivery.
//!
//! Each test creates isolated Zenoh sessions with `auto_connect = false` and
//! multicast/gossip disabled so they only communicate through the proxy.

use std::time::{Duration, Instant};

use oprc_dev_server::network_sim::NetworkSimState;
use oprc_dev_server::zenoh_bridge::{TransportProxy, find_free_port};
use tokio::time::timeout;

/// Create a Zenoh config that listens on `port` and optionally connects to
/// the given peer endpoints. Uses `tcp/127.0.0.1` like the working ZRPC tests.
fn make_zenoh_config(
    listen_port: u16,
    connect_endpoints: &[String],
) -> zenoh::Config {
    let mut cfg = zenoh::Config::default();
    let listen_ep = format!("tcp/127.0.0.1:{}", listen_port);
    cfg.insert_json5("listen/endpoints", &format!(r#"["{}"]"#, listen_ep))
        .unwrap();
    // Disable scouting — we rely exclusively on explicit connect endpoints.
    cfg.insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    if !connect_endpoints.is_empty() {
        let eps: Vec<String> = connect_endpoints
            .iter()
            .map(|e| format!(r#""{}""#, e))
            .collect();
        cfg.insert_json5("connect/endpoints", &format!("[{}]", eps.join(",")))
            .unwrap();
    }
    cfg
}

/// Spin up two sessions (A listens, B connects through proxy) and return
/// (session_a, session_b, proxy).
async fn setup_pair(
    net_state: &NetworkSimState,
) -> (zenoh::Session, zenoh::Session, TransportProxy) {
    let port_a = find_free_port();

    // A must be listening BEFORE the proxy target is used,
    // so create session A first.
    let cfg_a = make_zenoh_config(port_a, &[]);
    let session_a = zenoh::open(cfg_a).await.expect("open session A");

    // Small delay to ensure the listener is ready.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let proxy = TransportProxy::start(
        "cloud".into(),
        "edge".into(),
        port_a,
        net_state.clone(),
    )
    .await
    .expect("start proxy");

    let peers_b = vec![format!("tcp/127.0.0.1:{}", proxy.listen_port)];
    let cfg_b = make_zenoh_config(find_free_port(), &peers_b);
    let session_b = zenoh::open(cfg_b).await.expect("open session B");

    // Give Zenoh time to establish the transport through the proxy.
    tokio::time::sleep(Duration::from_secs(2)).await;

    (session_a, session_b, proxy)
}

// -----------------------------------------------------------------------
// Baseline: direct Zenoh peer connection (no proxy)
// -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn baseline_direct_zenoh_connection() {
    let port_a = find_free_port();

    let cfg_a = make_zenoh_config(port_a, &[]);
    let session_a = zenoh::open(cfg_a).await.expect("open A");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let peers = vec![format!("tcp/127.0.0.1:{}", port_a)];
    let cfg_b = make_zenoh_config(find_free_port(), &peers);
    let session_b = zenoh::open(cfg_b).await.expect("open B");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sub = session_a
        .declare_subscriber("baseline/test")
        .await
        .expect("subscriber");

    // Allow subscription info to propagate from A to B.
    tokio::time::sleep(Duration::from_millis(500)).await;

    session_b.put("baseline/test", "direct").await.expect("put");

    let sample = timeout(Duration::from_secs(5), sub.recv_async())
        .await
        .expect("direct connection should work")
        .expect("recv");

    let payload: String =
        sample.payload().try_to_string().unwrap().into_owned();
    assert_eq!(payload, "direct");
}

// -----------------------------------------------------------------------
// Pub/sub through proxy
// -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pubsub_flows_through_proxy() {
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);

    let (session_a, session_b, _proxy) = setup_pair(&net_state).await;

    // Subscribe on session A.
    let sub = session_a
        .declare_subscriber("test/hello")
        .await
        .expect("declare subscriber");

    // Allow subscription info to propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish from session B.
    session_b.put("test/hello", "world").await.expect("put");

    // Should receive the message through the proxy.
    let sample = timeout(Duration::from_secs(5), sub.recv_async())
        .await
        .expect("recv timeout")
        .expect("recv error");

    let payload: String =
        sample.payload().try_to_string().unwrap().into_owned();
    assert_eq!(payload, "world");
}

// -----------------------------------------------------------------------
// Partition blocks messages
// -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partition_blocks_messages() {
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);

    let (session_a, session_b, _proxy) = setup_pair(&net_state).await;

    // Subscribe on A.
    let sub = session_a
        .declare_subscriber("test/blocked")
        .await
        .expect("subscriber");

    // Allow subscription info to propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Partition the link.
    {
        let mut links = net_state.links_for_test().await;
        if let Some(link) = links.values_mut().next() {
            link.connected = false;
        }
    }
    // Wait for the partition monitor to tear down the existing connection.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Publish from B — should NOT arrive.
    session_b.put("test/blocked", "nope").await.expect("put");

    let result = timeout(Duration::from_secs(1), sub.recv_async()).await;
    assert!(
        result.is_err(),
        "message should not arrive when link is partitioned"
    );
}

// -----------------------------------------------------------------------
// Heal restores connectivity
// -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heal_restores_connectivity() {
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);

    let (session_a, session_b, _proxy) = setup_pair(&net_state).await;

    // Partition.
    {
        let mut links = net_state.links_for_test().await;
        if let Some(link) = links.values_mut().next() {
            link.connected = false;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Heal.
    {
        let mut links = net_state.links_for_test().await;
        if let Some(link) = links.values_mut().next() {
            link.connected = true;
        }
    }

    // Wait for Zenoh to re-establish the transport.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let sub = session_a
        .declare_subscriber("test/healed")
        .await
        .expect("subscriber");

    // Allow subscription info to propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    session_b.put("test/healed", "back").await.expect("put");

    let sample = timeout(Duration::from_secs(5), sub.recv_async())
        .await
        .expect("message should arrive after heal")
        .expect("recv error");

    let payload: String =
        sample.payload().try_to_string().unwrap().into_owned();
    assert_eq!(payload, "back");
}

// -----------------------------------------------------------------------
// Latency injection
// -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latency_injection_delays_messages() {
    let env_names: Vec<String> = vec!["cloud".into(), "edge".into()];
    let net_state = NetworkSimState::new(&env_names);

    let (session_a, session_b, _proxy) = setup_pair(&net_state).await;

    // Inject 200ms latency.
    {
        let mut links = net_state.links_for_test().await;
        if let Some(link) = links.values_mut().next() {
            link.latency_ms = 200;
        }
    }

    let sub = session_a
        .declare_subscriber("test/latency")
        .await
        .expect("subscriber");

    // Allow subscription info to propagate.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let start = Instant::now();
    session_b.put("test/latency", "delayed").await.expect("put");

    let _sample = timeout(Duration::from_secs(5), sub.recv_async())
        .await
        .expect("recv timeout")
        .expect("recv error");

    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(150),
        "expected at least ~200ms latency, got {:?}",
        elapsed,
    );
}
