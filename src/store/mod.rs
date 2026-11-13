//! Persistence. [`MongoStore`] is used in production; [`MemoryStore`] backs unit tests
//! and the `LKUBE_STORE=memory` mode.

pub mod memory;
pub mod mongo;

pub use memory::MemoryStore;
pub use mongo::MongoStore;

use crate::authz::Role;
use crate::error::Result;
use crate::model::{Resource, ResourceKey};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    pub username: String,
    pub password_hash: String,
    pub roles: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorRecord {
    pub name: String,
    pub kinds: Vec<String>,
    pub role: String,
    pub token_id: String,
    pub token_expires_at: i64,
    pub registered_at: String,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    // --- resources -------------------------------------------------------
    async fn get(&self, key: &ResourceKey) -> Result<Option<Resource>>;
    /// `namespace = None` lists across all namespaces.
    async fn list(&self, kind: &str, namespace: Option<&str>) -> Result<Vec<Resource>>;
    /// Insert a new object. Fails with `AlreadyExists`. Sets `resource_version = 1`.
    async fn create(&self, resource: Resource) -> Result<Resource>;
    /// Compare-and-swap on `metadata.resource_version`. Fails with `Conflict` if stale,
    /// `NotFound` if missing. Increments the resource version.
    async fn update(&self, resource: Resource) -> Result<Resource>;
    async fn delete(&self, key: &ResourceKey) -> Result<Option<Resource>>;

    // --- identity --------------------------------------------------------
    async fn get_user(&self, username: &str) -> Result<Option<UserRecord>>;
    async fn put_user(&self, user: UserRecord) -> Result<()>;
    async fn delete_user(&self, username: &str) -> Result<bool>;
    async fn list_users(&self) -> Result<Vec<UserRecord>>;

    async fn get_role(&self, name: &str) -> Result<Option<Role>>;
    async fn put_role(&self, role: Role) -> Result<()>;
    async fn delete_role(&self, name: &str) -> Result<bool>;
    async fn list_roles(&self) -> Result<Vec<Role>>;

    async fn revoke_token(&self, token_id: &str, expires_at: i64) -> Result<()>;
    async fn is_token_revoked(&self, token_id: &str) -> Result<bool>;

    // --- operators -------------------------------------------------------
    async fn get_operator(&self, name: &str) -> Result<Option<OperatorRecord>>;
    async fn put_operator(&self, op: OperatorRecord) -> Result<()>;
    async fn delete_operator(&self, name: &str) -> Result<bool>;
    async fn list_operators(&self) -> Result<Vec<OperatorRecord>>;
}
