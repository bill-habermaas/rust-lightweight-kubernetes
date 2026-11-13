//! End-to-end tests of the built-in operators: Deployment -> Pods -> scheduling ->
//! containers (mock runtime) -> Service endpoints, plus restart policies and GC.

use lkube::api::Api;
use lkube::controllers::{self, DeploymentController, Kubelet, Scheduler, ServiceController};
use lkube::model::*;
use lkube::operator::{reconcile_key, Operator, OperatorContext, OperatorManager};
use lkube::runtime::MockRuntime;
use lkube::store::MemoryStore;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

struct Cluster {
    ctx: OperatorContext,
    runtime: Arc<MockRuntime>,
    kubelet: Kubelet,
}

impl Cluster {
    async fn new() -> Self {
        let api = Api::new(Arc::new(MemoryStore::new()));
        lkube::bootstrap::ensure_default_namespace(&api).await.unwrap();
        controllers::heartbeat_once(&api, "node-1", "10.0.0.1", 10).await.unwrap();
        let runtime = Arc::new(MockRuntime::new());
        let kubelet = Kubelet::new("node-1", runtime.clone());
        Self { ctx: OperatorContext { api }, runtime, kubelet }
    }

    fn api(&self) -> &Api {
        &self.ctx.api
    }

    async fn pods(&self) -> Vec<Resource> {
        self.api().list(kinds::POD, None, &Labels::new()).await.unwrap()
    }

    /// Run every built-in controller over every object until nothing changes.
    async fn converge(&self) {
        for _ in 0..4 {
            for d in self.api().list(kinds::DEPLOYMENT, None, &Labels::new()).await.unwrap() {
                reconcile_key(&self.ctx, &DeploymentController, d.key()).await.unwrap();
            }
            for p in self.pods().await {
                reconcile_key(&self.ctx, &Scheduler, p.key()).await.unwrap();
                reconcile_key(&self.ctx, &self.kubelet, p.key()).await.unwrap();
            }
            for s in self.api().list(kinds::SERVICE, None, &Labels::new()).await.unwrap() {
                reconcile_key(&self.ctx, &ServiceController, s.key()).await.unwrap();
            }
            // In the running system delete events reach the kubelet; here GC stands in.
            self.kubelet.garbage_collect(&self.ctx).await.unwrap();
        }
    }
}

fn deployment(name: &str, replicas: i32, image: &str) -> Resource {
    Resource::new(
        kinds::DEPLOYMENT,
        "default",
        name,
        json!({"replicas": replicas, "template": {"labels": {"app": name}, "spec": {"containers": [{"name": "app", "image": image}]}}}),
    )
}

#[tokio::test]
async fn deployment_to_running_pods_and_service_endpoints() {
    let c = Cluster::new().await;
    c.api().apply(deployment("web", 3, "nginx:1")).await.unwrap();
    c.api()
        .apply(Resource::new(kinds::SERVICE, "default", "web", json!({"selector": {"app": "web"}, "ports": [{"port": 80}]})))
        .await
        .unwrap();
    c.converge().await;

    let pods = c.pods().await;
    assert_eq!(pods.len(), 3);
    for p in &pods {
        assert_eq!(p.spec_as::<PodSpec>().unwrap().node_name.as_deref(), Some("node-1"));
        assert_eq!(p.status_as::<PodStatus>().phase, PodPhase::Running);
        assert_eq!(p.metadata.labels.get(LABEL_DEPLOYMENT).unwrap(), "web");
    }
    assert_eq!(c.runtime.running_count(), 3);

    let dep = c.api().get(&ResourceKey::new(kinds::DEPLOYMENT, "default", "web")).await.unwrap();
    let st: DeploymentStatus = dep.status_as();
    assert_eq!((st.replicas, st.ready_replicas), (3, 3));

    let svc = c.api().get(&ResourceKey::new(kinds::SERVICE, "default", "web")).await.unwrap();
    assert_eq!(svc.status_as::<ServiceStatus>().endpoints.len(), 3);
}

#[tokio::test]
async fn scaling_and_rollout_and_delete() {
    let c = Cluster::new().await;
    c.api().apply(deployment("web", 3, "nginx:1")).await.unwrap();
    c.converge().await;

    c.api().scale("default", "web", 1).await.unwrap();
    c.converge().await;
    assert_eq!(c.pods().await.len(), 1);
    assert_eq!(c.runtime.count(), 1, "containers of removed pods are stopped");

    // template change -> pods replaced
    let old = c.pods().await[0].metadata.name.clone();
    c.api().apply(deployment("web", 1, "nginx:2")).await.unwrap();
    c.converge().await;
    let pods = c.pods().await;
    assert_eq!(pods.len(), 1);
    assert_ne!(pods[0].metadata.name, old);
    let id = &pods[0].status_as::<PodStatus>().container_statuses[0].container_id;
    assert_eq!(c.runtime.spec_of(id).unwrap().image, "nginx:2");

    // deleting the deployment cascades to its pods; the kubelet stops containers
    c.api().delete(&ResourceKey::new(kinds::DEPLOYMENT, "default", "web")).await.unwrap();
    assert!(c.pods().await.is_empty());
    reconcile_key(&c.ctx, &c.kubelet, pods[0].key()).await.unwrap();
    assert_eq!(c.runtime.count(), 0);
}

#[tokio::test]
async fn restart_policies() {
    let c = Cluster::new().await;
    let mk = |name: &str, policy: &str| {
        Resource::new(kinds::POD, "default", name, json!({"restartPolicy": policy, "containers": [{"name": "c", "image": "busybox"}]}))
    };
    for (n, p) in [("always", "Always"), ("never", "Never"), ("onfail", "OnFailure")] {
        c.api().apply(mk(n, p)).await.unwrap();
    }
    c.converge().await;

    let cid = |name: &str| {
        let api = c.api();
        let name = name.to_string();
        async move {
            let p = api.get(&ResourceKey::new(kinds::POD, "default", &name)).await.unwrap();
            p.status_as::<PodStatus>().container_statuses[0].container_id.clone()
        }
    };
    c.runtime.exit(&cid("always").await, 0);
    c.runtime.exit(&cid("never").await, 1);
    c.runtime.exit(&cid("onfail").await, 0);
    c.converge().await;

    let get = |name: &str| {
        let api = c.api();
        let name = name.to_string();
        async move { api.get(&ResourceKey::new(kinds::POD, "default", &name)).await.unwrap().status_as::<PodStatus>() }
    };
    let always = get("always").await;
    assert_eq!(always.phase, PodPhase::Running);
    assert_eq!(always.container_statuses[0].restart_count, 1);
    let never = get("never").await;
    assert_eq!(never.phase, PodPhase::Failed);
    assert_eq!(never.container_statuses[0].exit_code, Some(1));
    assert_eq!(get("onfail").await.phase, PodPhase::Succeeded);
}

#[tokio::test]
async fn image_pull_failures_leave_pod_pending_with_message() {
    let c = Cluster::new().await;
    c.runtime.fail_image("does-not-exist");
    c.api()
        .apply(Resource::new(kinds::POD, "default", "bad", json!({"containers": [{"name": "c", "image": "does-not-exist"}]})))
        .await
        .unwrap();
    c.converge().await;
    let st: PodStatus = c.api().get(&ResourceKey::new(kinds::POD, "default", "bad")).await.unwrap().status_as();
    assert_eq!(st.phase, PodPhase::Pending);
    assert!(st.message.contains("failed to pull"));
}

#[tokio::test]
async fn unschedulable_pods_stay_pending() {
    let c = Cluster::new().await;
    c.api()
        .apply(Resource::new(
            kinds::POD,
            "default",
            "picky",
            json!({"nodeSelector": {"gpu": "true"}, "containers": [{"name": "c", "image": "x"}]}),
        ))
        .await
        .unwrap();
    c.converge().await;
    let p = c.api().get(&ResourceKey::new(kinds::POD, "default", "picky")).await.unwrap();
    assert!(p.spec_as::<PodSpec>().unwrap().node_name.is_none());
    assert!(p.status_as::<PodStatus>().message.contains("nodes available"));
    assert_eq!(c.runtime.count(), 0);
}

#[tokio::test]
async fn garbage_collects_orphaned_containers() {
    let c = Cluster::new().await;
    c.api().apply(deployment("web", 2, "nginx")).await.unwrap();
    c.converge().await;
    assert_eq!(c.runtime.count(), 2);
    // delete pods directly from the store: no events, kubelet never hears about it
    for p in c.pods().await {
        c.api().store().delete(&p.key()).await.unwrap();
    }
    assert_eq!(c.kubelet.garbage_collect(&c.ctx).await.unwrap(), 2);
    assert_eq!(c.runtime.count(), 0);
}

/// A custom in-process operator built on the public `Operator` trait.
struct EchoOperator;

#[async_trait::async_trait]
impl Operator for EchoOperator {
    fn name(&self) -> &str {
        "echo"
    }
    fn primary_kind(&self) -> &str {
        "Echo"
    }
    async fn reconcile(
        &self,
        ctx: &OperatorContext,
        req: lkube::operator::ReconcileRequest,
    ) -> anyhow::Result<lkube::operator::ReconcileAction> {
        if let Some(r) = req.resource {
            let msg = r.spec["message"].as_str().unwrap_or_default().to_uppercase();
            ctx.api.update_status(&r.key(), json!({"echo": msg})).await?;
        }
        Ok(lkube::operator::ReconcileAction::Done)
    }
}

#[tokio::test]
async fn operator_manager_runs_custom_and_builtin_operators() {
    let c = Cluster::new().await;
    let api = c.ctx.api.clone();
    api.apply(Resource::new(kinds::CRD, "", "echo", json!({"kind": "Echo"}))).await.unwrap();

    let mut mgr = OperatorManager::new(api.clone());
    for op in controllers::control_plane() {
        mgr.register(op);
    }
    mgr.register(Arc::new(Kubelet::new("node-1", c.runtime.clone())));
    mgr.register(Arc::new(EchoOperator));
    let handles = mgr.start();

    api.apply(Resource::new("Echo", "default", "hi", json!({"message": "hello"}))).await.unwrap();
    api.apply(deployment("web", 2, "nginx")).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let echo = api.get(&ResourceKey::new("Echo", "default", "hi")).await.unwrap();
        let dep: DeploymentStatus =
            api.get(&ResourceKey::new(kinds::DEPLOYMENT, "default", "web")).await.unwrap().status_as();
        if echo.status["echo"] == "HELLO" && dep.ready_replicas == 2 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "did not converge: echo={:?} dep={dep:?}", echo.status);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(c.runtime.running_count(), 2);
    handles.iter().for_each(|h| h.abort());
}
