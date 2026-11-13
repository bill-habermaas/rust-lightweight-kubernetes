//! gRPC service implementations. Every command (except Login) authenticates the
//! bearer token and authorizes (verb, kind, namespace) before touching the API.

use crate::api::Api;
use crate::auth::{hash_password, verify_password, TokenKind, TokenManager};
use crate::authz::{builtin_role, is_allowed, roles, system_kinds, verbs, PolicyRule, Role, WILDCARD};
use crate::convert::*;
use crate::error::ApiError;
use crate::model::*;
use crate::pb::{
    self,
    admin_service_server::{AdminService, AdminServiceServer},
    auth_service_server::{AuthService, AuthServiceServer},
    cluster_service_server::{ClusterService, ClusterServiceServer},
    operator_service_server::{OperatorService, OperatorServiceServer},
};
use crate::runtime::ContainerRuntime;
use crate::security::{Principal, Security};
use crate::store::{OperatorRecord, UserRecord};
use serde_json::json;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{broadcast::error::RecvError, mpsc};
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{Request, Response, Status};
use tower_http::cors::{Any, CorsLayer};

pub const OPERATOR_TOKEN_TTL: i64 = 365 * 24 * 3600;

/// Information about an operator running inside the server process.
#[derive(Debug, Clone)]
pub struct BuiltinOperator {
    pub name: String,
    pub kind: String,
}

pub struct AppState {
    pub api: Arc<Api>,
    pub security: Security,
    pub tokens: Arc<TokenManager>,
    pub runtime: Arc<dyn ContainerRuntime>,
    pub builtin_operators: Vec<BuiltinOperator>,
}

impl AppState {
    async fn principal<T>(&self, req: &Request<T>) -> Result<Principal, Status> {
        Ok(self.security.authenticate(req.metadata()).await?)
    }

    async fn require<T>(&self, req: &Request<T>, verb: &str, kind: &str, ns: &str) -> Result<Principal, Status> {
        let p = self.principal(req).await?;
        self.security.authorize(&p, verb, kind, ns).await?;
        Ok(p)
    }

    /// Callers may only hand out roles they hold themselves (admins may grant any).
    fn check_grantable(&self, p: &Principal, requested: &[String]) -> Result<(), Status> {
        if p.is_admin() {
            return Ok(());
        }
        match requested.iter().find(|r| !p.roles.contains(r)) {
            Some(r) => Err(ApiError::Forbidden(format!("cannot grant role '{r}' that you do not hold")).into()),
            None => Ok(()),
        }
    }

    async fn check_roles_exist(&self, names: &[String]) -> Result<(), Status> {
        for name in names {
            if self.security.store.get_role(name).await?.is_none() && builtin_role(name).is_none() {
                return Err(ApiError::Invalid(format!("role '{name}' does not exist")).into());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AuthService
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AuthSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl AuthService for AuthSvc {
    async fn login(&self, req: Request<pb::LoginRequest>) -> Result<Response<pb::TokenResponse>, Status> {
        let r = req.into_inner();
        let user = self.state.security.store.get_user(&r.username).await?;
        let valid = match &user {
            Some(u) => {
                let (pw, hash) = (r.password.clone(), u.password_hash.clone());
                tokio::task::spawn_blocking(move || verify_password(&pw, &hash)).await.unwrap_or(false)
            }
            None => false,
        };
        let user = match (valid, user) {
            (true, Some(u)) => u,
            _ => return Err(Status::unauthenticated("invalid username or password")),
        };
        let (token, claims) = self.state.tokens.issue(&user.username, user.roles.clone(), TokenKind::Session, None)?;
        tracing::info!(user = %user.username, "login");
        Ok(Response::new(pb::TokenResponse { token, expires_at: claims.exp, token_id: claims.jti }))
    }

    async fn who_am_i(&self, req: Request<pb::Empty>) -> Result<Response<pb::WhoAmIResponse>, Status> {
        let p = self.state.principal(&req).await?;
        Ok(Response::new(pb::WhoAmIResponse {
            subject: p.subject.clone(),
            roles: p.roles.clone(),
            token_kind: p.claims.kind.as_str().into(),
            expires_at: p.claims.exp,
        }))
    }

    async fn create_token(&self, req: Request<pb::CreateTokenRequest>) -> Result<Response<pb::TokenResponse>, Status> {
        let p = self.state.require(&req, verbs::CREATE, system_kinds::TOKENS, "").await?;
        let r = req.into_inner();
        let subject = if r.subject.is_empty() { p.subject.clone() } else { r.subject };
        let roles = if r.roles.is_empty() { p.roles.clone() } else { r.roles };
        self.state.check_grantable(&p, &roles)?;
        self.state.check_roles_exist(&roles).await?;
        let ttl = (r.ttl_seconds > 0).then_some(r.ttl_seconds);
        let (token, claims) = self.state.tokens.issue(&subject, roles, TokenKind::Api, ttl)?;
        Ok(Response::new(pb::TokenResponse { token, expires_at: claims.exp, token_id: claims.jti }))
    }

    async fn revoke_token(&self, req: Request<pb::RevokeTokenRequest>) -> Result<Response<pb::Empty>, Status> {
        let p = self.state.principal(&req).await?;
        let r = req.into_inner();
        let token_id = if r.token_id.is_empty() { p.claims.jti.clone() } else { r.token_id };
        let expires_at = if token_id == p.claims.jti {
            p.claims.exp // logging out: revoking your own token is always allowed
        } else {
            self.state.security.authorize(&p, verbs::DELETE, system_kinds::TOKENS, "").await?;
            chrono::Utc::now().timestamp() + OPERATOR_TOKEN_TTL
        };
        self.state.security.store.revoke_token(&token_id, expires_at).await?;
        Ok(Response::new(pb::Empty {}))
    }
}

// ---------------------------------------------------------------------------
// AdminService
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AdminSvc {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl AdminService for AdminSvc {
    async fn create_user(&self, req: Request<pb::CreateUserRequest>) -> Result<Response<pb::User>, Status> {
        let p = self.state.require(&req, verbs::CREATE, system_kinds::USERS, "").await?;
        let r = req.into_inner();
        validate_name(&r.username, "username")?;
        if r.password.len() < 8 {
            return Err(Status::invalid_argument("password must be at least 8 characters"));
        }
        self.state.check_grantable(&p, &r.roles)?;
        self.state.check_roles_exist(&r.roles).await?;
        let store = &self.state.security.store;
        if store.get_user(&r.username).await?.is_some() {
            return Err(Status::already_exists(format!("user '{}' already exists", r.username)));
        }
        let pw = r.password.clone();
        let hash = tokio::task::spawn_blocking(move || hash_password(&pw))
            .await
            .map_err(|e| Status::internal(e.to_string()))??;
        let user = UserRecord { username: r.username, password_hash: hash, roles: r.roles, created_at: now_rfc3339() };
        store.put_user(user.clone()).await?;
        Ok(Response::new(user_to_pb(&user)))
    }

    async fn list_users(&self, req: Request<pb::Empty>) -> Result<Response<pb::ListUsersResponse>, Status> {
        self.state.require(&req, verbs::LIST, system_kinds::USERS, "").await?;
        let users = self.state.security.store.list_users().await?;
        Ok(Response::new(pb::ListUsersResponse { users: users.iter().map(user_to_pb).collect() }))
    }

    async fn set_user_roles(&self, req: Request<pb::SetUserRolesRequest>) -> Result<Response<pb::User>, Status> {
        let p = self.state.require(&req, verbs::UPDATE, system_kinds::USERS, "").await?;
        let r = req.into_inner();
        self.state.check_grantable(&p, &r.roles)?;
        self.state.check_roles_exist(&r.roles).await?;
        let store = &self.state.security.store;
        let mut user = store
            .get_user(&r.username)
            .await?
            .ok_or_else(|| Status::not_found(format!("user '{}'", r.username)))?;
        user.roles = r.roles;
        store.put_user(user.clone()).await?;
        Ok(Response::new(user_to_pb(&user)))
    }

    async fn delete_user(&self, req: Request<pb::NameRequest>) -> Result<Response<pb::Empty>, Status> {
        let p = self.state.require(&req, verbs::DELETE, system_kinds::USERS, "").await?;
        let name = req.into_inner().name;
        if name == p.subject {
            return Err(Status::failed_precondition("you cannot delete yourself"));
        }
        if !self.state.security.store.delete_user(&name).await? {
            return Err(Status::not_found(format!("user '{name}'")));
        }
        Ok(Response::new(pb::Empty {}))
    }

    async fn put_role(&self, req: Request<pb::Role>) -> Result<Response<pb::Role>, Status> {
        self.state.require(&req, verbs::UPDATE, system_kinds::ROLES, "").await?;
        let role = role_from_pb(req.into_inner());
        if role.name.is_empty() || role.name.len() > 63 {
            return Err(Status::invalid_argument("role name must be 1-63 characters"));
        }
        if builtin_role(&role.name).is_some() {
            return Err(Status::failed_precondition(format!("built-in role '{}' cannot be modified", role.name)));
        }
        if role.rules.iter().any(|r| r.verbs.is_empty() || r.kinds.is_empty()) {
            return Err(Status::invalid_argument("every rule needs at least one verb and one kind"));
        }
        self.state.security.store.put_role(role.clone()).await?;
        Ok(Response::new(role_to_pb(&role)))
    }

    async fn list_roles(&self, req: Request<pb::Empty>) -> Result<Response<pb::ListRolesResponse>, Status> {
        self.state.require(&req, verbs::LIST, system_kinds::ROLES, "").await?;
        let roles = self.state.security.store.list_roles().await?;
        Ok(Response::new(pb::ListRolesResponse { roles: roles.iter().map(role_to_pb).collect() }))
    }

    async fn delete_role(&self, req: Request<pb::NameRequest>) -> Result<Response<pb::Empty>, Status> {
        self.state.require(&req, verbs::DELETE, system_kinds::ROLES, "").await?;
        let name = req.into_inner().name;
        if builtin_role(&name).is_some() {
            return Err(Status::failed_precondition(format!("built-in role '{name}' cannot be deleted")));
        }
        if !self.state.security.store.delete_role(&name).await? {
            return Err(Status::not_found(format!("role '{name}'")));
        }
        Ok(Response::new(pb::Empty {}))
    }
}

// ---------------------------------------------------------------------------
// ClusterService
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ClusterSvc {
    pub state: Arc<AppState>,
}

type WatchStream = Pin<Box<dyn Stream<Item = Result<pb::WatchEvent, Status>> + Send>>;

impl ClusterSvc {
    async fn key_of(&self, r: &pb::ResourceRef) -> Result<ResourceKey, Status> {
        let ns = self.state.api.effective_namespace(&r.kind, &r.namespace).await?;
        Ok(ResourceKey::new(&r.kind, ns, &r.name))
    }
}

#[tonic::async_trait]
impl ClusterService for ClusterSvc {
    async fn apply(&self, req: Request<pb::ApplyRequest>) -> Result<Response<pb::Resource>, Status> {
        let principal = self.state.principal(&req).await?;
        let pbr = req.into_inner().resource.ok_or_else(|| Status::invalid_argument("resource is required"))?;
        let mut r = resource_from_pb(pbr)?;
        r.metadata.namespace = self.state.api.effective_namespace(&r.kind, &r.metadata.namespace).await?;
        let exists = self.state.api.try_get(&r.key()).await?.is_some();
        let verb = if exists { verbs::UPDATE } else { verbs::CREATE };
        self.state.security.authorize(&principal, verb, &r.kind, &r.metadata.namespace).await?;
        let saved = self.state.api.apply(r).await?;
        tracing::info!(user = %principal.subject, verb, resource = %saved.key(), "apply");
        Ok(Response::new(resource_to_pb(&saved)))
    }

    async fn get(&self, req: Request<pb::ResourceRef>) -> Result<Response<pb::Resource>, Status> {
        let key = self.key_of(req.get_ref()).await?;
        self.state.require(&req, verbs::GET, &key.kind, &key.namespace).await?;
        Ok(Response::new(resource_to_pb(&self.state.api.get(&key).await?)))
    }

    async fn list(&self, req: Request<pb::ListRequest>) -> Result<Response<pb::ListResponse>, Status> {
        let r = req.get_ref().clone();
        let info = self.state.api.kind_info(&r.kind).await?;
        // Listing across all namespaces needs an all-namespace grant.
        let scope = match (info.namespaced, r.namespace.is_empty()) {
            (false, _) => String::new(),
            (true, true) => WILDCARD.to_string(),
            (true, false) => r.namespace.clone(),
        };
        self.state.require(&req, verbs::LIST, &r.kind, &scope).await?;
        let ns = (info.namespaced && !r.namespace.is_empty()).then_some(r.namespace.as_str());
        let selector: Labels = r.label_selector.into_iter().collect();
        let items = self.state.api.list(&r.kind, ns, &selector).await?;
        Ok(Response::new(pb::ListResponse { items: items.iter().map(resource_to_pb).collect() }))
    }

    async fn delete(&self, req: Request<pb::ResourceRef>) -> Result<Response<pb::Empty>, Status> {
        let key = self.key_of(req.get_ref()).await?;
        let p = self.state.require(&req, verbs::DELETE, &key.kind, &key.namespace).await?;
        self.state.api.delete(&key).await?;
        tracing::info!(user = %p.subject, resource = %key, "delete");
        Ok(Response::new(pb::Empty {}))
    }

    async fn scale(&self, req: Request<pb::ScaleRequest>) -> Result<Response<pb::Resource>, Status> {
        let ns = self.state.api.effective_namespace(kinds::DEPLOYMENT, &req.get_ref().namespace).await?;
        self.state.require(&req, verbs::SCALE, kinds::DEPLOYMENT, &ns).await?;
        let r = req.into_inner();
        Ok(Response::new(resource_to_pb(&self.state.api.scale(&ns, &r.name, r.replicas).await?)))
    }

    async fn update_status(&self, req: Request<pb::UpdateStatusRequest>) -> Result<Response<pb::Resource>, Status> {
        let target = req.get_ref().target.clone().ok_or_else(|| Status::invalid_argument("target is required"))?;
        let key = self.key_of(&target).await?;
        self.state.require(&req, verbs::STATUS, &key.kind, &key.namespace).await?;
        let status = parse_status(&req.get_ref().status_json)?;
        Ok(Response::new(resource_to_pb(&self.state.api.update_status(&key, status).await?)))
    }

    type WatchStream = WatchStream;

    async fn watch(&self, req: Request<pb::WatchRequest>) -> Result<Response<Self::WatchStream>, Status> {
        let principal = self.state.principal(&req).await?;
        let roles = self.state.security.load_roles(&principal).await?;
        let r = req.into_inner();
        let kinds: Vec<String> = if r.kinds.is_empty() {
            self.state.api.list_kinds().await?.into_iter().map(|k| k.kind).collect()
        } else {
            r.kinds
        };
        let ns_filter = r.namespace;
        let allowed = move |res: &Resource| -> bool {
            kinds.contains(&res.kind)
                && (ns_filter.is_empty() || res.metadata.namespace.is_empty() || res.metadata.namespace == ns_filter)
                && is_allowed(&roles, verbs::WATCH, &res.kind, &res.metadata.namespace)
        };

        // Subscribe before listing so no event between list and watch is lost.
        let mut events = self.state.api.subscribe();
        let api = self.state.api.clone();
        let (tx, rx) = mpsc::channel::<Result<pb::WatchEvent, Status>>(256);
        let send_initial = r.send_initial;
        tokio::spawn(async move {
            if send_initial {
                for kind in api.list_kinds().await.unwrap_or_default() {
                    for res in api.list(&kind.kind, None, &Labels::new()).await.unwrap_or_default() {
                        if allowed(&res) {
                            let ev = pb::WatchEvent {
                                r#type: pb::EventType::Added as i32,
                                resource: Some(resource_to_pb(&res)),
                            };
                            if tx.send(Ok(ev)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            loop {
                match events.recv().await {
                    Ok(ev) if allowed(&ev.resource) => {
                        if tx.send(Ok(event_to_pb(&ev))).await.is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => tracing::warn!(skipped = n, "watch stream lagged"),
                    Err(RecvError::Closed) => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn logs(&self, req: Request<pb::LogsRequest>) -> Result<Response<pb::LogsResponse>, Status> {
        let ns = self.state.api.effective_namespace(kinds::POD, &req.get_ref().namespace).await?;
        self.state.require(&req, verbs::LOGS, kinds::POD, &ns).await?;
        let r = req.into_inner();
        let pod = self.state.api.get(&ResourceKey::new(kinds::POD, &ns, &r.pod)).await?;
        let status: PodStatus = pod.status_as();
        let cs = if r.container.is_empty() {
            status.container_statuses.first()
        } else {
            status.container_statuses.iter().find(|c| c.name == r.container)
        }
        .ok_or_else(|| Status::not_found("container has no status yet"))?;
        if cs.container_id.is_empty() {
            return Err(Status::failed_precondition(format!("container '{}' is not running", cs.name)));
        }
        let tail = if r.tail > 0 { r.tail as usize } else { 200 };
        let text = self
            .state
            .runtime
            .logs(&cs.container_id, tail)
            .await
            .map_err(|e| Status::unavailable(format!("cannot read logs: {e}")))?;
        Ok(Response::new(pb::LogsResponse { text }))
    }

    async fn list_kinds(&self, req: Request<pb::Empty>) -> Result<Response<pb::ListKindsResponse>, Status> {
        self.state.principal(&req).await?;
        let kinds = self.state.api.list_kinds().await?;
        Ok(Response::new(pb::ListKindsResponse { kinds: kinds.iter().map(kind_to_pb).collect() }))
    }
}

// ---------------------------------------------------------------------------
// OperatorService
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OperatorSvc {
    pub state: Arc<AppState>,
}

pub fn operator_role_name(name: &str) -> String {
    format!("operator:{name}")
}

#[tonic::async_trait]
impl OperatorService for OperatorSvc {
    async fn register(
        &self,
        req: Request<pb::RegisterOperatorRequest>,
    ) -> Result<Response<pb::RegisterOperatorResponse>, Status> {
        let caller = self.state.require(&req, verbs::CREATE, system_kinds::OPERATORS, "").await?;
        let r = req.into_inner();
        validate_name(&r.name, "operator name")?;

        // Extra rules must be a subset of what the caller may do (no privilege escalation).
        let extra: Vec<PolicyRule> = r.extra_rules.into_iter().map(rule_from_pb).collect();
        for rule in &extra {
            let namespaces = if rule.namespaces.is_empty() { vec![WILDCARD.to_string()] } else { rule.namespaces.clone() };
            for verb in &rule.verbs {
                for kind in &rule.kinds {
                    for ns in &namespaces {
                        self.state.security.authorize(&caller, verb, kind, ns).await?;
                    }
                }
            }
        }

        // Register CRDs.
        let mut kinds = vec![];
        for crd in r.crds {
            validate_kind_name(&crd.kind)?;
            let schema = parse_status(&crd.schema_json)?;
            let spec = json!({
                "kind": crd.kind,
                "namespaced": crd.namespaced,
                "apiVersion": crd.api_version,
                "schema": schema,
                "operator": r.name,
            });
            let res = Resource::new(kinds::CRD, "", &crd.kind.to_lowercase(), spec);
            self.state.api.apply(res).await?;
            kinds.push(crd.kind);
        }

        // Generated role: full control of its own kinds + approved extra rules.
        let role_name = operator_role_name(&r.name);
        let mut rules = vec![];
        if !kinds.is_empty() {
            rules.push(PolicyRule {
                verbs: verbs::ALL.iter().map(|v| v.to_string()).collect(),
                kinds: kinds.clone(),
                namespaces: vec![],
            });
        }
        rules.extend(extra);
        self.state.security.store.put_role(Role { name: role_name.clone(), rules, builtin: false }).await?;

        // Replace any previous registration's token.
        if let Some(prev) = self.state.security.store.get_operator(&r.name).await? {
            self.state.security.store.revoke_token(&prev.token_id, prev.token_expires_at).await?;
        }
        let ttl = if r.token_ttl_seconds > 0 { r.token_ttl_seconds } else { OPERATOR_TOKEN_TTL };
        let (token, claims) = self.state.tokens.issue(
            &format!("operator:{}", r.name),
            vec![roles::OPERATOR.into(), role_name.clone()],
            TokenKind::Operator,
            Some(ttl),
        )?;
        let record = OperatorRecord {
            name: r.name.clone(),
            kinds,
            role: role_name,
            token_id: claims.jti,
            token_expires_at: claims.exp,
            registered_at: now_rfc3339(),
        };
        self.state.security.store.put_operator(record.clone()).await?;
        tracing::info!(operator = %r.name, by = %caller.subject, "operator registered");
        Ok(Response::new(pb::RegisterOperatorResponse { operator: Some(operator_to_pb(&record)), token }))
    }

    async fn list(&self, req: Request<pb::Empty>) -> Result<Response<pb::ListOperatorsResponse>, Status> {
        self.state.require(&req, verbs::LIST, system_kinds::OPERATORS, "").await?;
        let mut ops: Vec<pb::OperatorInfo> = self
            .state
            .builtin_operators
            .iter()
            .map(|o| pb::OperatorInfo {
                name: o.name.clone(),
                kinds: vec![o.kind.clone()],
                registered_at: String::new(),
                role: "system".into(),
                builtin: true,
            })
            .collect();
        ops.extend(self.state.security.store.list_operators().await?.iter().map(operator_to_pb));
        Ok(Response::new(pb::ListOperatorsResponse { operators: ops }))
    }

    async fn unregister(&self, req: Request<pb::NameRequest>) -> Result<Response<pb::Empty>, Status> {
        self.state.require(&req, verbs::DELETE, system_kinds::OPERATORS, "").await?;
        let name = req.into_inner().name;
        let store = &self.state.security.store;
        let rec = store.get_operator(&name).await?.ok_or_else(|| Status::not_found(format!("operator '{name}'")))?;
        store.revoke_token(&rec.token_id, rec.token_expires_at).await?;
        store.delete_role(&rec.role).await?;
        store.delete_operator(&name).await?;
        // CRDs and their objects are intentionally kept; delete the CRDs explicitly to remove data.
        Ok(Response::new(pb::Empty {}))
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Serve native gRPC (HTTP/2) and gRPC-Web (HTTP/1.1, for the browser UI) on one port.
pub async fn serve(
    state: Arc<AppState>,
    addr: SocketAddr,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers(Any);
    tracing::info!(%addr, "lkube API listening (gRPC + gRPC-Web)");
    tonic::transport::Server::builder()
        .accept_http1(true)
        .layer(cors)
        .layer(tonic_web::GrpcWebLayer::new())
        .add_service(AuthServiceServer::new(AuthSvc { state: state.clone() }))
        .add_service(AdminServiceServer::new(AdminSvc { state: state.clone() }))
        .add_service(ClusterServiceServer::new(ClusterSvc { state: state.clone() }))
        .add_service(OperatorServiceServer::new(OperatorSvc { state }))
        .serve_with_shutdown(addr, shutdown)
        .await?;
    Ok(())
}

/// Labels helper for tests/clients.
pub fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}
