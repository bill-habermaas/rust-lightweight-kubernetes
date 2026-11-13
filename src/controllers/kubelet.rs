//! Node agent: runs the containers of pods bound to this node through a
//! [`ContainerRuntime`], applies the restart policy and reports pod status.

use crate::model::*;
use crate::operator::{Operator, OperatorContext, ReconcileAction, ReconcileRequest};
use crate::runtime::{pod_label, ContainerRuntime, RunSpec, LABEL_CONTAINER, LABEL_POD};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

pub struct Kubelet {
    node: String,
    runtime: Arc<dyn ContainerRuntime>,
}

impl Kubelet {
    pub fn new(node: &str, runtime: Arc<dyn ContainerRuntime>) -> Self {
        Self { node: node.into(), runtime }
    }

    fn run_spec(&self, pod: &Resource, c: &Container, restart: u32) -> RunSpec {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert(LABEL_POD.into(), pod_label(&pod.metadata.namespace, &pod.metadata.name));
        labels.insert(LABEL_CONTAINER.into(), c.name.clone());
        let mut env = c.env.clone();
        env.entry("LKUBE_POD_NAME".into()).or_insert_with(|| pod.metadata.name.clone());
        env.entry("LKUBE_NAMESPACE".into()).or_insert_with(|| pod.metadata.namespace.clone());
        env.entry("LKUBE_NODE_NAME".into()).or_insert_with(|| self.node.clone());
        RunSpec {
            name: format!(
                "lkube_{}_{}_{}_{}_{}",
                pod.metadata.namespace,
                pod.metadata.name,
                c.name,
                restart,
                random_suffix(4)
            ),
            image: c.image.clone(),
            command: c.command.clone(),
            args: c.args.clone(),
            env,
            labels,
            ports: c.ports.clone(),
        }
    }

    async fn start(&self, pod: &Resource, c: &Container, cs: &mut ContainerStatus) {
        match self.runtime.run(&self.run_spec(pod, c, cs.restart_count)).await {
            Ok(id) => {
                cs.container_id = id;
                cs.state = "running".into();
                cs.exit_code = None;
                cs.message = None;
            }
            Err(e) => {
                cs.container_id.clear();
                cs.state = "waiting".into();
                cs.message = Some(e.to_string());
            }
        }
    }

    /// Bring the pod's containers in line with its spec and compute the new status.
    pub async fn sync_pod(&self, pod: &Resource, spec: &PodSpec, mut status: PodStatus) -> PodStatus {
        let mut statuses = Vec::with_capacity(spec.containers.len());
        let mut pod_ip = None;

        for c in &spec.containers {
            let mut cs = status
                .container_statuses
                .iter()
                .find(|s| s.name == c.name)
                .cloned()
                .unwrap_or_else(|| ContainerStatus { name: c.name.clone(), ..Default::default() });

            let state = if cs.container_id.is_empty() {
                None
            } else {
                self.runtime.inspect(&cs.container_id).await.unwrap_or(None)
            };

            match state {
                Some(s) if s.running => {
                    cs.state = "running".into();
                    pod_ip = pod_ip.or(s.ip);
                }
                Some(s) => {
                    let code = s.exit_code.unwrap_or(-1);
                    let restart = match spec.restart_policy {
                        RestartPolicy::Always => true,
                        RestartPolicy::OnFailure => code != 0,
                        RestartPolicy::Never => false,
                    };
                    if restart {
                        let _ = self.runtime.remove(&cs.container_id).await;
                        cs.restart_count += 1;
                        self.start(pod, c, &mut cs).await;
                    } else {
                        cs.state = "exited".into();
                        cs.exit_code = Some(code);
                        cs.message = s.error;
                    }
                }
                None => {
                    // Never started, start failed earlier, or the container vanished.
                    let finished = cs.state == "exited";
                    if !finished {
                        if !cs.container_id.is_empty() {
                            cs.restart_count += 1;
                        }
                        self.start(pod, c, &mut cs).await;
                        if cs.state == "running" {
                            if let Ok(Some(s)) = self.runtime.inspect(&cs.container_id).await {
                                pod_ip = pod_ip.or(s.ip);
                            }
                        }
                    }
                }
            }
            statuses.push(cs);
        }

        status.phase = compute_phase(&statuses);
        status.message = statuses.iter().filter_map(|s| s.message.clone()).collect::<Vec<_>>().join("; ");
        status.pod_ip = if status.phase == PodPhase::Running { pod_ip } else { None };
        status.container_statuses = statuses;
        status
    }

    /// Remove containers whose pod no longer exists (e.g. deleted while we were down).
    pub async fn garbage_collect(&self, ctx: &OperatorContext) -> anyhow::Result<usize> {
        let live: BTreeSet<String> = ctx
            .api
            .list(kinds::POD, None, &Labels::new())
            .await?
            .iter()
            .filter(|p| p.spec_as::<PodSpec>().ok().and_then(|s| s.node_name).as_deref() == Some(&self.node))
            .map(|p| pod_label(&p.metadata.namespace, &p.metadata.name))
            .collect();
        let mut removed = 0;
        for c in self.runtime.list_managed().await? {
            if !live.contains(&c.pod) {
                tracing::info!(container = %c.id, pod = %c.pod, "removing orphaned container");
                self.runtime.remove(&c.id).await?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

pub fn compute_phase(statuses: &[ContainerStatus]) -> PodPhase {
    if statuses.is_empty() {
        return PodPhase::Pending;
    }
    if statuses.iter().any(|s| s.state == "waiting") {
        return PodPhase::Pending;
    }
    if statuses.iter().all(|s| s.state == "exited") {
        return if statuses.iter().all(|s| s.exit_code == Some(0)) { PodPhase::Succeeded } else { PodPhase::Failed };
    }
    PodPhase::Running
}

#[async_trait]
impl Operator for Kubelet {
    fn name(&self) -> &str {
        "kubelet"
    }
    fn primary_kind(&self) -> &str {
        kinds::POD
    }
    fn resync_interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    async fn reconcile(&self, ctx: &OperatorContext, req: ReconcileRequest) -> anyhow::Result<ReconcileAction> {
        let Some(pod) = req.resource else {
            // Pod deleted: stop its containers.
            let label = pod_label(&req.key.namespace, &req.key.name);
            for c in self.runtime.list_managed().await? {
                if c.pod == label {
                    self.runtime.remove(&c.id).await?;
                }
            }
            return Ok(ReconcileAction::Done);
        };
        let spec: PodSpec = pod.spec_as()?;
        if spec.node_name.as_deref() != Some(self.node.as_str()) {
            return Ok(ReconcileAction::Done);
        }
        let current: PodStatus = pod.status_as();
        if current.phase.is_terminal() {
            return Ok(ReconcileAction::Done);
        }
        let next = self.sync_pod(&pod, &spec, current.clone()).await;
        if next != current {
            ctx.api.update_status(&req.key, serde_json::to_value(&next)?).await?;
        }
        Ok(ReconcileAction::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(state: &str, code: Option<i64>) -> ContainerStatus {
        ContainerStatus { name: "c".into(), state: state.into(), exit_code: code, ..Default::default() }
    }

    #[test]
    fn phase_computation() {
        assert_eq!(compute_phase(&[]), PodPhase::Pending);
        assert_eq!(compute_phase(&[cs("running", None)]), PodPhase::Running);
        assert_eq!(compute_phase(&[cs("running", None), cs("waiting", None)]), PodPhase::Pending);
        assert_eq!(compute_phase(&[cs("exited", Some(0)), cs("exited", Some(0))]), PodPhase::Succeeded);
        assert_eq!(compute_phase(&[cs("exited", Some(0)), cs("exited", Some(2))]), PodPhase::Failed);
        assert_eq!(compute_phase(&[cs("exited", Some(0)), cs("running", None)]), PodPhase::Running);
    }
}
