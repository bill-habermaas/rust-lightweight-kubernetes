//! Keeps `spec.replicas` pods running for each Deployment. Pods are owned by the
//! deployment (owner_uid) and labelled with `lkube.io/deployment`. A change to the pod
//! template replaces pods (Recreate strategy) using a template hash annotation.

use crate::api::WatchEvent;
use crate::model::*;
use crate::operator::{Operator, OperatorContext, ReconcileAction, ReconcileRequest};
use async_trait::async_trait;
use std::time::Duration;

pub struct DeploymentController;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScalePlan {
    pub create: usize,
    pub delete: Vec<String>,
    /// Pods that stay.
    pub keep: Vec<String>,
}

pub fn template_hash(t: &PodTemplate) -> String {
    fnv1a_hex(&serde_json::to_string(t).unwrap_or_default())[..10].to_string()
}

/// Pure planning function: which pods to delete and how many to create.
pub fn plan(desired: i32, hash: &str, pods: &[Resource]) -> ScalePlan {
    let mut plan = ScalePlan::default();
    let mut current: Vec<&Resource> = vec![];
    for p in pods {
        let stale = p.metadata.annotations.get(ANNOTATION_TEMPLATE_HASH).map(String::as_str) != Some(hash);
        let terminal = p.status_as::<PodStatus>().phase.is_terminal();
        if stale || terminal {
            plan.delete.push(p.metadata.name.clone());
        } else {
            current.push(p);
        }
    }
    let desired = desired.max(0) as usize;
    if current.len() > desired {
        // Remove non-running pods first, then the newest ones.
        current.sort_by(|a, b| {
            let ar = a.status_as::<PodStatus>().phase == PodPhase::Running;
            let br = b.status_as::<PodStatus>().phase == PodPhase::Running;
            ar.cmp(&br).then_with(|| b.metadata.created_at.cmp(&a.metadata.created_at))
        });
        let excess = current.len() - desired;
        plan.delete.extend(current.drain(..excess).map(|p| p.metadata.name.clone()));
    }
    plan.create = desired.saturating_sub(current.len());
    plan.keep = current.iter().map(|p| p.metadata.name.clone()).collect();
    plan
}

#[async_trait]
impl Operator for DeploymentController {
    fn name(&self) -> &str {
        "deployment-controller"
    }
    fn primary_kind(&self) -> &str {
        kinds::DEPLOYMENT
    }
    fn watches(&self) -> Vec<String> {
        vec![kinds::DEPLOYMENT.into(), kinds::POD.into()]
    }

    async fn map_event(&self, _ctx: &OperatorContext, ev: &WatchEvent) -> Vec<ResourceKey> {
        let r = &ev.resource;
        if r.kind == kinds::DEPLOYMENT {
            return vec![r.key()];
        }
        match r.metadata.labels.get(LABEL_DEPLOYMENT) {
            Some(dep) => vec![ResourceKey::new(kinds::DEPLOYMENT, &r.metadata.namespace, dep)],
            None => vec![],
        }
    }

    async fn reconcile(&self, ctx: &OperatorContext, req: ReconcileRequest) -> anyhow::Result<ReconcileAction> {
        // Deleted deployments: their pods are removed by the API's cascading delete.
        let Some(dep) = req.resource else { return Ok(ReconcileAction::Done) };
        let spec: DeploymentSpec = dep.spec_as()?;
        let ns = &dep.metadata.namespace;
        let hash = template_hash(&spec.template);

        let pods: Vec<Resource> = ctx
            .api
            .list(kinds::POD, Some(ns), &spec.selector)
            .await?
            .into_iter()
            .filter(|p| p.metadata.owner_uid == dep.metadata.uid)
            .collect();
        let plan = plan(spec.replicas, &hash, &pods);

        for name in &plan.delete {
            let _ = ctx.api.delete(&ResourceKey::new(kinds::POD, ns, name)).await;
        }
        for _ in 0..plan.create {
            let mut labels = spec.template.labels.clone();
            labels.insert(LABEL_DEPLOYMENT.into(), dep.metadata.name.clone());
            let mut pod_spec = spec.template.spec.clone();
            pod_spec.node_name = None;
            let mut pod = Resource::new(
                kinds::POD,
                ns,
                &format!("{}-{}", dep.metadata.name, random_suffix(5)),
                serde_json::to_value(&pod_spec)?,
            );
            pod.metadata.labels = labels;
            pod.metadata.annotations.insert(ANNOTATION_TEMPLATE_HASH.into(), hash.clone());
            pod.metadata.owner_uid = dep.metadata.uid.clone();
            ctx.api.create(pod).await?;
        }

        let ready = pods
            .iter()
            .filter(|p| plan.keep.contains(&p.metadata.name))
            .filter(|p| p.status_as::<PodStatus>().phase == PodPhase::Running)
            .count() as i32;
        let total = (plan.keep.len() + plan.create) as i32;
        let status = DeploymentStatus {
            replicas: total,
            ready_replicas: ready,
            updated_replicas: total,
            observed_generation: dep.metadata.generation,
        };
        ctx.api.update_status(&dep.key(), serde_json::to_value(status)?).await?;

        if plan.create > 0 || !plan.delete.is_empty() || ready < spec.replicas {
            Ok(ReconcileAction::RequeueAfter(Duration::from_secs(2)))
        } else {
            Ok(ReconcileAction::Done)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pod(name: &str, hash: &str, phase: &str, created: &str) -> Resource {
        let mut p = Resource::new(kinds::POD, "default", name, json!({"containers": [{"name": "c", "image": "i"}]}));
        p.metadata.annotations.insert(ANNOTATION_TEMPLATE_HASH.into(), hash.into());
        p.metadata.created_at = created.into();
        p.status = json!({"phase": phase});
        p
    }

    #[test]
    fn scales_up_from_zero() {
        let p = plan(3, "h", &[]);
        assert_eq!(p.create, 3);
        assert!(p.delete.is_empty());
    }

    #[test]
    fn steady_state_is_noop() {
        let pods = vec![pod("a", "h", "Running", "1"), pod("b", "h", "Running", "2")];
        let p = plan(2, "h", &pods);
        assert_eq!(p, ScalePlan { create: 0, delete: vec![], keep: vec!["a".into(), "b".into()] });
    }

    #[test]
    fn scales_down_preferring_pending_then_newest() {
        let pods = vec![
            pod("old-running", "h", "Running", "1"),
            pod("new-running", "h", "Running", "3"),
            pod("pending", "h", "Pending", "2"),
        ];
        let p = plan(1, "h", &pods);
        assert_eq!(p.create, 0);
        assert_eq!(p.delete, vec!["pending".to_string(), "new-running".to_string()]);
        assert_eq!(p.keep, vec!["old-running".to_string()]);
    }

    #[test]
    fn replaces_stale_and_failed_pods() {
        let pods = vec![pod("stale", "old", "Running", "1"), pod("failed", "h", "Failed", "1"), pod("ok", "h", "Running", "1")];
        let p = plan(2, "h", &pods);
        assert_eq!(p.delete, vec!["stale".to_string(), "failed".to_string()]);
        assert_eq!(p.create, 1);
    }

    #[test]
    fn template_hash_changes_with_template() {
        let t1: PodTemplate = serde_json::from_value(json!({"labels": {"a": "b"}, "spec": {"containers": [{"name": "c", "image": "nginx:1"}]}})).unwrap();
        let mut t2 = t1.clone();
        t2.spec.containers[0].image = "nginx:2".into();
        assert_eq!(template_hash(&t1), template_hash(&t1.clone()));
        assert_ne!(template_hash(&t1), template_hash(&t2));
    }
}
