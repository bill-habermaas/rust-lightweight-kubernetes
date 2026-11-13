use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreBackend {
    Mongo,
    Memory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeBackend {
    Docker,
    Mock,
}

/// Server configuration, read from `LKUBE_*` environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub store: StoreBackend,
    pub mongo_uri: String,
    pub mongo_db: String,
    pub jwt_secret: String,
    pub token_ttl_secs: i64,
    pub admin_user: String,
    pub admin_password: String,
    pub node_name: String,
    pub node_address: String,
    pub node_capacity: u32,
    pub runtime: RuntimeBackend,
    pub docker_network: Option<String>,
    pub run_controllers: bool,
}

fn env(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let jwt_secret = env("LKUBE_JWT_SECRET", "");
        let jwt_secret = if jwt_secret.is_empty() {
            tracing::warn!("LKUBE_JWT_SECRET is not set; using an insecure development secret");
            "insecure-dev-secret-change-me".to_string()
        } else {
            jwt_secret
        };
        let hostname = env("HOSTNAME", "node-1").to_lowercase();
        Ok(Self {
            listen: env("LKUBE_LISTEN", "0.0.0.0:50051").parse()?,
            store: match env("LKUBE_STORE", "mongo").as_str() {
                "memory" => StoreBackend::Memory,
                _ => StoreBackend::Mongo,
            },
            mongo_uri: env("LKUBE_MONGO_URI", "mongodb://localhost:27017"),
            mongo_db: env("LKUBE_MONGO_DB", "lkube"),
            jwt_secret,
            token_ttl_secs: env("LKUBE_TOKEN_TTL_SECS", "3600").parse()?,
            admin_user: env("LKUBE_ADMIN_USER", "admin"),
            admin_password: env("LKUBE_ADMIN_PASSWORD", "admin12345"),
            node_name: env("LKUBE_NODE_NAME", &hostname),
            node_address: env("LKUBE_NODE_ADDRESS", &hostname),
            node_capacity: env("LKUBE_NODE_CAPACITY", "110").parse()?,
            runtime: match env("LKUBE_RUNTIME", "docker").as_str() {
                "mock" => RuntimeBackend::Mock,
                _ => RuntimeBackend::Docker,
            },
            docker_network: std::env::var("LKUBE_DOCKER_NETWORK").ok().filter(|v| !v.is_empty()),
            run_controllers: env("LKUBE_CONTROLLERS", "true") != "false",
        })
    }
}
