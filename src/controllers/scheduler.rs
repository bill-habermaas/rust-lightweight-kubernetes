//! Assigns unscheduled pods to ready nodes (least-loaded first, honouring nodeSelector
//! and node capacity).

use crate::error::ApiError;
use crate::model::*;
use crate::operator::{Operator, OperatorContext, ReconcileAction, ReconcileRequest};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::time::Duration;

/// A node whose last heartbeat is older than this is considered NotReady.
pub const HEARTBEAT_STALE_SECS: i64 = 40;

pub struct Scheduler;

pub fn node_is_schedulable(node: &Resource, now: DateTime<Utc>) -> bool {
    let status: NodeStatus = node.status_as();
    if !status.ready {
        return false;
    }
    DateTime::parse_from_rfc3339(&status.last_heartbeat)
        .map(|hb| (now - hb.with_timezone(&Utc)).num_seconds() < HEARTBEAT_STALE_SECS)
        .unwrap_or(false)
}

/// Pure scheduling decision.
pub fn pick_node(pod: &PodSpec, nodes: &[Resource], pods: &[Resource], now: DateTime<Utc>) -> Option<String> {
    nodes
        .iter()
        .filter(|n| node_is_schedulable(n, now))
        .filter(|n| pod.node_selector.iter().all(|(k, v)| n.metadata.labels.get(k) == Some(v)))
        .filter_map(|n| {
            let capacity = n.spec_as::<NodeSpec>().map(|s| s.capacity_pods).unwrap_or(110) as usize;
            let load = pods
                .iter()
                .filter(|p| {
                    p.spec_as::<PodSpec>().ok().and_then(|s| s.node_name).as_deref() == Some(&n.metadata.name)
                        && !p.status_as::<PodStatus>().phase.is_terminal()
                })
                .count();
            (load < capacity).then_some((load, n.metadata.name.clone()))
        })
        .min()
        .map(|(_, name)| name)
}

#[async_trait]
impl Operator for Scheduler {
    fn name(&self) -> &str {
        "scheduler"
    }
    fn primary_kind(&self) -> &str {
        kinds::POD
    }
    fn resync_interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    async fn reconcile(&self, ctx: &OperatorContext, req: ReconcileRequest) -> anyhow::Result<ReconcileAction> {
        let Some(pod) = req.resource else { return Ok(ReconcileAction::Done) };
        let spec: PodSpec = pod.spec_as()?;
        if spec.node_name.is_some() {
            return Ok(ReconcileAction::Done);
        }
        let nodes = ctx.api.list(kinds::NODE, None, &Labels::new()).await?;
        let pods = ctx.api.list(kinds::POD, None, &Labels::new()).await?;
        match pick_node(&spec, &nodes, &pods, Utc::now()) {
            Some(node) => {
                let mut bound = pod.clone();
                bound.spec["nodeName"] = json!(node);
                match ctx.api.update(bound).await {
                    Ok(_) => {
                        tracing::info!(pod = %req.key, node, "scheduled pod");
                        Ok(ReconcileAction::Done)
                    }
                    Err(ApiError::Conflict(_)) => Ok(ReconcileAction::RequeueAfter(Duration::from_millis(100))),
                    Err(e) => Err(e.into()),
                }
            }
            None => {
                let mut status: PodStatus = pod.status_as();
                status.phase = PodPhase::Pending;
                status.message = format!("0/{} nodes available for scheduling", nodes.len());
                ctx.api.update_status(&req.key, serde_json::to_value(status)?).await?;
                Ok(ReconcileAction::RequeueAfter(Duration::from_secs(5)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn node(name: &str, ready: bool, age_secs: i64, capacity: u32, labels: &[(&str, &str)]) -> Resource {
        let hb = (Utc::now() - ChronoDuration::seconds(age_secs)).to_rfc3339();
        let mut n = Resource::new(kinds::NODE, "", name, json!({"capacityPods": capacity})).with_labels(labels);
        n.status = json!({"ready": ready, "lastHeartbeat": hb});
        n
    }

    fn pod_on(node: &str) -> Resource {
        Resource::new(kinds::POD, "default", &format!("p-{}", random_suffix(4)), json!({"containers": [{"name": "c", "image": "i"}], "nodeName": node}))
    }

    fn spec() -> PodSpec {
        serde_json::from_value(json!({"containers": [{"name": "c", "image": "i"}]})).unwrap()
    }

    #[test]
    fn picks_least_loaded_ready_node() {
        let nodes = vec![node("a", true, 1, 10, &[]), node("b", true, 1, 10, &[])];
        let pods = vec![pod_on("a"), pod_on("a"), pod_on("b")];
        assert_eq!(pick_node(&spec(), &nodes, &pods, Utc::now()).as_deref(), Some("b"));
    }

    #[test]
    fn ties_break_by_name() {
        let nodes = vec![node("b", true, 1, 10, &[]), node("a", true, 1, 10, &[])];
        assert_eq!(pick_node(&spec(), &nodes, &[], Utc::now()).as_deref(), Some("a"));
    }

    #[test]
    fn skips_not_ready_and_stale_nodes() {
        let nodes = vec![node("down", false, 1, 10, &[]), node("stale", true, 300, 10, &[])];
        assert_eq!(pick_node(&spec(), &nodes, &[], Utc::now()), None);
    }

    #[test]
    fn honours_capacity() {
        let nodes = vec![node("a", true, 1, 1, &[])];
        assert_eq!(pick_node(&spec(), &nodes, &[pod_on("a")], Utc::now()), None);
        // terminal pods do not count against capacity
        let mut done = pod_on("a");
        done.status = json!({"phase": "Succeeded"});
        assert_eq!(pick_node(&spec(), &nodes, &[done], Utc::now()).as_deref(), Some("a"));
    }

    #[test]
    fn honours_node_selector() {
        let nodes = vec![node("a", true, 1, 10, &[("disk", "hdd")]), node("b", true, 1, 10, &[("disk", "ssd")])];
        let mut s = spec();
        s.node_selector.insert("disk".into(), "ssd".into());
        let pods = vec![pod_on("a"), pod_on("b"), pod_on("b")];
        assert_eq!(pick_node(&s, &nodes, &pods, Utc::now()).as_deref(), Some("b"));
        s.node_selector.insert("disk".into(), "nvme".into());
        assert_eq!(pick_node(&s, &nodes, &pods, Utc::now()), None);
    }
}
