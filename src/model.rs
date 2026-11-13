//! Resource model: a generic object envelope plus typed specs for built-in kinds.

use crate::error::{ApiError, Result};
use chrono::{SecondsFormat, Utc};
use rand::{distributions::Alphanumeric, Rng};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;

pub const API_VERSION: &str = "lkube.io/v1";

pub mod kinds {
    pub const NAMESPACE: &str = "Namespace";
    pub const NODE: &str = "Node";
    pub const POD: &str = "Pod";
    pub const DEPLOYMENT: &str = "Deployment";
    pub const SERVICE: &str = "Service";
    pub const CONFIG_MAP: &str = "ConfigMap";
    pub const CRD: &str = "CustomResourceDefinition";
}

pub const DEFAULT_NAMESPACE: &str = "default";
pub const LABEL_DEPLOYMENT: &str = "lkube.io/deployment";
pub const ANNOTATION_TEMPLATE_HASH: &str = "lkube.io/template-hash";

pub type Labels = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ResourceKey {
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

impl ResourceKey {
    pub fn new(kind: impl Into<String>, namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self { kind: kind.into(), namespace: namespace.into(), name: name.into() }
    }
    /// Stable storage id.
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.kind, self.namespace, self.name)
    }
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.namespace.is_empty() {
            write!(f, "{}/{}", self.kind, self.name)
        } else {
            write!(f, "{}/{}/{}", self.kind, self.namespace, self.name)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub name: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub labels: Labels,
    #[serde(default)]
    pub annotations: Labels,
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub resource_version: i64,
    #[serde(default)]
    pub generation: i64,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub owner_uid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub api_version: String,
    pub kind: String,
    pub metadata: ObjectMeta,
    #[serde(default)]
    pub spec: Value,
    #[serde(default)]
    pub status: Value,
}

impl Resource {
    pub fn new(kind: &str, namespace: &str, name: &str, spec: Value) -> Self {
        Self {
            api_version: API_VERSION.into(),
            kind: kind.into(),
            metadata: ObjectMeta { name: name.into(), namespace: namespace.into(), ..Default::default() },
            spec,
            status: Value::Null,
        }
    }

    pub fn with_labels(mut self, labels: &[(&str, &str)]) -> Self {
        for (k, v) in labels {
            self.metadata.labels.insert((*k).into(), (*v).into());
        }
        self
    }

    pub fn key(&self) -> ResourceKey {
        ResourceKey::new(&self.kind, &self.metadata.namespace, &self.metadata.name)
    }

    pub fn spec_as<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.spec.clone())
            .map_err(|e| ApiError::Invalid(format!("{}: invalid spec: {e}", self.key())))
    }

    /// Typed status; an absent or unparsable status yields the default.
    pub fn status_as<T: DeserializeOwned + Default>(&self) -> T {
        if self.status.is_null() {
            return T::default();
        }
        serde_json::from_value(self.status.clone()).unwrap_or_default()
    }

    pub fn matches_selector(&self, selector: &Labels) -> bool {
        selector.iter().all(|(k, v)| self.metadata.labels.get(k) == Some(v))
    }
}

// ---------------------------------------------------------------------------
// Built-in kind specs (JSON uses camelCase, like Kubernetes manifests)
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}
fn default_one() -> i32 {
    1
}
fn default_capacity() -> u32 {
    110
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerPort {
    pub container_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub name: String,
    pub image: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<ContainerPort>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestartPolicy {
    #[default]
    Always,
    OnFailure,
    Never,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSpec {
    pub containers: Vec<Container>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_selector: Labels,
    #[serde(default)]
    pub restart_policy: RestartPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PodPhase {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl PodPhase {
    pub fn is_terminal(self) -> bool {
        matches!(self, PodPhase::Succeeded | PodPhase::Failed)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatus {
    pub name: String,
    #[serde(default)]
    pub container_id: String,
    /// waiting | running | exited
    #[serde(default)]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub restart_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodStatus {
    #[serde(default)]
    pub phase: PodPhase,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_ip: Option<String>,
    #[serde(default)]
    pub container_statuses: Vec<ContainerStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodTemplate {
    #[serde(default)]
    pub labels: Labels,
    pub spec: PodSpec,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentSpec {
    #[serde(default = "default_one")]
    pub replicas: i32,
    #[serde(default)]
    pub selector: Labels,
    pub template: PodTemplate,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentStatus {
    pub replicas: i32,
    pub ready_replicas: i32,
    pub updated_replicas: i32,
    pub observed_generation: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServicePort {
    #[serde(default)]
    pub name: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    #[serde(default)]
    pub selector: Labels,
    #[serde(default)]
    pub ports: Vec<ServicePort>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub pod: String,
    pub node: String,
    pub ip: String,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub endpoints: Vec<Endpoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSpec {
    #[serde(default)]
    pub address: String,
    #[serde(default = "default_capacity")]
    pub capacity_pods: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub ready: bool,
    pub last_heartbeat: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMapSpec {
    #[serde(default)]
    pub data: BTreeMap<String, String>,
}

/// Spec of a CustomResourceDefinition. The CRD object's name must be `kind` lower-cased.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrdSpec {
    pub kind: String,
    #[serde(default = "default_true")]
    pub namespaced: bool,
    #[serde(default)]
    pub api_version: String,
    /// Minimal JSON-schema subset: `required` and `properties.<name>.type`.
    #[serde(default)]
    pub schema: Value,
    /// Name of the operator that registered it (informational).
    #[serde(default)]
    pub operator: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KindInfo {
    pub kind: String,
    pub namespaced: bool,
    pub builtin: bool,
    pub api_version: String,
    pub schema: Value,
}

pub fn builtin_kinds() -> Vec<KindInfo> {
    let mk = |kind: &str, namespaced: bool| KindInfo {
        kind: kind.into(),
        namespaced,
        builtin: true,
        api_version: API_VERSION.into(),
        schema: Value::Null,
    };
    vec![
        mk(kinds::NAMESPACE, false),
        mk(kinds::NODE, false),
        mk(kinds::POD, true),
        mk(kinds::DEPLOYMENT, true),
        mk(kinds::SERVICE, true),
        mk(kinds::CONFIG_MAP, true),
        mk(kinds::CRD, false),
    ]
}

pub fn is_builtin_kind(kind: &str) -> bool {
    builtin_kinds().iter().any(|k| k.kind == kind)
}

/// DNS-1123-ish names: lowercase alphanumerics, '-' and '.', max 63 chars.
pub fn validate_name(name: &str, what: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && !name.starts_with(['-', '.'])
        && !name.ends_with(['-', '.']);
    if ok {
        Ok(())
    } else {
        Err(ApiError::Invalid(format!(
            "{what} '{name}' must be 1-63 lowercase alphanumeric characters, '-' or '.'"
        )))
    }
}

/// Custom kind names: PascalCase alphanumerics.
pub fn validate_kind_name(kind: &str) -> Result<()> {
    let ok = !kind.is_empty()
        && kind.len() <= 63
        && kind.starts_with(|c: char| c.is_ascii_uppercase())
        && kind.chars().all(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(ApiError::Invalid(format!("kind '{kind}' must be PascalCase alphanumeric")))
    }
}

/// Validate a custom resource spec against the minimal schema subset.
pub fn validate_against_schema(spec: &Value, schema: &Value) -> Result<()> {
    if schema.is_null() {
        return Ok(());
    }
    let obj = spec
        .as_object()
        .ok_or_else(|| ApiError::Invalid("spec must be a JSON object".into()))?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if !obj.contains_key(field) {
                return Err(ApiError::Invalid(format!("spec.{field} is required")));
            }
        }
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (field, def) in props {
            let (Some(v), Some(t)) = (obj.get(field), def.get("type").and_then(Value::as_str)) else {
                continue;
            };
            let ok = match t {
                "string" => v.is_string(),
                "integer" => v.is_i64() || v.is_u64(),
                "number" => v.is_number(),
                "boolean" => v.is_boolean(),
                "object" => v.is_object(),
                "array" => v.is_array(),
                _ => true,
            };
            if !ok {
                return Err(ApiError::Invalid(format!("spec.{field} must be of type {t}")));
            }
        }
    }
    Ok(())
}

/// Stable 64-bit FNV-1a hash rendered as hex (used for pod-template hashes).
pub fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn random_suffix(n: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(n)
        .map(|c| (c as char).to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_validated() {
        assert!(validate_name("web-1", "name").is_ok());
        assert!(validate_name("a.b", "name").is_ok());
        assert!(validate_name("", "name").is_err());
        assert!(validate_name("Web", "name").is_err());
        assert!(validate_name("-web", "name").is_err());
        assert!(validate_name(&"a".repeat(64), "name").is_err());
    }

    #[test]
    fn kind_names_are_validated() {
        assert!(validate_kind_name("WebApp").is_ok());
        assert!(validate_kind_name("webApp").is_err());
        assert!(validate_kind_name("Web-App").is_err());
    }

    #[test]
    fn selector_matching() {
        let r = Resource::new(kinds::POD, "default", "p", json!({})).with_labels(&[("app", "web"), ("tier", "fe")]);
        let mut sel = Labels::new();
        assert!(r.matches_selector(&sel));
        sel.insert("app".into(), "web".into());
        assert!(r.matches_selector(&sel));
        sel.insert("tier".into(), "be".into());
        assert!(!r.matches_selector(&sel));
    }

    #[test]
    fn schema_validation() {
        let schema = json!({"required": ["image"], "properties": {"image": {"type": "string"}, "replicas": {"type": "integer"}}});
        assert!(validate_against_schema(&json!({"image": "nginx", "replicas": 2}), &schema).is_ok());
        assert!(validate_against_schema(&json!({"replicas": 2}), &schema).is_err());
        assert!(validate_against_schema(&json!({"image": 5}), &schema).is_err());
        assert!(validate_against_schema(&json!({"image": "x", "replicas": "two"}), &schema).is_err());
        assert!(validate_against_schema(&json!("not-an-object"), &schema).is_err());
        assert!(validate_against_schema(&json!({}), &Value::Null).is_ok());
    }

    #[test]
    fn pod_spec_defaults_and_camel_case() {
        let spec: PodSpec = serde_json::from_value(json!({
            "containers": [{"name": "c", "image": "nginx", "ports": [{"containerPort": 80}]}],
            "nodeName": "n1"
        }))
        .unwrap();
        assert_eq!(spec.restart_policy, RestartPolicy::Always);
        assert_eq!(spec.node_name.as_deref(), Some("n1"));
        assert_eq!(spec.containers[0].ports[0].container_port, 80);
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(fnv1a_hex("abc"), fnv1a_hex("abc"));
        assert_ne!(fnv1a_hex("abc"), fnv1a_hex("abd"));
        assert_eq!(fnv1a_hex(""), "cbf29ce484222325");
    }

    #[test]
    fn status_defaults_when_missing() {
        let r = Resource::new(kinds::POD, "default", "p", json!({}));
        let st: PodStatus = r.status_as();
        assert_eq!(st.phase, PodPhase::Pending);
    }
}
