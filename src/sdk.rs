//! Client SDK for out-of-process operators and tools.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use lkube::sdk::LkubeClient;
//! let token = LkubeClient::login("http://localhost:50051", "admin", "admin12345").await?;
//! let mut client = LkubeClient::connect("http://localhost:50051", &token).await?;
//! let kinds = client.cluster.list_kinds(lkube::pb::Empty {}).await?;
//! # Ok(()) }
//! ```

use crate::pb::{
    self, admin_service_client::AdminServiceClient, auth_service_client::AuthServiceClient,
    cluster_service_client::ClusterServiceClient, operator_service_client::OperatorServiceClient,
};
use async_trait::async_trait;
use std::time::Duration;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::interceptor::InterceptedService;
use tonic::service::Interceptor;
use tonic::transport::Channel;
use tonic::{Request, Status};

/// Adds `authorization: Bearer <token>` to every request.
#[derive(Clone)]
pub struct AuthInterceptor {
    header: MetadataValue<Ascii>,
}

impl AuthInterceptor {
    pub fn new(token: &str) -> anyhow::Result<Self> {
        Ok(Self { header: format!("Bearer {token}").parse()? })
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        req.metadata_mut().insert("authorization", self.header.clone());
        Ok(req)
    }
}

pub type AuthedChannel = InterceptedService<Channel, AuthInterceptor>;

#[derive(Clone)]
pub struct LkubeClient {
    pub auth: AuthServiceClient<AuthedChannel>,
    pub admin: AdminServiceClient<AuthedChannel>,
    pub cluster: ClusterServiceClient<AuthedChannel>,
    pub operators: OperatorServiceClient<AuthedChannel>,
}

impl LkubeClient {
    pub async fn connect(endpoint: &str, token: &str) -> anyhow::Result<Self> {
        let channel = Channel::from_shared(endpoint.to_string())?.connect().await?;
        let i = AuthInterceptor::new(token)?;
        Ok(Self {
            auth: AuthServiceClient::with_interceptor(channel.clone(), i.clone()),
            admin: AdminServiceClient::with_interceptor(channel.clone(), i.clone()),
            cluster: ClusterServiceClient::with_interceptor(channel.clone(), i.clone()),
            operators: OperatorServiceClient::with_interceptor(channel, i),
        })
    }

    /// Username/password login; returns a session token.
    pub async fn login(endpoint: &str, username: &str, password: &str) -> anyhow::Result<String> {
        let mut c = AuthServiceClient::connect(endpoint.to_string()).await?;
        let resp = c
            .login(pb::LoginRequest { username: username.into(), password: password.into() })
            .await?
            .into_inner();
        Ok(resp.token)
    }

    /// Build a resource message from JSON spec.
    pub fn resource(kind: &str, namespace: &str, name: &str, spec: serde_json::Value) -> pb::Resource {
        pb::Resource {
            api_version: String::new(),
            kind: kind.into(),
            metadata: Some(pb::ObjectMeta { name: name.into(), namespace: namespace.into(), ..Default::default() }),
            spec_json: spec.to_string(),
            status_json: String::new(),
        }
    }
}

/// Implemented by out-of-process operators.
#[async_trait]
pub trait RemoteReconciler: Send + Sync {
    async fn reconcile(&self, client: &mut LkubeClient, event: pb::WatchEvent) -> anyhow::Result<()>;
}

/// Watch `kinds` and call the reconciler for every event, reconnecting on failure.
/// `send_initial = true` on every (re)connect gives level-triggered semantics.
pub async fn run_remote_operator<R: RemoteReconciler>(
    mut client: LkubeClient,
    kinds: Vec<String>,
    reconciler: R,
) -> anyhow::Result<()> {
    loop {
        let req = pb::WatchRequest { kinds: kinds.clone(), namespace: String::new(), send_initial: true };
        match client.cluster.watch(req).await {
            Ok(resp) => {
                let mut stream = resp.into_inner();
                loop {
                    match stream.message().await {
                        Ok(Some(ev)) => {
                            if let Err(e) = reconciler.reconcile(&mut client, ev).await {
                                tracing::warn!(error = %e, "reconcile failed");
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            if e.code() == tonic::Code::Unauthenticated || e.code() == tonic::Code::PermissionDenied {
                                return Err(e.into());
                            }
                            tracing::warn!(error = %e, "watch stream error");
                            break;
                        }
                    }
                }
            }
            Err(e) if e.code() == tonic::Code::Unauthenticated || e.code() == tonic::Code::PermissionDenied => {
                return Err(e.into())
            }
            Err(e) => tracing::warn!(error = %e, "watch failed"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
