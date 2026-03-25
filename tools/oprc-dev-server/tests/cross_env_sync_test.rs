//! Integration test: verify that objects replicate between environments
//! when `consistency_model: READ_YOUR_WRITE` (shard_type = "mst").
//!
//! This test reproduces the bug where each environment's Zenoh session is
//! completely isolated, preventing MST replication from working.

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use oprc_dev_server::config::{
    DevServerConfig, create_requests_for_env, extract_env_names,
};
use oprc_models::deployment::OClassDeployment;
use oprc_models::enums::ConsistencyModel;
use oprc_models::package::{OClass, OPackage, StateSpecification};
use tower::ServiceExt;

/// Build a minimal OPackage with two target envs and READ_YOUR_WRITE consistency.
fn make_sync_test_package() -> OPackage {
    OPackage {
        name: "sync-test".into(),
        classes: vec![OClass {
            key: "counter".into(),
            description: None,
            state_spec: Some(StateSpecification {
                consistency_model: ConsistencyModel::ReadYourWrite,
                ..Default::default()
            }),
            options: HashMap::new(),
            function_bindings: vec![],
        }],
        functions: vec![],
        deployments: vec![OClassDeployment {
            key: "counter".into(),
            package_name: "sync-test".into(),
            class_key: "counter".into(),
            target_envs: vec!["cloud".into(), "edge".into()],
            ..Default::default()
        }],
        version: None,
        metadata: Default::default(),
        dependencies: vec![],
    }
}

/// Verify that the config layer correctly derives shard_type = "mst"
/// for READ_YOUR_WRITE consistency model.
#[test]
fn config_derives_mst_shard_type() {
    let pkg = make_sync_test_package();
    let reqs = create_requests_for_env(&pkg, "cloud");
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].shard_type, "mst",
        "READ_YOUR_WRITE should produce shard_type 'mst'"
    );
}

/// End-to-end test: write an object in environment "cloud", verify it
/// replicates to environment "edge" via MST sync.
///
/// With the bug (isolated Zenoh sessions), the GET from edge returns 404.
/// After the fix (environments connected through transport proxies), the
/// GET should return the object.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore] // integration test — requires real ODGM + Zenoh
async fn cross_env_object_replication() {
    // Construct config
    let pkg = make_sync_test_package();
    let config = DevServerConfig {
        port: 19000, // won't actually bind
        package: pkg,
    };

    let env_names = extract_env_names(&config.package);
    assert_eq!(env_names, vec!["cloud", "edge"]);

    // Start dev environments (uses the real init_environment path)
    let (envs, _net_state) = oprc_dev_server::start_environments(&config)
        .await
        .expect("failed to start environments");
    assert_eq!(envs.len(), 2);

    let cloud_router = &envs[0].gateway_router;
    let edge_router = &envs[1].gateway_router;

    // PUT an object into the cloud env
    let obj_json = serde_json::json!({
        "metadata": {
            "cls_id": "sync-test.counter",
            "partition_id": 0,
            "object_id": "test-obj-1"
        },
        "entries": {
            "value": {
                "data": "AQAAAA==",
                "type": 0
            }
        }
    });

    let put_resp = cloud_router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/class/sync-test.counter/0/objects/test-obj-1")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&obj_json).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let put_status = put_resp.status();
    if put_status != StatusCode::OK {
        let body = axum::body::to_bytes(put_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        panic!(
            "PUT to cloud failed with {}: {}",
            put_status,
            String::from_utf8_lossy(&body)
        );
    }

    // Verify object exists locally on cloud
    let get_cloud = cloud_router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/class/sync-test.counter/0/objects/test-obj-1")
                .header("accept", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        get_cloud.status(),
        StatusCode::OK,
        "GET from cloud should succeed (local read)"
    );

    // Wait for MST replication (MST sync interval is ~5 seconds by default,
    // but the initial page publication may happen sooner).
    // We poll with a timeout to avoid flaky timing.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut replicated = false;
    while tokio::time::Instant::now() < deadline {
        let get_edge = edge_router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/class/sync-test.counter/0/objects/test-obj-1")
                    .header("accept", "application/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if get_edge.status() == StatusCode::OK {
            replicated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        replicated,
        "Object should replicate from cloud to edge via MST"
    );

    // Cleanup: close ODGMs
    for env in &envs {
        env.odgm.close().await;
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// PUT a JSON object to the given gateway router.
async fn put_object(
    router: &axum::Router,
    cls: &str,
    oid: &str,
    entries_json: serde_json::Value,
) -> StatusCode {
    let obj_json = serde_json::json!({
        "metadata": {
            "cls_id": cls,
            "partition_id": 0,
            "object_id": oid
        },
        "entries": entries_json
    });
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/class/{}/0/objects/{}", cls, oid))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&obj_json).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    if status != StatusCode::OK {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        eprintln!(
            "PUT {} failed {}: {}",
            oid,
            status,
            String::from_utf8_lossy(&body)
        );
    }
    status
}

/// GET a JSON object from the given gateway.  Returns `None` on 404.
async fn get_object(
    router: &axum::Router,
    cls: &str,
    oid: &str,
) -> Option<serde_json::Value> {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/class/{}/0/objects/{}", cls, oid))
                .header("accept", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    if resp.status() != StatusCode::OK {
        return None;
    }
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).ok()
}

/// Poll until a GET returns Some or deadline expires.
async fn wait_for_object(
    router: &axum::Router,
    cls: &str,
    oid: &str,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if get_object(router, cls, oid).await.is_some() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

// ── Partition → divergent writes → heal → merge test ────────────────

/// 1. Create envs, sync a baseline object to both sides.
/// 2. Partition the network.
/// 3. Write different objects to each side during partition.
/// 4. Heal the network.
/// 5. Verify both sides eventually have ALL objects (merge).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore] // integration test
async fn partition_heal_merges_divergent_writes() {
    let pkg = make_sync_test_package();
    let config = DevServerConfig {
        port: 19100,
        package: pkg,
    };

    let (envs, net_state) = oprc_dev_server::start_environments(&config)
        .await
        .expect("failed to start environments");
    assert_eq!(envs.len(), 2);

    let cls = "sync-test.counter";
    let cloud = &envs[0].gateway_router;
    let edge = &envs[1].gateway_router;
    let entries = serde_json::json!({"v": {"data": "AA==", "type": 0}});

    // ── Step 1: Write a baseline object and wait for replication ──
    assert_eq!(
        put_object(cloud, cls, "baseline", entries.clone()).await,
        StatusCode::OK
    );
    assert!(
        wait_for_object(edge, cls, "baseline", Duration::from_secs(30)).await,
        "Baseline object should replicate before partition"
    );

    // ── Step 2: Partition the network ──
    {
        let mut links = net_state.links_for_test().await;
        for link in links.values_mut() {
            link.connected = false;
        }
    }
    // Give Zenoh time to notice the transport is down.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // ── Step 3: Write divergent objects during partition ──
    assert_eq!(
        put_object(cloud, cls, "cloud-only", entries.clone()).await,
        StatusCode::OK,
    );
    assert_eq!(
        put_object(edge, cls, "edge-only", entries.clone()).await,
        StatusCode::OK,
    );

    // Verify isolation: cloud-only should NOT appear on edge (and vice-versa)
    // within a short window.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        get_object(edge, cls, "cloud-only").await.is_none(),
        "cloud-only must NOT replicate while partitioned"
    );
    assert!(
        get_object(cloud, cls, "edge-only").await.is_none(),
        "edge-only must NOT replicate while partitioned"
    );

    // ── Step 4: Heal the network ──
    {
        let mut links = net_state.links_for_test().await;
        for link in links.values_mut() {
            link.connected = true;
        }
    }

    // ── Step 5: Verify that BOTH divergent objects merge to both sides ──
    assert!(
        wait_for_object(edge, cls, "cloud-only", Duration::from_secs(30)).await,
        "cloud-only should replicate to edge after heal"
    );
    assert!(
        wait_for_object(cloud, cls, "edge-only", Duration::from_secs(30)).await,
        "edge-only should replicate to cloud after heal"
    );

    // Cleanup
    for env in &envs {
        env.odgm.close().await;
    }
}
