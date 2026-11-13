#![allow(dead_code)]

use lkube::api::Api;
use lkube::auth::{hash_password, TokenManager};
use lkube::bootstrap;
use lkube::model::now_rfc3339;
use lkube::pb;
use lkube::runtime::MockRuntime;
use lkube::security::Security;
use lkube::server::{AdminSvc, AppState, AuthSvc, BuiltinOperator, ClusterSvc, OperatorSvc};
use lkube::store::{MemoryStore, Store, UserRecord};
use std::sync::Arc;
use tonic::Request;

pub const ADMIN_PW: &str = "admin-password";

pub struct Harness {
    pub state: Arc<AppState>,
    pub store: Arc<dyn Store>,
    pub runtime: Arc<MockRuntime>,
    pub auth: AuthSvc,
    pub admin: AdminSvc,
    pub cluster: ClusterSvc,
    pub operators: OperatorSvc,
}

impl Harness {
    pub async fn new() -> Self {
        let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
        let api = Api::new(store.clone());
        bootstrap::run(&api, "admin", ADMIN_PW).await.unwrap();
        let tokens = Arc::new(TokenManager::new(b"test-secret", "lkube", 3600));
        let runtime = Arc::new(MockRuntime::new());
        let state = Arc::new(AppState {
            api,
            security: Security::new(store.clone(), tokens.clone()),
            tokens,
            runtime: runtime.clone(),
            builtin_operators: vec![BuiltinOperator { name: "scheduler".into(), kind: "Pod".into() }],
        });
        Self {
            auth: AuthSvc { state: state.clone() },
            admin: AdminSvc { state: state.clone() },
            cluster: ClusterSvc { state: state.clone() },
            operators: OperatorSvc { state: state.clone() },
            state,
            store,
            runtime,
        }
    }

    pub async fn add_user(&self, name: &str, roles: &[&str]) {
        self.store
            .put_user(UserRecord {
                username: name.into(),
                password_hash: hash_password("password123").unwrap(),
                roles: roles.iter().map(|r| r.to_string()).collect(),
                created_at: now_rfc3339(),
            })
            .await
            .unwrap();
    }

    pub async fn login(&self, user: &str, pw: &str) -> String {
        use lkube::pb::auth_service_server::AuthService;
        self.auth
            .login(Request::new(pb::LoginRequest { username: user.into(), password: pw.into() }))
            .await
            .unwrap()
            .into_inner()
            .token
    }

    pub async fn admin_token(&self) -> String {
        self.login("admin", ADMIN_PW).await
    }
}

pub fn req<T>(msg: T, token: &str) -> Request<T> {
    let mut r = Request::new(msg);
    r.metadata_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    r
}

pub fn resource(kind: &str, ns: &str, name: &str, spec: serde_json::Value) -> pb::Resource {
    lkube::sdk::LkubeClient::resource(kind, ns, name, spec)
}

pub fn deployment(name: &str, replicas: i32) -> serde_json::Value {
    serde_json::json!({
        "replicas": replicas,
        "template": {
            "labels": {"app": name},
            "spec": {"containers": [{"name": "app", "image": "nginx:1.27"}]}
        }
    })
}
