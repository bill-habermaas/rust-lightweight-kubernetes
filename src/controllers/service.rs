//! Maintains `status.endpoints` of each Service: the running pods matching its selector.

use crate::api::WatchEvent;
use crate::model::*;
use crate::operator::{Operator, OperatorContext, ReconcileAction, ReconcileRequest};
use async_trait::async_trait;

pub struct ServiceController;

pub fn endpoints_for(spec: &ServiceSpec, pods: &[Resource]) -> Vec<Endpoint> {
    if spec.selector.is_empty() {
        return vec![];
    }
    let ports: Vec<u16> = spec.ports.iter().map(|p| p.target_port.unwrap_or(p.port)).collect();
    let mut eps: Vec<Endpoint> = pods
        .iter()
        .filter(|p| p.matches_selector(&spec.selector))
        .filter_map(|p| {
            let st: PodStatus = p.status_as();
            if st.phase != PodPhase::Running {
                return None;
            }
            let ip = st.pod_ip?;
            let node = p.spec_as::<PodSpec>().ok().and_then(|s| s.node_name).unwrap_or_default();
            Some(Endpoint { pod: p.metadata.name.clone(), node, ip, ports: ports.clone() })
        })
        .collect();
    eps.sort_by(|a, b| a.pod.cmp(&b.pod));
    eps
}

#[async_trait]
impl Operator for ServiceController {
    fn name(&self) -> &str {
        "service-controller"
    }
    fn primary_kind(&self) -> &str {
        kinds::SERVICE
    }
    fn watches(&self) -> Vec<String> {
        vec![kinds::SERVICE.into(), kinds::POD.into()]
    }

    async fn map_event(&self, ctx: &OperatorContext, ev: &WatchEvent) -> Vec<ResourceKey> {
        let r = &ev.resource;
        if r.kind == kinds::SERVICE {
            return vec![r.key()];
        }
        // A pod changed: reconcile every service in its namespace that selects it.
        ctx.api
            .list(kinds::SERVICE, Some(&r.metadata.namespace), &Labels::new())
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|svc| {
                svc.spec_as::<ServiceSpec>()
                    .map(|s| !s.selector.is_empty() && r.matches_selector(&s.selector))
                    .unwrap_or(false)
            })
            .map(|svc| svc.key())
            .collect()
    }

    async fn reconcile(&self, ctx: &OperatorContext, req: ReconcileRequest) -> anyhow::Result<ReconcileAction> {
        let Some(svc) = req.resource else { return Ok(ReconcileAction::Done) };
        let spec: ServiceSpec = svc.spec_as()?;
        let pods = ctx.api.list(kinds::POD, Some(&svc.metadata.namespace), &spec.selector).await?;
        let status = ServiceStatus { endpoints: endpoints_for(&spec, &pods) };
        ctx.api.update_status(&svc.key(), serde_json::to_value(status)?).await?;
        Ok(ReconcileAction::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_running_pods_with_ip_become_endpoints() {
        let spec: ServiceSpec = serde_json::from_value(json!({"selector": {"app": "web"}, "ports": [{"port": 80, "targetPort": 8080}]})).unwrap();
        let mk = |name: &str, app: &str, status: serde_json::Value| {
            let mut p = Resource::new(kinds::POD, "default", name, json!({"containers": [{"name": "c", "image": "i"}], "nodeName": "n1"}))
                .with_labels(&[("app", app)]);
            p.status = status;
            p
        };
        let pods = vec![
            mk("b", "web", json!({"phase": "Running", "podIp": "10.0.0.2"})),
            mk("a", "web", json!({"phase": "Running", "podIp": "10.0.0.1"})),
            mk("pending", "web", json!({"phase": "Pending"})),
            mk("other", "db", json!({"phase": "Running", "podIp": "10.0.0.9"})),
        ];
        let eps = endpoints_for(&spec, &pods);
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].pod, "a");
        assert_eq!(eps[0].ports, vec![8080]);
        assert_eq!(eps[0].node, "n1");
    }

    #[test]
    fn empty_selector_has_no_endpoints() {
        let spec = ServiceSpec::default();
        assert!(endpoints_for(&spec, &[]).is_empty());
    }
}
