//! Container runtimes used by the kubelet.

pub mod docker;
pub mod mock;

pub use docker::DockerRuntime;
pub use mock::MockRuntime;

use crate::model::ContainerPort;
use async_trait::async_trait;
use std::collections::BTreeMap;

/// Label put on every managed container: `<namespace>.<pod>`.
pub const LABEL_POD: &str = "lkube.pod";
pub const LABEL_CONTAINER: &str = "lkube.container";
pub const LABEL_MANAGED: &str = "lkube.managed";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunSpec {
    pub name: String,
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub labels: BTreeMap<String, String>,
    pub ports: Vec<ContainerPort>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContainerState {
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i64>,
    pub ip: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ManagedContainer {
    pub id: String,
    pub pod: String,
}

#[async_trait]
pub trait ContainerRuntime: Send + Sync + 'static {
    /// Pull (if needed), create and start a container. Returns its id.
    async fn run(&self, spec: &RunSpec) -> anyhow::Result<String>;
    /// `None` if the container no longer exists.
    async fn inspect(&self, id: &str) -> anyhow::Result<Option<ContainerState>>;
    async fn remove(&self, id: &str) -> anyhow::Result<()>;
    /// All containers created by lkube on this host.
    async fn list_managed(&self) -> anyhow::Result<Vec<ManagedContainer>>;
    async fn logs(&self, id: &str, tail: usize) -> anyhow::Result<String>;
}

pub fn pod_label(namespace: &str, pod: &str) -> String {
    format!("{namespace}.{pod}")
}
