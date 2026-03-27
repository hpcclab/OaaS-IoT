pub mod crm_info;
pub mod deployment;
pub mod health;
pub mod helpers;
#[cfg(feature = "network-sim")]
pub mod netsim;
pub mod topology;
pub mod builders {
    pub mod class_runtime;
}

use kube::Client;
use std::sync::Arc;
use tonic::service::Routes;
use tonic_reflection::server::Builder as ReflectionBuilder;
use zenoh::Session;

use crm_info::CrmInfoSvc;
use deployment::DeploymentSvc;
use health::HealthSvc;
use oprc_grpc::proto::deployment::deployment_service_server::DeploymentServiceServer;
use topology::TopologySvc;

pub async fn build_grpc_routes(
    client: Client,
    default_namespace: String,
    zenoh: Arc<Session>,
) -> Routes {
    let reflection = ReflectionBuilder::configure().build_v1().ok();

    let health = HealthSvc;
    let crm_info = CrmInfoSvc::new(client.clone(), default_namespace.clone());
    let topology = TopologySvc::new(zenoh.clone());

    let deploy_svc = DeploymentSvc {
        client,
        default_namespace,
    };
    let tonic_deploy = DeploymentServiceServer::new(deploy_svc);

    let mut routes = Routes::new(
        oprc_grpc::proto::health::health_service_server::HealthServiceServer::new(
            health,
        ),
    );
    routes = routes
        .add_service(oprc_grpc::proto::health::crm_info_service_server::CrmInfoServiceServer::new(
            crm_info,
        ))
        .add_service(oprc_grpc::proto::topology::topology_service_server::TopologyServiceServer::new(
            topology,
        ))
        .add_service(tonic_deploy);

    // Conditionally add netsim service if enabled at runtime.
    #[cfg(feature = "network-sim")]
    {
        let netsim_enabled = std::env::var("OPRC_NETSIM_ENABLED")
            .unwrap_or_default()
            .eq_ignore_ascii_case("true");
        if netsim_enabled {
            let env_name = std::env::var("OPRC_NETSIM_ENV_NAME")
                .unwrap_or_else(|_| "unknown".to_string());
            let netsim_svc = netsim::NetsimSvc::new(zenoh, env_name).await;
            routes = routes.add_service(
                oprc_grpc::proto::netsim::netsim_service_server::NetsimServiceServer::new(netsim_svc),
            );
        }
    }

    if let Some(reflection) = reflection {
        routes = routes.add_service(reflection);
    }

    routes
}
