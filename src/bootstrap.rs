//! First-start initialisation: built-in roles, the initial admin user and the
//! `default` namespace.

use crate::api::Api;
use crate::auth::hash_password;
use crate::authz::{builtin_roles, roles};
use crate::error::{ApiError, Result};
use crate::model::{kinds, now_rfc3339, Resource, DEFAULT_NAMESPACE};
use crate::store::{Store, UserRecord};
use serde_json::json;

pub async fn ensure_builtin_roles(store: &dyn Store) -> Result<()> {
    for role in builtin_roles() {
        store.put_role(role).await?;
    }
    Ok(())
}

/// Create the initial admin user if no users exist yet.
pub async fn ensure_admin(store: &dyn Store, username: &str, password: &str) -> Result<bool> {
    if !store.list_users().await?.is_empty() {
        return Ok(false);
    }
    store
        .put_user(UserRecord {
            username: username.into(),
            password_hash: hash_password(password)?,
            roles: vec![roles::ADMIN.into()],
            created_at: now_rfc3339(),
        })
        .await?;
    tracing::warn!(user = username, "created initial admin user — change its password");
    Ok(true)
}

pub async fn ensure_default_namespace(api: &Api) -> Result<()> {
    match api.create(Resource::new(kinds::NAMESPACE, "", DEFAULT_NAMESPACE, json!({}))).await {
        Ok(_) | Err(ApiError::AlreadyExists(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn run(api: &Api, admin_user: &str, admin_password: &str) -> Result<()> {
    ensure_builtin_roles(api.store().as_ref()).await?;
    ensure_admin(api.store().as_ref(), admin_user, admin_password).await?;
    ensure_default_namespace(api).await
}
