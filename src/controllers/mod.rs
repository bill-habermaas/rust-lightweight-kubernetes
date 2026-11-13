//! Built-in operators (the "control plane" and the node agent).

pub mod deployment;
pub mod kubelet;
pub mod scheduler;
pub mod service;

pub use deployment::DeploymentController;
pub use kubelet::Kubelet;
pub use scheduler::Scheduler;
pub use service::ServiceController;

use crate::api::Api;
use crate::model::*;
use crate::operator::{Operator, OperatorContext};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Control-plane operators (run on exactly one instance).
pub fn control_plane() -> Vec<Arc<dyn Operator>> {
    vec![Arc::new(Scheduler), Arc::new(DeploymentController), Arc::new(ServiceController)]
}

/// Register this node and keep its heartbeat fresh.
pub async fn heartbeat_once(api: &Api, node: &str, address: &str, capacity: u32) -> crate::error::Result<()> {
    let key = ResourceKey::new(kinds::NODE, "", node);
    if api.try_get(&key).await?.is_none() {
        let n = Resource::new(kinds::NODE, "", node, json!({"address": address, "capacityPods": capacity}))
            .with_labels(&[("lkube.io/hostname", node)]);
        match api.create(n).await {
            Ok(_) | Err(crate::error::ApiError::AlreadyExists(_)) => {}
            Err(e) => return Err(e),
        }
    }
    let status = NodeStatus { ready: true, last_heartbeat: now_rfc3339() };
    api.update_status(&key, serde_json::to_value(status)?).await?;
    Ok(())
}

/// Background tasks for the node agent: heartbeats and container garbage collection.
pub fn spawn_node_agent(
    api: Arc<Api>,
    kubelet: Arc<Kubelet>,
    node: String,
    address: String,
    capacity: u32,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let ctx = OperatorContext { api: api.clone() };
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            if let Err(e) = heartbeat_once(&api, &node, &address, capacity).await {
                tracing::warn!(error = %e, "node heartbeat failed");
            }
            if n.is_multiple_of(6) {
                if let Err(e) = kubelet.garbage_collect(&ctx).await {
                    tracing::warn!(error = %e, "container garbage collection failed");
                }
            }
            n += 1;
        }
    })
}
