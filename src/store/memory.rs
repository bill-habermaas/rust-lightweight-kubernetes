use super::{OperatorRecord, Store, UserRecord};
use crate::authz::Role;
use crate::error::{ApiError, Result};
use crate::model::{Resource, ResourceKey};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// In-memory store with the same semantics as the MongoDB store.
#[derive(Default)]
pub struct MemoryStore {
    resources: RwLock<HashMap<String, Resource>>,
    users: RwLock<HashMap<String, UserRecord>>,
    roles: RwLock<HashMap<String, Role>>,
    revoked: RwLock<HashMap<String, i64>>,
    operators: RwLock<HashMap<String, OperatorRecord>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn get(&self, key: &ResourceKey) -> Result<Option<Resource>> {
        Ok(self.resources.read().await.get(&key.id()).cloned())
    }

    async fn list(&self, kind: &str, namespace: Option<&str>) -> Result<Vec<Resource>> {
        let guard = self.resources.read().await;
        let mut items: Vec<Resource> = guard
            .values()
            .filter(|r| r.kind == kind && namespace.is_none_or(|ns| r.metadata.namespace == ns))
            .cloned()
            .collect();
        items.sort_by_key(|a| a.key());
        Ok(items)
    }

    async fn create(&self, mut resource: Resource) -> Result<Resource> {
        let id = resource.key().id();
        let mut guard = self.resources.write().await;
        if guard.contains_key(&id) {
            return Err(ApiError::AlreadyExists(resource.key().to_string()));
        }
        resource.metadata.resource_version = 1;
        guard.insert(id, resource.clone());
        Ok(resource)
    }

    async fn update(&self, mut resource: Resource) -> Result<Resource> {
        let id = resource.key().id();
        let mut guard = self.resources.write().await;
        let current = guard.get(&id).ok_or_else(|| ApiError::NotFound(resource.key().to_string()))?;
        if current.metadata.resource_version != resource.metadata.resource_version {
            return Err(ApiError::Conflict(format!(
                "{} was modified (have version {}, current {})",
                resource.key(),
                resource.metadata.resource_version,
                current.metadata.resource_version
            )));
        }
        resource.metadata.resource_version += 1;
        guard.insert(id, resource.clone());
        Ok(resource)
    }

    async fn delete(&self, key: &ResourceKey) -> Result<Option<Resource>> {
        Ok(self.resources.write().await.remove(&key.id()))
    }

    async fn get_user(&self, username: &str) -> Result<Option<UserRecord>> {
        Ok(self.users.read().await.get(username).cloned())
    }
    async fn put_user(&self, user: UserRecord) -> Result<()> {
        self.users.write().await.insert(user.username.clone(), user);
        Ok(())
    }
    async fn delete_user(&self, username: &str) -> Result<bool> {
        Ok(self.users.write().await.remove(username).is_some())
    }
    async fn list_users(&self) -> Result<Vec<UserRecord>> {
        let mut v: Vec<_> = self.users.read().await.values().cloned().collect();
        v.sort_by(|a, b| a.username.cmp(&b.username));
        Ok(v)
    }

    async fn get_role(&self, name: &str) -> Result<Option<Role>> {
        Ok(self.roles.read().await.get(name).cloned())
    }
    async fn put_role(&self, role: Role) -> Result<()> {
        self.roles.write().await.insert(role.name.clone(), role);
        Ok(())
    }
    async fn delete_role(&self, name: &str) -> Result<bool> {
        Ok(self.roles.write().await.remove(name).is_some())
    }
    async fn list_roles(&self) -> Result<Vec<Role>> {
        let mut v: Vec<_> = self.roles.read().await.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    async fn revoke_token(&self, token_id: &str, expires_at: i64) -> Result<()> {
        self.revoked.write().await.insert(token_id.into(), expires_at);
        Ok(())
    }
    async fn is_token_revoked(&self, token_id: &str) -> Result<bool> {
        Ok(self.revoked.read().await.contains_key(token_id))
    }

    async fn get_operator(&self, name: &str) -> Result<Option<OperatorRecord>> {
        Ok(self.operators.read().await.get(name).cloned())
    }
    async fn put_operator(&self, op: OperatorRecord) -> Result<()> {
        self.operators.write().await.insert(op.name.clone(), op);
        Ok(())
    }
    async fn delete_operator(&self, name: &str) -> Result<bool> {
        Ok(self.operators.write().await.remove(name).is_some())
    }
    async fn list_operators(&self) -> Result<Vec<OperatorRecord>> {
        let mut v: Vec<_> = self.operators.read().await.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::kinds;
    use serde_json::json;

    #[tokio::test]
    async fn create_get_list_delete() {
        let s = MemoryStore::new();
        let r = Resource::new(kinds::POD, "default", "a", json!({}));
        let saved = s.create(r.clone()).await.unwrap();
        assert_eq!(saved.metadata.resource_version, 1);
        assert!(matches!(s.create(r.clone()).await, Err(ApiError::AlreadyExists(_))));
        s.create(Resource::new(kinds::POD, "other", "b", json!({}))).await.unwrap();

        assert_eq!(s.list(kinds::POD, None).await.unwrap().len(), 2);
        assert_eq!(s.list(kinds::POD, Some("default")).await.unwrap().len(), 1);
        assert_eq!(s.list(kinds::DEPLOYMENT, None).await.unwrap().len(), 0);

        assert!(s.delete(&r.key()).await.unwrap().is_some());
        assert!(s.get(&r.key()).await.unwrap().is_none());
        assert!(s.delete(&r.key()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn optimistic_concurrency() {
        let s = MemoryStore::new();
        let v1 = s.create(Resource::new(kinds::POD, "default", "a", json!({}))).await.unwrap();
        let v2 = s.update(v1.clone()).await.unwrap();
        assert_eq!(v2.metadata.resource_version, 2);
        // writing with the stale version fails
        assert!(matches!(s.update(v1).await, Err(ApiError::Conflict(_))));
        let missing = Resource::new(kinds::POD, "default", "missing", json!({}));
        assert!(matches!(s.update(missing).await, Err(ApiError::NotFound(_))));
    }

    #[tokio::test]
    async fn token_revocation() {
        let s = MemoryStore::new();
        assert!(!s.is_token_revoked("t1").await.unwrap());
        s.revoke_token("t1", 0).await.unwrap();
        assert!(s.is_token_revoked("t1").await.unwrap());
    }
}
