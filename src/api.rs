//! The API machinery: validation + defaulting, create/apply/update with optimistic
//! concurrency, cascading deletes (owner references, namespaces, CRDs) and watch events.
//! Authorization is NOT done here — callers (gRPC layer) authorize before calling in;
//! controllers call in with system privileges.

use crate::error::{ApiError, Result};
use crate::model::*;
use crate::store::Store;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub event_type: EventType,
    pub resource: Resource,
}

pub struct Api {
    store: Arc<dyn Store>,
    events: broadcast::Sender<WatchEvent>,
}

const MAX_RETRIES: usize = 8;

impl Api {
    pub fn new(store: Arc<dyn Store>) -> Arc<Self> {
        let (events, _) = broadcast::channel(4096);
        Arc::new(Self { store, events })
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event_type: EventType, resource: &Resource) {
        let _ = self.events.send(WatchEvent { event_type, resource: resource.clone() });
    }

    // --- kinds -------------------------------------------------------------

    pub async fn list_kinds(&self) -> Result<Vec<KindInfo>> {
        let mut out = builtin_kinds();
        for crd in self.store.list(kinds::CRD, None).await? {
            if let Ok(spec) = crd.spec_as::<CrdSpec>() {
                out.push(KindInfo {
                    kind: spec.kind,
                    namespaced: spec.namespaced,
                    builtin: false,
                    api_version: if spec.api_version.is_empty() { API_VERSION.into() } else { spec.api_version },
                    schema: spec.schema,
                });
            }
        }
        Ok(out)
    }

    pub async fn kind_info(&self, kind: &str) -> Result<KindInfo> {
        self.list_kinds()
            .await?
            .into_iter()
            .find(|k| k.kind == kind)
            .ok_or_else(|| ApiError::Invalid(format!("unknown kind '{kind}'")))
    }

    /// Resolve the namespace a request refers to: `default` for namespaced kinds when
    /// empty, and always empty for cluster-scoped kinds.
    pub async fn effective_namespace(&self, kind: &str, namespace: &str) -> Result<String> {
        let info = self.kind_info(kind).await?;
        Ok(match (info.namespaced, namespace.is_empty()) {
            (false, _) => String::new(),
            (true, true) => DEFAULT_NAMESPACE.into(),
            (true, false) => namespace.into(),
        })
    }

    // --- reads -------------------------------------------------------------

    pub async fn get(&self, key: &ResourceKey) -> Result<Resource> {
        self.store.get(key).await?.ok_or_else(|| ApiError::NotFound(key.to_string()))
    }

    pub async fn try_get(&self, key: &ResourceKey) -> Result<Option<Resource>> {
        self.store.get(key).await
    }

    pub async fn list(&self, kind: &str, namespace: Option<&str>, selector: &Labels) -> Result<Vec<Resource>> {
        let mut items = self.store.list(kind, namespace.filter(|n| !n.is_empty())).await?;
        items.retain(|r| r.matches_selector(selector));
        items.sort_by_key(|a| a.key());
        Ok(items)
    }

    // --- writes ------------------------------------------------------------

    /// Validate and default an object in place.
    async fn prepare(&self, r: &mut Resource) -> Result<KindInfo> {
        let info = self.kind_info(&r.kind).await?;
        validate_name(&r.metadata.name, "name")?;
        if info.namespaced {
            if r.metadata.namespace.is_empty() {
                r.metadata.namespace = DEFAULT_NAMESPACE.into();
            }
            validate_name(&r.metadata.namespace, "namespace")?;
            let ns_key = ResourceKey::new(kinds::NAMESPACE, "", &r.metadata.namespace);
            if self.store.get(&ns_key).await?.is_none() {
                return Err(ApiError::Invalid(format!("namespace '{}' does not exist", r.metadata.namespace)));
            }
        } else {
            r.metadata.namespace.clear();
        }
        // The kind determines the canonical apiVersion.
        r.api_version = info.api_version.clone();
        if r.spec.is_null() {
            r.spec = json!({});
        }
        validate_spec(&info, r)?;
        Ok(info)
    }

    /// Create a new object (fails if it exists).
    pub async fn create(&self, mut r: Resource) -> Result<Resource> {
        self.prepare(&mut r).await?;
        self.insert(r).await
    }

    async fn insert(&self, mut r: Resource) -> Result<Resource> {
        r.metadata.uid = uuid::Uuid::new_v4().to_string();
        r.metadata.created_at = now_rfc3339();
        r.metadata.generation = 1;
        r.metadata.resource_version = 0;
        let saved = self.store.create(r).await?;
        self.emit(EventType::Added, &saved);
        Ok(saved)
    }

    /// Declarative create-or-update (like `kubectl apply`). Status is preserved;
    /// generation is bumped when the spec changes.
    pub async fn apply(&self, mut r: Resource) -> Result<Resource> {
        self.prepare(&mut r).await?;
        for _ in 0..MAX_RETRIES {
            match self.store.get(&r.key()).await? {
                None => match self.insert(r.clone()).await {
                    Err(ApiError::AlreadyExists(_)) => continue,
                    other => return other,
                },
                Some(existing) => {
                    let mut updated = existing.clone();
                    updated.api_version = r.api_version.clone();
                    updated.spec = r.spec.clone();
                    updated.metadata.labels = r.metadata.labels.clone();
                    updated.metadata.annotations = r.metadata.annotations.clone();
                    if !r.metadata.owner_uid.is_empty() {
                        updated.metadata.owner_uid = r.metadata.owner_uid.clone();
                    }
                    if updated == existing {
                        return Ok(existing);
                    }
                    if updated.spec != existing.spec {
                        updated.metadata.generation += 1;
                    }
                    match self.store.update(updated).await {
                        Ok(saved) => {
                            self.emit(EventType::Modified, &saved);
                            return Ok(saved);
                        }
                        Err(ApiError::Conflict(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Err(ApiError::Conflict(format!("{}: too many concurrent modifications", r.key())))
    }

    /// Full replacement guarded by `resource_version` (used by controllers).
    pub async fn update(&self, mut r: Resource) -> Result<Resource> {
        self.prepare(&mut r).await?;
        let saved = self.store.update(r).await?;
        self.emit(EventType::Modified, &saved);
        Ok(saved)
    }

    /// Replace only the status sub-object, retrying on conflicts.
    pub async fn update_status(&self, key: &ResourceKey, status: Value) -> Result<Resource> {
        for _ in 0..MAX_RETRIES {
            let mut current = self.get(key).await?;
            if current.status == status {
                return Ok(current);
            }
            current.status = status.clone();
            match self.store.update(current).await {
                Ok(saved) => {
                    self.emit(EventType::Modified, &saved);
                    return Ok(saved);
                }
                Err(ApiError::Conflict(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(ApiError::Conflict(format!("{key}: too many concurrent modifications")))
    }

    /// Mutate the spec with `f`, retrying on conflicts.
    pub async fn patch_spec<F>(&self, key: &ResourceKey, mut f: F) -> Result<Resource>
    where
        F: FnMut(&mut Value) -> Result<()> + Send,
    {
        for _ in 0..MAX_RETRIES {
            let mut current = self.get(key).await?;
            let before = current.spec.clone();
            f(&mut current.spec)?;
            if current.spec == before {
                return Ok(current);
            }
            current.metadata.generation += 1;
            let info = self.kind_info(&current.kind).await?;
            validate_spec(&info, &mut current)?;
            match self.store.update(current).await {
                Ok(saved) => {
                    self.emit(EventType::Modified, &saved);
                    return Ok(saved);
                }
                Err(ApiError::Conflict(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(ApiError::Conflict(format!("{key}: too many concurrent modifications")))
    }

    pub async fn scale(&self, namespace: &str, name: &str, replicas: i32) -> Result<Resource> {
        if replicas < 0 {
            return Err(ApiError::Invalid("replicas must be >= 0".into()));
        }
        let ns = if namespace.is_empty() { DEFAULT_NAMESPACE } else { namespace };
        let key = ResourceKey::new(kinds::DEPLOYMENT, ns, name);
        self.patch_spec(&key, |spec| {
            spec["replicas"] = json!(replicas);
            Ok(())
        })
        .await
    }

    /// Delete an object and, recursively, everything it owns.
    pub fn delete<'a>(&'a self, key: &'a ResourceKey) -> BoxFuture<'a, Result<Resource>> {
        Box::pin(async move {
            if key.kind == kinds::NAMESPACE && key.name == DEFAULT_NAMESPACE {
                return Err(ApiError::Invalid("the default namespace cannot be deleted".into()));
            }
            let removed = self.store.delete(key).await?.ok_or_else(|| ApiError::NotFound(key.to_string()))?;
            self.emit(EventType::Deleted, &removed);
            self.cascade(&removed).await?;
            Ok(removed)
        })
    }

    async fn cascade(&self, parent: &Resource) -> Result<()> {
        // Deleting a CRD removes all of its instances.
        if parent.kind == kinds::CRD {
            if let Ok(spec) = parent.spec_as::<CrdSpec>() {
                for child in self.store.list(&spec.kind, None).await? {
                    let _ = self.delete(&child.key()).await;
                }
            }
            return Ok(());
        }
        let all_kinds = self.list_kinds().await?;
        // Deleting a namespace removes everything inside it.
        if parent.kind == kinds::NAMESPACE {
            for k in all_kinds.iter().filter(|k| k.namespaced) {
                for child in self.store.list(&k.kind, Some(&parent.metadata.name)).await? {
                    let _ = self.delete(&child.key()).await;
                }
            }
            return Ok(());
        }
        // Owner references.
        let uid = &parent.metadata.uid;
        if uid.is_empty() {
            return Ok(());
        }
        for k in &all_kinds {
            let ns = if k.namespaced && !parent.metadata.namespace.is_empty() {
                Some(parent.metadata.namespace.as_str())
            } else {
                None
            };
            for child in self.store.list(&k.kind, ns).await? {
                if &child.metadata.owner_uid == uid {
                    let _ = self.delete(&child.key()).await;
                }
            }
        }
        Ok(())
    }
}

/// Kind specific validation and defaulting.
pub fn validate_spec(info: &KindInfo, r: &mut Resource) -> Result<()> {
    match r.kind.as_str() {
        kinds::POD => validate_pod_spec(&r.spec_as::<PodSpec>()?),
        kinds::DEPLOYMENT => {
            let mut spec: DeploymentSpec = r.spec_as()?;
            if spec.replicas < 0 {
                return Err(ApiError::Invalid("replicas must be >= 0".into()));
            }
            validate_pod_spec(&spec.template.spec)?;
            if spec.selector.is_empty() {
                spec.selector = spec.template.labels.clone();
            }
            if spec.selector.is_empty() {
                return Err(ApiError::Invalid("deployment needs spec.selector or spec.template.labels".into()));
            }
            if !spec.selector.iter().all(|(k, v)| spec.template.labels.get(k) == Some(v)) {
                return Err(ApiError::Invalid("spec.selector must match spec.template.labels".into()));
            }
            r.spec = serde_json::to_value(&spec)?;
            Ok(())
        }
        kinds::SERVICE => {
            let spec: ServiceSpec = r.spec_as()?;
            if spec.ports.iter().any(|p| p.port == 0) {
                return Err(ApiError::Invalid("service ports must be > 0".into()));
            }
            Ok(())
        }
        kinds::NODE => r.spec_as::<NodeSpec>().map(|_| ()),
        kinds::CONFIG_MAP => r.spec_as::<ConfigMapSpec>().map(|_| ()),
        kinds::NAMESPACE => Ok(()),
        kinds::CRD => {
            let spec: CrdSpec = r.spec_as()?;
            validate_kind_name(&spec.kind)?;
            if is_builtin_kind(&spec.kind) {
                return Err(ApiError::Invalid(format!("'{}' is a built-in kind", spec.kind)));
            }
            if r.metadata.name != spec.kind.to_lowercase() {
                return Err(ApiError::Invalid(format!(
                    "CustomResourceDefinition name must be '{}'",
                    spec.kind.to_lowercase()
                )));
            }
            if !spec.schema.is_null() && !spec.schema.is_object() {
                return Err(ApiError::Invalid("schema must be a JSON object".into()));
            }
            Ok(())
        }
        _ => validate_against_schema(&r.spec, &info.schema),
    }
}

fn validate_pod_spec(spec: &PodSpec) -> Result<()> {
    if spec.containers.is_empty() {
        return Err(ApiError::Invalid("pod needs at least one container".into()));
    }
    let mut names = std::collections::HashSet::new();
    for c in &spec.containers {
        validate_name(&c.name, "container name")?;
        if c.image.trim().is_empty() {
            return Err(ApiError::Invalid(format!("container '{}' needs an image", c.name)));
        }
        if !names.insert(&c.name) {
            return Err(ApiError::Invalid(format!("duplicate container name '{}'", c.name)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    async fn api() -> Arc<Api> {
        let api = Api::new(Arc::new(MemoryStore::new()));
        api.create(Resource::new(kinds::NAMESPACE, "", "default", json!({}))).await.unwrap();
        api
    }

    fn pod(name: &str) -> Resource {
        Resource::new(kinds::POD, "default", name, json!({"containers": [{"name": "c", "image": "nginx"}]}))
    }

    fn deployment(name: &str, replicas: i32) -> Resource {
        Resource::new(
            kinds::DEPLOYMENT,
            "default",
            name,
            json!({"replicas": replicas, "template": {"labels": {"app": name}, "spec": {"containers": [{"name": "c", "image": "nginx"}]}}}),
        )
    }

    #[tokio::test]
    async fn create_assigns_metadata_and_emits_event() {
        let api = api().await;
        let mut rx = api.subscribe();
        let p = api.create(pod("p1")).await.unwrap();
        assert!(!p.metadata.uid.is_empty());
        assert_eq!(p.metadata.generation, 1);
        assert_eq!(p.metadata.resource_version, 1);
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.event_type, EventType::Added);
        assert_eq!(ev.resource.metadata.name, "p1");
    }

    #[tokio::test]
    async fn rejects_unknown_kind_and_missing_namespace() {
        let api = api().await;
        let r = Resource::new("Nope", "default", "x", json!({}));
        assert!(matches!(api.create(r).await, Err(ApiError::Invalid(_))));
        let mut p = pod("p");
        p.metadata.namespace = "missing".into();
        assert!(matches!(api.create(p).await, Err(ApiError::Invalid(_))));
    }

    #[tokio::test]
    async fn rejects_invalid_pods() {
        let api = api().await;
        let r = Resource::new(kinds::POD, "default", "p", json!({"containers": []}));
        assert!(api.create(r).await.is_err());
        let r = Resource::new(kinds::POD, "default", "p", json!({"containers": [{"name": "c", "image": ""}]}));
        assert!(api.create(r).await.is_err());
        let r = Resource::new(
            kinds::POD,
            "default",
            "p",
            json!({"containers": [{"name": "c", "image": "a"}, {"name": "c", "image": "b"}]}),
        );
        assert!(api.create(r).await.is_err());
    }

    #[tokio::test]
    async fn apply_creates_then_updates_and_bumps_generation() {
        let api = api().await;
        let d1 = api.apply(deployment("web", 1)).await.unwrap();
        assert_eq!(d1.metadata.generation, 1);
        // defaulted selector
        assert_eq!(d1.spec["selector"]["app"], "web");
        // identical apply is a no-op
        let same = api.apply(deployment("web", 1)).await.unwrap();
        assert_eq!(same.metadata.resource_version, d1.metadata.resource_version);
        let d2 = api.apply(deployment("web", 3)).await.unwrap();
        assert_eq!(d2.metadata.generation, 2);
        assert_eq!(d2.metadata.uid, d1.metadata.uid);
        assert_eq!(d2.spec["replicas"], 3);
    }

    #[tokio::test]
    async fn apply_preserves_status() {
        let api = api().await;
        let d = api.apply(deployment("web", 1)).await.unwrap();
        api.update_status(&d.key(), json!({"replicas": 1})).await.unwrap();
        let d2 = api.apply(deployment("web", 2)).await.unwrap();
        assert_eq!(d2.status["replicas"], 1);
    }

    #[tokio::test]
    async fn selector_must_match_template() {
        let api = api().await;
        let r = Resource::new(
            kinds::DEPLOYMENT,
            "default",
            "bad",
            json!({"selector": {"app": "x"}, "template": {"labels": {"app": "y"}, "spec": {"containers": [{"name": "c", "image": "i"}]}}}),
        );
        assert!(matches!(api.apply(r).await, Err(ApiError::Invalid(_))));
    }

    #[tokio::test]
    async fn scale_updates_replicas() {
        let api = api().await;
        api.apply(deployment("web", 1)).await.unwrap();
        let d = api.scale("default", "web", 5).await.unwrap();
        assert_eq!(d.spec["replicas"], 5);
        assert_eq!(d.metadata.generation, 2);
        assert!(api.scale("default", "web", -1).await.is_err());
        assert!(matches!(api.scale("default", "nope", 1).await, Err(ApiError::NotFound(_))));
    }

    #[tokio::test]
    async fn delete_cascades_to_owned_objects() {
        let api = api().await;
        let d = api.apply(deployment("web", 1)).await.unwrap();
        let mut p = pod("web-abc");
        p.metadata.owner_uid = d.metadata.uid.clone();
        api.create(p).await.unwrap();
        api.create(pod("unrelated")).await.unwrap();
        api.delete(&d.key()).await.unwrap();
        let pods = api.list(kinds::POD, None, &Labels::new()).await.unwrap();
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0].metadata.name, "unrelated");
    }

    #[tokio::test]
    async fn namespace_delete_cascades() {
        let api = api().await;
        api.create(Resource::new(kinds::NAMESPACE, "", "dev", json!({}))).await.unwrap();
        let mut p = pod("p");
        p.metadata.namespace = "dev".into();
        api.create(p).await.unwrap();
        api.create(pod("keep")).await.unwrap();
        api.delete(&ResourceKey::new(kinds::NAMESPACE, "", "dev")).await.unwrap();
        assert_eq!(api.list(kinds::POD, None, &Labels::new()).await.unwrap().len(), 1);
        assert!(api.delete(&ResourceKey::new(kinds::NAMESPACE, "", "default")).await.is_err());
    }

    #[tokio::test]
    async fn custom_resources_via_crd() {
        let api = api().await;
        let crd = Resource::new(
            kinds::CRD,
            "",
            "webapp",
            json!({"kind": "WebApp", "namespaced": true, "apiVersion": "example.com/v1", "schema": {"required": ["image"]}}),
        );
        api.apply(crd).await.unwrap();
        assert!(api.kind_info("WebApp").await.unwrap().namespaced);

        let bad = Resource::new("WebApp", "default", "site", json!({"replicas": 2}));
        assert!(matches!(api.apply(bad).await, Err(ApiError::Invalid(_))));
        let ok = api.apply(Resource::new("WebApp", "default", "site", json!({"image": "nginx"}))).await.unwrap();
        assert_eq!(ok.api_version, "example.com/v1");

        // deleting the CRD removes its instances
        api.delete(&ResourceKey::new(kinds::CRD, "", "webapp")).await.unwrap();
        assert!(api.store().list("WebApp", None).await.unwrap().is_empty());
        assert!(api.kind_info("WebApp").await.is_err());
    }

    #[tokio::test]
    async fn crd_validation() {
        let api = api().await;
        let wrong_name = Resource::new(kinds::CRD, "", "foo", json!({"kind": "WebApp"}));
        assert!(api.apply(wrong_name).await.is_err());
        let builtin = Resource::new(kinds::CRD, "", "pod", json!({"kind": "Pod"}));
        assert!(api.apply(builtin).await.is_err());
    }

    #[tokio::test]
    async fn cluster_scoped_kinds_drop_namespace() {
        let api = api().await;
        let mut n = Resource::new(kinds::NODE, "default", "node-1", json!({}));
        n.metadata.namespace = "ignored".into();
        let saved = api.create(n).await.unwrap();
        assert_eq!(saved.metadata.namespace, "");
        assert_eq!(api.effective_namespace(kinds::POD, "").await.unwrap(), "default");
        assert_eq!(api.effective_namespace(kinds::NODE, "x").await.unwrap(), "");
    }
}
