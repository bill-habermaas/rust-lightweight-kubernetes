//! Request guards: authenticate the bearer token, then authorize (verb, kind, namespace).

use crate::auth::{bearer_token, Claims, TokenKind, TokenManager};
use crate::authz::{builtin_role, is_allowed, roles, Role};
use crate::error::{ApiError, Result};
use crate::store::Store;
use std::sync::Arc;
use tonic::metadata::MetadataMap;

#[derive(Debug, Clone)]
pub struct Principal {
    pub subject: String,
    pub roles: Vec<String>,
    pub claims: Claims,
}

impl Principal {
    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == roles::ADMIN)
    }
}

pub struct Security {
    pub store: Arc<dyn Store>,
    pub tokens: Arc<TokenManager>,
}

impl Security {
    pub fn new(store: Arc<dyn Store>, tokens: Arc<TokenManager>) -> Self {
        Self { store, tokens }
    }

    pub async fn authenticate(&self, md: &MetadataMap) -> Result<Principal> {
        let token = bearer_token(md)?;
        self.authenticate_token(&token).await
    }

    pub async fn authenticate_token(&self, token: &str) -> Result<Principal> {
        let claims = self.tokens.verify(token)?;
        if self.store.is_token_revoked(&claims.jti).await? {
            return Err(ApiError::Unauthenticated("token has been revoked".into()));
        }
        // Session tokens always reflect the user's *current* roles, so role changes and
        // user deletion take effect immediately. API/operator tokens carry fixed roles.
        let roles = match claims.kind {
            TokenKind::Session => {
                self.store
                    .get_user(&claims.sub)
                    .await?
                    .ok_or_else(|| ApiError::Unauthenticated("user no longer exists".into()))?
                    .roles
            }
            _ => claims.roles.clone(),
        };
        Ok(Principal { subject: claims.sub.clone(), roles, claims })
    }

    pub async fn load_roles(&self, p: &Principal) -> Result<Vec<Role>> {
        let mut out = Vec::with_capacity(p.roles.len());
        for name in &p.roles {
            if let Some(r) = self.store.get_role(name).await? {
                out.push(r);
            } else if let Some(r) = builtin_role(name) {
                out.push(r);
            }
        }
        Ok(out)
    }

    pub async fn authorize(&self, p: &Principal, verb: &str, kind: &str, namespace: &str) -> Result<()> {
        let roles = self.load_roles(p).await?;
        if is_allowed(&roles, verb, kind, namespace) {
            Ok(())
        } else {
            let scope = if namespace.is_empty() { String::new() } else { format!(" in namespace '{namespace}'") };
            Err(ApiError::Forbidden(format!("'{}' is not allowed to {verb} {kind}{scope}", p.subject)))
        }
    }
}
