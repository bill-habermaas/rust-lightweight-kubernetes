//! Conversions between the internal model and the protobuf messages.

use crate::api::{EventType, WatchEvent};
use crate::authz::{PolicyRule, Role};
use crate::error::{ApiError, Result};
use crate::model::{KindInfo, ObjectMeta, Resource};
use crate::pb;
use crate::store::{OperatorRecord, UserRecord};
use serde_json::Value;

fn json_or_null(s: &str) -> Result<Value> {
    if s.trim().is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_str(s).map_err(|e| ApiError::Invalid(format!("invalid JSON: {e}")))
    }
}

fn json_string(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        v.to_string()
    }
}

pub fn resource_to_pb(r: &Resource) -> pb::Resource {
    let m = &r.metadata;
    pb::Resource {
        api_version: r.api_version.clone(),
        kind: r.kind.clone(),
        metadata: Some(pb::ObjectMeta {
            name: m.name.clone(),
            namespace: m.namespace.clone(),
            labels: m.labels.clone().into_iter().collect(),
            annotations: m.annotations.clone().into_iter().collect(),
            uid: m.uid.clone(),
            resource_version: m.resource_version,
            generation: m.generation,
            created_at: m.created_at.clone(),
            owner_uid: m.owner_uid.clone(),
        }),
        spec_json: json_string(&r.spec),
        status_json: json_string(&r.status),
    }
}

/// Client supplied objects: server-managed metadata (uid, versions, timestamps) is ignored.
pub fn resource_from_pb(p: pb::Resource) -> Result<Resource> {
    let m = p.metadata.ok_or_else(|| ApiError::Invalid("metadata is required".into()))?;
    if p.kind.is_empty() {
        return Err(ApiError::Invalid("kind is required".into()));
    }
    Ok(Resource {
        api_version: p.api_version,
        kind: p.kind,
        metadata: ObjectMeta {
            name: m.name,
            namespace: m.namespace,
            labels: m.labels.into_iter().collect(),
            annotations: m.annotations.into_iter().collect(),
            owner_uid: m.owner_uid,
            ..Default::default()
        },
        spec: json_or_null(&p.spec_json)?,
        status: Value::Null,
    })
}

pub fn parse_status(s: &str) -> Result<Value> {
    json_or_null(s)
}

pub fn event_to_pb(ev: &WatchEvent) -> pb::WatchEvent {
    let t = match ev.event_type {
        EventType::Added => pb::EventType::Added,
        EventType::Modified => pb::EventType::Modified,
        EventType::Deleted => pb::EventType::Deleted,
    };
    pb::WatchEvent { r#type: t as i32, resource: Some(resource_to_pb(&ev.resource)) }
}

pub fn rule_to_pb(r: &PolicyRule) -> pb::PolicyRule {
    pb::PolicyRule { verbs: r.verbs.clone(), kinds: r.kinds.clone(), namespaces: r.namespaces.clone() }
}

pub fn rule_from_pb(r: pb::PolicyRule) -> PolicyRule {
    PolicyRule { verbs: r.verbs, kinds: r.kinds, namespaces: r.namespaces }
}

pub fn role_to_pb(r: &Role) -> pb::Role {
    pb::Role { name: r.name.clone(), rules: r.rules.iter().map(rule_to_pb).collect(), builtin: r.builtin }
}

pub fn role_from_pb(r: pb::Role) -> Role {
    Role { name: r.name, rules: r.rules.into_iter().map(rule_from_pb).collect(), builtin: false }
}

pub fn user_to_pb(u: &UserRecord) -> pb::User {
    pb::User { username: u.username.clone(), roles: u.roles.clone(), created_at: u.created_at.clone() }
}

pub fn kind_to_pb(k: &KindInfo) -> pb::KindInfo {
    pb::KindInfo { kind: k.kind.clone(), namespaced: k.namespaced, builtin: k.builtin, api_version: k.api_version.clone() }
}

pub fn operator_to_pb(o: &OperatorRecord) -> pb::OperatorInfo {
    pb::OperatorInfo {
        name: o.name.clone(),
        kinds: o.kinds.clone(),
        registered_at: o.registered_at.clone(),
        role: o.role.clone(),
        builtin: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resource_roundtrip_drops_server_fields() {
        let mut r = Resource::new("Pod", "default", "p", json!({"a": 1})).with_labels(&[("x", "y")]);
        r.metadata.uid = "u".into();
        r.status = json!({"phase": "Running"});
        let p = resource_to_pb(&r);
        assert_eq!(p.status_json, r#"{"phase":"Running"}"#);
        let back = resource_from_pb(p).unwrap();
        assert_eq!(back.spec, json!({"a": 1}));
        assert_eq!(back.metadata.labels.get("x").unwrap(), "y");
        assert!(back.metadata.uid.is_empty());
        assert!(back.status.is_null());
    }

    #[test]
    fn rejects_bad_input() {
        let mut p = resource_to_pb(&Resource::new("Pod", "default", "p", json!({})));
        p.spec_json = "{not json".into();
        assert!(resource_from_pb(p.clone()).is_err());
        p.spec_json = String::new();
        p.metadata = None;
        assert!(resource_from_pb(p).is_err());
    }
}
