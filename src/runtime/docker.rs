use super::{ContainerRuntime, ContainerState, ManagedContainer, RunSpec, LABEL_MANAGED, LABEL_POD};
use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, PortBinding};
use bollard::Docker;
use futures::{StreamExt, TryStreamExt};
use std::collections::HashMap;

/// Runs pods as Docker containers through the Docker Engine API (unix socket).
pub struct DockerRuntime {
    docker: Docker,
    network: Option<String>,
}

impl DockerRuntime {
    pub fn connect(network: Option<String>) -> anyhow::Result<Self> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(Self { docker, network })
    }

    async fn ensure_image(&self, image: &str) -> anyhow::Result<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        let (from_image, tag) = split_image(image);
        tracing::info!(image, "pulling image");
        self.docker
            .create_image(
                Some(CreateImageOptions { from_image, tag, ..Default::default() }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await?;
        Ok(())
    }
}

/// Split `repo[:tag]` so that pulling never fetches every tag of a repository.
pub fn split_image(image: &str) -> (&str, &str) {
    if image.contains('@') {
        return (image, "");
    }
    let last_slash = image.rfind('/').map(|i| i + 1).unwrap_or(0);
    match image[last_slash..].rfind(':') {
        Some(i) => (&image[..last_slash + i], &image[last_slash + i + 1..]),
        None => (image, "latest"),
    }
}

fn is_not_found(e: &bollard::errors::Error) -> bool {
    matches!(e, bollard::errors::Error::DockerResponseServerError { status_code: 404, .. })
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn run(&self, spec: &RunSpec) -> anyhow::Result<String> {
        self.ensure_image(&spec.image).await?;

        let mut labels: HashMap<String, String> = spec.labels.clone().into_iter().collect();
        labels.insert(LABEL_MANAGED.into(), "true".into());

        let mut exposed = HashMap::new();
        let mut bindings = HashMap::new();
        for p in &spec.ports {
            let key = format!("{}/tcp", p.container_port);
            exposed.insert(key.clone(), HashMap::new());
            if let Some(host) = p.host_port {
                bindings.insert(
                    key,
                    Some(vec![PortBinding { host_ip: None, host_port: Some(host.to_string()) }]),
                );
            }
        }

        let config = Config {
            image: Some(spec.image.clone()),
            entrypoint: (!spec.command.is_empty()).then(|| spec.command.clone()),
            cmd: (!spec.args.is_empty()).then(|| spec.args.clone()),
            env: Some(spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            labels: Some(labels),
            exposed_ports: Some(exposed),
            host_config: Some(HostConfig {
                port_bindings: Some(bindings),
                network_mode: self.network.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let created = self
            .docker
            .create_container(Some(CreateContainerOptions { name: spec.name.clone(), platform: None }), config)
            .await?;
        self.docker.start_container(&created.id, None::<StartContainerOptions<String>>).await?;
        Ok(created.id)
    }

    async fn inspect(&self, id: &str) -> anyhow::Result<Option<ContainerState>> {
        let info = match self.docker.inspect_container(id, None).await {
            Ok(i) => i,
            Err(e) if is_not_found(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let state = info.state.unwrap_or_default();
        let running = state.running.unwrap_or(false);
        let ip = info.network_settings.and_then(|ns| {
            ns.networks
                .and_then(|nets| nets.values().filter_map(|n| n.ip_address.clone()).find(|ip| !ip.is_empty()))
                .or(ns.ip_address.filter(|ip| !ip.is_empty()))
        });
        Ok(Some(ContainerState {
            id: id.to_string(),
            running,
            exit_code: if running { None } else { state.exit_code },
            ip: if running { ip } else { None },
            error: state.error.filter(|e| !e.is_empty()),
        }))
    }

    async fn remove(&self, id: &str) -> anyhow::Result<()> {
        match self
            .docker
            .remove_container(id, Some(RemoveContainerOptions { force: true, ..Default::default() }))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn list_managed(&self) -> anyhow::Result<Vec<ManagedContainer>> {
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![format!("{LABEL_MANAGED}=true")]);
        let list = self
            .docker
            .list_containers(Some(ListContainersOptions { all: true, filters, ..Default::default() }))
            .await?;
        Ok(list
            .into_iter()
            .filter_map(|c| {
                let pod = c.labels.as_ref().and_then(|l| l.get(LABEL_POD).cloned()).unwrap_or_default();
                c.id.map(|id| ManagedContainer { id, pod })
            })
            .collect())
    }

    async fn logs(&self, id: &str, tail: usize) -> anyhow::Result<String> {
        let mut stream = self.docker.logs(
            id,
            Some(LogsOptions::<String> {
                stdout: true,
                stderr: true,
                tail: tail.to_string(),
                ..Default::default()
            }),
        );
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            out.push_str(&chunk?.to_string());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::split_image;

    #[test]
    fn image_references_are_split() {
        assert_eq!(split_image("nginx"), ("nginx", "latest"));
        assert_eq!(split_image("nginx:1.27"), ("nginx", "1.27"));
        assert_eq!(split_image("localhost:5000/app"), ("localhost:5000/app", "latest"));
        assert_eq!(split_image("localhost:5000/app:v2"), ("localhost:5000/app", "v2"));
        assert_eq!(split_image("repo/app@sha256:abc"), ("repo/app@sha256:abc", ""));
    }
}
