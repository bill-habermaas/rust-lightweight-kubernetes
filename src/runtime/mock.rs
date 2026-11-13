use super::{ContainerRuntime, ContainerState, ManagedContainer, RunSpec, LABEL_POD};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// In-memory runtime: containers "run" instantly. Used by tests and `LKUBE_RUNTIME=mock`.
#[derive(Default)]
pub struct MockRuntime {
    containers: Mutex<HashMap<String, (RunSpec, ContainerState)>>,
    counter: AtomicUsize,
    failing_images: Mutex<Vec<String>>,
}

impl MockRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulate a container exiting.
    pub fn exit(&self, id: &str, code: i64) {
        if let Some((_, st)) = self.containers.lock().unwrap().get_mut(id) {
            st.running = false;
            st.exit_code = Some(code);
            st.ip = None;
        }
    }

    /// Make `run` fail for an image (simulates pull errors).
    pub fn fail_image(&self, image: &str) {
        self.failing_images.lock().unwrap().push(image.into());
    }

    pub fn running_count(&self) -> usize {
        self.containers.lock().unwrap().values().filter(|(_, s)| s.running).count()
    }

    pub fn count(&self) -> usize {
        self.containers.lock().unwrap().len()
    }

    pub fn spec_of(&self, id: &str) -> Option<RunSpec> {
        self.containers.lock().unwrap().get(id).map(|(s, _)| s.clone())
    }
}

#[async_trait]
impl ContainerRuntime for MockRuntime {
    async fn run(&self, spec: &RunSpec) -> anyhow::Result<String> {
        if self.failing_images.lock().unwrap().contains(&spec.image) {
            anyhow::bail!("failed to pull image {}", spec.image);
        }
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("mock-{n:06}");
        let state = ContainerState {
            id: id.clone(),
            running: true,
            exit_code: None,
            ip: Some(format!("10.0.{}.{}", (n / 250) % 250, n % 250 + 2)),
            error: None,
        };
        self.containers.lock().unwrap().insert(id.clone(), (spec.clone(), state));
        Ok(id)
    }

    async fn inspect(&self, id: &str) -> anyhow::Result<Option<ContainerState>> {
        Ok(self.containers.lock().unwrap().get(id).map(|(_, s)| s.clone()))
    }

    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        self.containers.lock().unwrap().remove(id);
        Ok(())
    }

    async fn list_managed(&self) -> anyhow::Result<Vec<ManagedContainer>> {
        Ok(self
            .containers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, (spec, _))| ManagedContainer {
                id: id.clone(),
                pod: spec.labels.get(LABEL_POD).cloned().unwrap_or_default(),
            })
            .collect())
    }

    async fn logs(&self, id: &str, _tail: usize) -> anyhow::Result<String> {
        let guard = self.containers.lock().unwrap();
        let (spec, state) = guard.get(id).ok_or_else(|| anyhow::anyhow!("no such container {id}"))?;
        Ok(format!(
            "[mock] container {} ({}) image={} running={}\n",
            spec.name, id, spec.image, state.running
        ))
    }
}
