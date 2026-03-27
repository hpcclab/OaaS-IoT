//! gRPC → ZRPC bridge for network simulation.
//!
//! The CRM exposes a `NetsimService` gRPC endpoint and forwards requests to
//! the co-deployed router's ZRPC netsim queryable.

use std::sync::Arc;

use oprc_grpc::proto::netsim::{
    self as pb, NetsimAction, NetsimControlRequest, NetsimControlResponse,
    netsim_service_server::NetsimService,
};
use oprc_netsim::zrpc_types::{
    NETSIM_KEY_PREFIX, NetsimCommand, NetsimResponse, NetsimZrpcType,
};
use oprc_zrpc::ZrpcClient;
use tonic::{Request, Response, Status};
use tracing::info;
use zenoh::Session;

/// gRPC service that bridges netsim commands to the local router's ZRPC queryable.
pub struct NetsimSvc {
    zrpc_client: ZrpcClient<NetsimZrpcType>,
    #[allow(dead_code)]
    env_name: String,
}

impl NetsimSvc {
    pub async fn new(session: Arc<Session>, env_name: String) -> Self {
        let service_id = format!("{NETSIM_KEY_PREFIX}/{env_name}");
        let client =
            ZrpcClient::<NetsimZrpcType>::new(service_id, (*session).clone())
                .await;
        info!(env = %env_name, "CRM netsim gRPC→ZRPC bridge initialized");
        Self {
            zrpc_client: client,
            env_name,
        }
    }
}

#[tonic::async_trait]
impl NetsimService for NetsimSvc {
    async fn control(
        &self,
        request: Request<NetsimControlRequest>,
    ) -> Result<Response<NetsimControlResponse>, Status> {
        let req = request.into_inner();
        let cmd = match req.action() {
            NetsimAction::GetStatus => NetsimCommand::GetStatus,
            NetsimAction::Partition => NetsimCommand::Partition {
                peer: req.peer.clone(),
            },
            NetsimAction::Heal => NetsimCommand::Heal {
                peer: req.peer.clone(),
            },
            NetsimAction::SetLatency => NetsimCommand::SetLatency {
                peer: req.peer.clone(),
                latency_ms: req.latency_ms,
            },
        };

        let resp =
            self.zrpc_client.call(&cmd).await.map_err(|e| {
                Status::internal(format!("ZRPC call failed: {e}"))
            })?;

        match resp {
            NetsimResponse::Ok { env_name, links } => {
                let pb_links = links
                    .into_iter()
                    .map(|l| pb::PeerLinkInfo {
                        peer_env: l.peer_env,
                        connected: l.connected,
                        latency_ms: l.latency_ms,
                    })
                    .collect();
                Ok(Response::new(NetsimControlResponse {
                    env_name,
                    links: pb_links,
                }))
            }
            NetsimResponse::Error { message } => Err(Status::internal(message)),
        }
    }
}
