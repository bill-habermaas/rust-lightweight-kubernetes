use super::{OperatorRecord, Store, UserRecord};
use crate::authz::Role;
use crate::error::{ApiError, Result};
use crate::model::{Resource, ResourceKey};
use async_trait::async_trait;
use bson::{doc, Document};
use futures::TryStreamExt;
use mongodb::options::IndexOptions;
use mongodb::{Client, Collection, Database, IndexModel};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;

/// Resource documents: `{ _id: "<kind>/<ns>/<name>", resource: {...} }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResourceDoc {
    #[serde(rename = "_id")]
    id: String,
    resource: Resource,
}

/// Generic key/value documents for users, roles and operators.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct KvDoc<T> {
    #[serde(rename = "_id")]
    id: String,
    data: T,
}

pub struct MongoStore {
    db: Database,
    resources: Collection<ResourceDoc>,
    users: Collection<KvDoc<UserRecord>>,
    roles: Collection<KvDoc<Role>>,
    operators: Collection<KvDoc<OperatorRecord>>,
    revoked: Collection<Document>,
}

fn is_duplicate_key(e: &mongodb::error::Error) -> bool {
    e.to_string().contains("E11000")
}

impl MongoStore {
    pub async fn connect(uri: &str, db_name: &str) -> anyhow::Result<Self> {
        let client = Client::with_uri_str(uri).await?;
        let db = client.database(db_name);
        db.run_command(doc! { "ping": 1 }).await?;
        let store = Self {
            resources: db.collection("resources"),
            users: db.collection("users"),
            roles: db.collection("roles"),
            operators: db.collection("operators"),
            revoked: db.collection("revoked_tokens"),
            db,
        };
        store
            .resources
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "resource.kind": 1, "resource.metadata.namespace": 1 })
                    .build(),
            )
            .await?;
        // Revoked token ids expire automatically once the token itself would have expired.
        store
            .revoked
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "expire_at": 1 })
                    .options(IndexOptions::builder().expire_after(Duration::from_secs(0)).build())
                    .build(),
            )
            .await?;
        Ok(store)
    }

    pub fn database(&self) -> &Database {
        &self.db
    }

    async fn kv_get<T>(coll: &Collection<KvDoc<T>>, id: &str) -> Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Send + Sync + Unpin,
    {
        Ok(coll.find_one(doc! { "_id": id }).await?.map(|d| d.data))
    }

    async fn kv_put<T>(coll: &Collection<KvDoc<T>>, id: &str, data: T) -> Result<()>
    where
        T: DeserializeOwned + Serialize + Send + Sync + Unpin,
    {
        coll.replace_one(doc! { "_id": id }, KvDoc { id: id.to_string(), data })
            .upsert(true)
            .await?;
        Ok(())
    }

    async fn kv_delete<T>(coll: &Collection<KvDoc<T>>, id: &str) -> Result<bool>
    where
        T: Send + Sync,
    {
        Ok(coll.delete_one(doc! { "_id": id }).await?.deleted_count > 0)
    }

    async fn kv_list<T>(coll: &Collection<KvDoc<T>>) -> Result<Vec<T>>
    where
        T: DeserializeOwned + Serialize + Send + Sync + Unpin,
    {
        let docs: Vec<KvDoc<T>> = coll.find(doc! {}).sort(doc! { "_id": 1 }).await?.try_collect().await?;
        Ok(docs.into_iter().map(|d| d.data).collect())
    }
}

#[async_trait]
impl Store for MongoStore {
    async fn get(&self, key: &ResourceKey) -> Result<Option<Resource>> {
        Ok(self.resources.find_one(doc! { "_id": key.id() }).await?.map(|d| d.resource))
    }

    async fn list(&self, kind: &str, namespace: Option<&str>) -> Result<Vec<Resource>> {
        let mut filter = doc! { "resource.kind": kind };
        if let Some(ns) = namespace {
            filter.insert("resource.metadata.namespace", ns);
        }
        let docs: Vec<ResourceDoc> = self.resources.find(filter).sort(doc! { "_id": 1 }).await?.try_collect().await?;
        Ok(docs.into_iter().map(|d| d.resource).collect())
    }

    async fn create(&self, mut resource: Resource) -> Result<Resource> {
        resource.metadata.resource_version = 1;
        let doc = ResourceDoc { id: resource.key().id(), resource: resource.clone() };
        match self.resources.insert_one(doc).await {
            Ok(_) => Ok(resource),
            Err(e) if is_duplicate_key(&e) => Err(ApiError::AlreadyExists(resource.key().to_string())),
            Err(e) => Err(e.into()),
        }
    }

    async fn update(&self, mut resource: Resource) -> Result<Resource> {
        let id = resource.key().id();
        let expected = resource.metadata.resource_version;
        resource.metadata.resource_version = expected + 1;
        let res = self
            .resources
            .replace_one(
                doc! { "_id": &id, "resource.metadata.resource_version": expected },
                ResourceDoc { id: id.clone(), resource: resource.clone() },
            )
            .await?;
        if res.matched_count == 1 {
            return Ok(resource);
        }
        if self.resources.find_one(doc! { "_id": &id }).await?.is_some() {
            Err(ApiError::Conflict(format!("{} was modified concurrently", resource.key())))
        } else {
            Err(ApiError::NotFound(resource.key().to_string()))
        }
    }

    async fn delete(&self, key: &ResourceKey) -> Result<Option<Resource>> {
        Ok(self.resources.find_one_and_delete(doc! { "_id": key.id() }).await?.map(|d| d.resource))
    }

    async fn get_user(&self, username: &str) -> Result<Option<UserRecord>> {
        Self::kv_get(&self.users, username).await
    }
    async fn put_user(&self, user: UserRecord) -> Result<()> {
        let id = user.username.clone();
        Self::kv_put(&self.users, &id, user).await
    }
    async fn delete_user(&self, username: &str) -> Result<bool> {
        Self::kv_delete(&self.users, username).await
    }
    async fn list_users(&self) -> Result<Vec<UserRecord>> {
        Self::kv_list(&self.users).await
    }

    async fn get_role(&self, name: &str) -> Result<Option<Role>> {
        Self::kv_get(&self.roles, name).await
    }
    async fn put_role(&self, role: Role) -> Result<()> {
        let id = role.name.clone();
        Self::kv_put(&self.roles, &id, role).await
    }
    async fn delete_role(&self, name: &str) -> Result<bool> {
        Self::kv_delete(&self.roles, name).await
    }
    async fn list_roles(&self) -> Result<Vec<Role>> {
        Self::kv_list(&self.roles).await
    }

    async fn revoke_token(&self, token_id: &str, expires_at: i64) -> Result<()> {
        let expire_at = bson::DateTime::from_millis(expires_at.saturating_mul(1000));
        self.revoked
            .replace_one(doc! { "_id": token_id }, doc! { "_id": token_id, "expire_at": expire_at })
            .upsert(true)
            .await?;
        Ok(())
    }
    async fn is_token_revoked(&self, token_id: &str) -> Result<bool> {
        Ok(self.revoked.find_one(doc! { "_id": token_id }).await?.is_some())
    }

    async fn get_operator(&self, name: &str) -> Result<Option<OperatorRecord>> {
        Self::kv_get(&self.operators, name).await
    }
    async fn put_operator(&self, op: OperatorRecord) -> Result<()> {
        let id = op.name.clone();
        Self::kv_put(&self.operators, &id, op).await
    }
    async fn delete_operator(&self, name: &str) -> Result<bool> {
        Self::kv_delete(&self.operators, name).await
    }
    async fn list_operators(&self) -> Result<Vec<OperatorRecord>> {
        Self::kv_list(&self.operators).await
    }
}
