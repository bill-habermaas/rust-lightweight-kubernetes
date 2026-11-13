use lkube::api::Api;
use lkube::auth::TokenManager;
use lkube::config::{Config, RuntimeBackend, StoreBackend};
use lkube::controllers::{self, Kubelet};
use lkube::operator::OperatorManager;
use lkube::runtime::{ContainerRuntime, DockerRuntime, MockRuntime};
use lkube::security::Security;
use lkube::server::{self, AppState, BuiltinOperator};
use lkube::store::{MemoryStore, MongoStore, Store};
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

async fn connect_mongo(cfg: &Config) -> anyhow::Result<MongoStore> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match MongoStore::connect(&cfg.mongo_uri, &cfg.mongo_db).await {
            Ok(s) => return Ok(s),
            Err(e) if attempt < 30 => {
                tracing::warn!(attempt, error = %e, "waiting for MongoDB");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cfg = Config::from_env()?;
    tracing::info!(store = ?cfg.store, runtime = ?cfg.runtime, node = %cfg.node_name, "starting lkube");

    let store: Arc<dyn Store> = match cfg.store {
        StoreBackend::Mongo => Arc::new(connect_mongo(&cfg).await?),
        StoreBackend::Memory => Arc::new(MemoryStore::new()),
    };
    let runtime: Arc<dyn ContainerRuntime> = match cfg.runtime {
        RuntimeBackend::Docker => Arc::new(DockerRuntime::connect(cfg.docker_network.clone())?),
        RuntimeBackend::Mock => Arc::new(MockRuntime::new()),
    };

    let api = Api::new(store.clone());
    lkube::bootstrap::run(&api, &cfg.admin_user, &cfg.admin_password).await?;
    controllers::heartbeat_once(&api, &cfg.node_name, &cfg.node_address, cfg.node_capacity).await?;

    // Operators: control plane + this node's kubelet.
    let kubelet = Arc::new(Kubelet::new(&cfg.node_name, runtime.clone()));
    let mut manager = OperatorManager::new(api.clone());
    if cfg.run_controllers {
        for op in controllers::control_plane() {
            manager.register(op);
        }
    }
    manager.register(kubelet.clone());
    let builtin_operators = manager
        .names()
        .into_iter()
        .map(|(name, kind)| BuiltinOperator { name, kind })
        .collect();
    manager.start();
    controllers::spawn_node_agent(
        api.clone(),
        kubelet,
        cfg.node_name.clone(),
        cfg.node_address.clone(),
        cfg.node_capacity,
    );

    let tokens = Arc::new(TokenManager::new(cfg.jwt_secret.as_bytes(), "lkube", cfg.token_ttl_secs));
    let state = Arc::new(AppState {
        api,
        security: Security::new(store, tokens.clone()),
        tokens,
        runtime,
        builtin_operators,
    });
    server::serve(state, cfg.listen, shutdown_signal()).await
}
