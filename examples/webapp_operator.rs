//! Example out-of-process operator.
//!
//! Registers a `WebApp` CustomResourceDefinition and turns every WebApp into a
//! Deployment + Service (owned by the WebApp, so they are garbage-collected with it).
//!
//! ```bash
//! LKUBE_ENDPOINT=http://localhost:50051 LKUBE_USER=admin LKUBE_PASSWORD=admin12345 \
//!   cargo run --example webapp_operator
//! ```
//! Then apply:
//! ```json
//! {"kind":"WebApp","metadata":{"name":"hello"},"spec":{"image":"nginx:1.27","replicas":2,"port":80}}
//! ```

use lkube::pb;
use lkube::sdk::{run_remote_operator, LkubeClient, RemoteReconciler};
use serde_json::{json, Value};

struct WebAppReconciler;

#[async_trait::async_trait]
impl RemoteReconciler for WebAppReconciler {
    async fn reconcile(&self, client: &mut LkubeClient, ev: pb::WatchEvent) -> anyhow::Result<()> {
        let Some(app) = ev.resource else { return Ok(()) };
        if ev.r#type == pb::EventType::Deleted as i32 {
            // Children carry owner_uid = app uid; the server garbage-collects them.
            return Ok(());
        }
        let meta = app.metadata.clone().unwrap_or_default();
        let spec: Value = serde_json::from_str(&app.spec_json)?;
        let image = spec["image"].as_str().unwrap_or("nginx:1.27");
        let replicas = spec["replicas"].as_i64().unwrap_or(1);
        let port = spec["port"].as_u64().unwrap_or(80);

        let mut deployment = LkubeClient::resource(
            "Deployment",
            &meta.namespace,
            &meta.name,
            json!({
                "replicas": replicas,
                "template": {
                    "labels": {"app": meta.name, "lkube.io/managed-by": "webapp-operator"},
                    "spec": {"containers": [{"name": "web", "image": image, "ports": [{"containerPort": port}]}]}
                }
            }),
        );
        deployment.metadata.as_mut().unwrap().owner_uid = meta.uid.clone();
        client.cluster.apply(pb::ApplyRequest { resource: Some(deployment) }).await?;

        let mut service = LkubeClient::resource(
            "Service",
            &meta.namespace,
            &meta.name,
            json!({"selector": {"app": meta.name}, "ports": [{"port": port}]}),
        );
        service.metadata.as_mut().unwrap().owner_uid = meta.uid.clone();
        client.cluster.apply(pb::ApplyRequest { resource: Some(service) }).await?;

        // Report status back on the custom resource.
        let dep = client
            .cluster
            .get(pb::ResourceRef { kind: "Deployment".into(), namespace: meta.namespace.clone(), name: meta.name.clone() })
            .await?
            .into_inner();
        let dep_status: Value = serde_json::from_str(if dep.status_json.is_empty() { "{}" } else { &dep.status_json })?;
        let status = json!({
            "phase": if dep_status["readyReplicas"].as_i64() == Some(replicas) { "Ready" } else { "Progressing" },
            "readyReplicas": dep_status["readyReplicas"],
            "deployment": meta.name,
            "service": meta.name,
        });
        client
            .cluster
            .update_status(pb::UpdateStatusRequest {
                target: Some(pb::ResourceRef { kind: "WebApp".into(), namespace: meta.namespace, name: meta.name }),
                status_json: status.to_string(),
            })
            .await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let endpoint = std::env::var("LKUBE_ENDPOINT").unwrap_or_else(|_| "http://localhost:50051".into());

    // Use a pre-issued operator token if given, otherwise register with admin credentials.
    let token = match std::env::var("LKUBE_OPERATOR_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            let user = std::env::var("LKUBE_USER").unwrap_or_else(|_| "admin".into());
            let pw = std::env::var("LKUBE_PASSWORD").unwrap_or_else(|_| "admin12345".into());
            let admin_token = LkubeClient::login(&endpoint, &user, &pw).await?;
            let mut admin = LkubeClient::connect(&endpoint, &admin_token).await?;
            let resp = admin
                .operators
                .register(pb::RegisterOperatorRequest {
                    name: "webapp-operator".into(),
                    crds: vec![pb::CrdSpec {
                        kind: "WebApp".into(),
                        namespaced: true,
                        api_version: "example.com/v1".into(),
                        schema_json: json!({
                            "required": ["image"],
                            "properties": {"image": {"type": "string"}, "replicas": {"type": "integer"}, "port": {"type": "integer"}}
                        })
                        .to_string(),
                    }],
                    extra_rules: vec![pb::PolicyRule {
                        verbs: vec!["get".into(), "list".into(), "create".into(), "update".into()],
                        kinds: vec!["Deployment".into(), "Service".into()],
                        namespaces: vec![],
                    }],
                    token_ttl_seconds: 0,
                })
                .await?
                .into_inner();
            tracing::info!("registered webapp-operator");
            resp.token
        }
    };

    let client = LkubeClient::connect(&endpoint, &token).await?;
    // Watch Deployments too, so WebApp status follows readiness changes.
    run_remote_operator(client, vec!["WebApp".into()], WebAppReconciler).await
}
