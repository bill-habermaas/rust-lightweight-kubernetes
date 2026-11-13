//! Tests of the gRPC layer: authentication, authorization and command semantics.

mod common;

use common::*;
use lkube::pb::{
    self, admin_service_server::AdminService, auth_service_server::AuthService,
    cluster_service_server::ClusterService, operator_service_server::OperatorService,
};
use serde_json::json;
use tokio_stream::StreamExt;
use tonic::{Code, Request};

// ---------------------------------------------------------------- authentication

#[tokio::test]
async fn login_and_whoami() {
    let h = Harness::new().await;
    let token = h.admin_token().await;
    let me = h.auth.who_am_i(req(pb::Empty {}, &token)).await.unwrap().into_inner();
    assert_eq!(me.subject, "admin");
    assert_eq!(me.roles, vec!["admin"]);
    assert_eq!(me.token_kind, "session");
}

#[tokio::test]
async fn login_rejects_bad_credentials() {
    let h = Harness::new().await;
    for (u, p) in [("admin", "wrong"), ("nobody", "x")] {
        let err = h
            .auth
            .login(Request::new(pb::LoginRequest { username: u.into(), password: p.into() }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::Unauthenticated);
    }
}

#[tokio::test]
async fn commands_require_a_valid_token() {
    let h = Harness::new().await;
    let no_token = Request::new(pb::ListRequest { kind: "Pod".into(), ..Default::default() });
    assert_eq!(h.cluster.list(no_token).await.unwrap_err().code(), Code::Unauthenticated);
    let bad = req(pb::ListRequest { kind: "Pod".into(), ..Default::default() }, "garbage");
    assert_eq!(h.cluster.list(bad).await.unwrap_err().code(), Code::Unauthenticated);
}

#[tokio::test]
async fn revoked_tokens_are_rejected() {
    let h = Harness::new().await;
    let token = h.admin_token().await;
    h.auth.revoke_token(req(pb::RevokeTokenRequest { token_id: String::new() }, &token)).await.unwrap();
    let err = h.auth.who_am_i(req(pb::Empty {}, &token)).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn deleting_a_user_invalidates_sessions_and_role_changes_apply_immediately() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    h.add_user("bob", &["viewer"]).await;
    let bob = h.login("bob", "password123").await;

    let apply = || req(pb::ApplyRequest { resource: Some(resource("Deployment", "", "web", deployment("web", 1))) }, &bob);
    assert_eq!(h.cluster.apply(apply()).await.unwrap_err().code(), Code::PermissionDenied);

    h.admin
        .set_user_roles(req(pb::SetUserRolesRequest { username: "bob".into(), roles: vec!["editor".into()] }, &admin))
        .await
        .unwrap();
    h.cluster.apply(apply()).await.unwrap();

    h.admin.delete_user(req(pb::NameRequest { name: "bob".into() }, &admin)).await.unwrap();
    assert_eq!(h.auth.who_am_i(req(pb::Empty {}, &bob)).await.unwrap_err().code(), Code::Unauthenticated);
}

// ---------------------------------------------------------------- authorization

#[tokio::test]
async fn viewer_is_read_only() {
    let h = Harness::new().await;
    h.add_user("vic", &["viewer"]).await;
    let t = h.login("vic", "password123").await;
    h.cluster.list(req(pb::ListRequest { kind: "Pod".into(), ..Default::default() }, &t)).await.unwrap();
    let err = h
        .cluster
        .apply(req(pb::ApplyRequest { resource: Some(resource("Namespace", "", "x", json!({}))) }, &t))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    let err = h
        .cluster
        .delete(req(pb::ResourceRef { kind: "Namespace".into(), namespace: "".into(), name: "default".into() }, &t))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn editor_cannot_administer_identity() {
    let h = Harness::new().await;
    h.add_user("ed", &["editor"]).await;
    let t = h.login("ed", "password123").await;
    assert_eq!(h.admin.list_users(req(pb::Empty {}, &t)).await.unwrap_err().code(), Code::PermissionDenied);
    let err = h
        .admin
        .create_user(req(
            pb::CreateUserRequest { username: "eve".into(), password: "password123".into(), roles: vec!["admin".into()] },
            &t,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    let err = h
        .operators
        .register(req(pb::RegisterOperatorRequest { name: "x".into(), ..Default::default() }, &t))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn namespace_scoped_custom_role() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    for ns in ["dev", "prod"] {
        h.cluster.apply(req(pb::ApplyRequest { resource: Some(resource("Namespace", "", ns, json!({}))) }, &admin)).await.unwrap();
    }
    h.admin
        .put_role(req(
            pb::Role {
                name: "dev-deployer".into(),
                rules: vec![pb::PolicyRule {
                    verbs: vec!["create".into(), "update".into(), "get".into(), "list".into(), "scale".into()],
                    kinds: vec!["Deployment".into()],
                    namespaces: vec!["dev".into()],
                }],
                builtin: false,
            },
            &admin,
        ))
        .await
        .unwrap();
    h.admin
        .create_user(req(
            pb::CreateUserRequest { username: "dana".into(), password: "password123".into(), roles: vec!["dev-deployer".into()] },
            &admin,
        ))
        .await
        .unwrap();
    let t = h.login("dana", "password123").await;

    let apply = |ns: &str| req(pb::ApplyRequest { resource: Some(resource("Deployment", ns, "web", deployment("web", 1))) }, &t);
    h.cluster.apply(apply("dev")).await.unwrap();
    assert_eq!(h.cluster.apply(apply("prod")).await.unwrap_err().code(), Code::PermissionDenied);

    h.cluster.scale(req(pb::ScaleRequest { namespace: "dev".into(), name: "web".into(), replicas: 3 }, &t)).await.unwrap();
    // listing all namespaces is not allowed, listing dev is
    let all = pb::ListRequest { kind: "Deployment".into(), ..Default::default() };
    assert_eq!(h.cluster.list(req(all, &t)).await.unwrap_err().code(), Code::PermissionDenied);
    let dev = pb::ListRequest { kind: "Deployment".into(), namespace: "dev".into(), ..Default::default() };
    assert_eq!(h.cluster.list(req(dev, &t)).await.unwrap().into_inner().items.len(), 1);
    // no delete verb
    let del = pb::ResourceRef { kind: "Deployment".into(), namespace: "dev".into(), name: "web".into() };
    assert_eq!(h.cluster.delete(req(del, &t)).await.unwrap_err().code(), Code::PermissionDenied);
}

#[tokio::test]
async fn api_tokens_cannot_escalate() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    // give editors token-creation rights via a custom role
    h.admin
        .put_role(req(
            pb::Role {
                name: "token-maker".into(),
                rules: vec![pb::PolicyRule { verbs: vec!["create".into()], kinds: vec!["system:tokens".into()], namespaces: vec![] }],
                builtin: false,
            },
            &admin,
        ))
        .await
        .unwrap();
    h.add_user("ed", &["editor", "token-maker"]).await;
    let t = h.login("ed", "password123").await;

    let escalate = pb::CreateTokenRequest { subject: String::new(), roles: vec!["admin".into()], ttl_seconds: 60 };
    assert_eq!(h.auth.create_token(req(escalate, &t)).await.unwrap_err().code(), Code::PermissionDenied);

    let ok = pb::CreateTokenRequest { subject: "ci-bot".into(), roles: vec!["editor".into()], ttl_seconds: 60 };
    let api_token = h.auth.create_token(req(ok, &t)).await.unwrap().into_inner();
    let me = h.auth.who_am_i(req(pb::Empty {}, &api_token.token)).await.unwrap().into_inner();
    assert_eq!(me.subject, "ci-bot");
    assert_eq!(me.token_kind, "api");
    assert_eq!(me.roles, vec!["editor"]);
}

#[tokio::test]
async fn builtin_roles_are_protected() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    let err = h.admin.delete_role(req(pb::NameRequest { name: "admin".into() }, &admin)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let err = h
        .admin
        .put_role(req(pb::Role { name: "viewer".into(), rules: vec![], builtin: false }, &admin))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    let roles = h.admin.list_roles(req(pb::Empty {}, &admin)).await.unwrap().into_inner().roles;
    assert!(roles.iter().any(|r| r.name == "admin" && r.builtin));
}

#[tokio::test]
async fn admin_cannot_delete_self() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    let err = h.admin.delete_user(req(pb::NameRequest { name: "admin".into() }, &admin)).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
}

// ---------------------------------------------------------------- cluster commands

#[tokio::test]
async fn apply_get_list_delete_roundtrip() {
    let h = Harness::new().await;
    let t = h.admin_token().await;
    let created = h
        .cluster
        .apply(req(pb::ApplyRequest { resource: Some(resource("Deployment", "", "web", deployment("web", 2))) }, &t))
        .await
        .unwrap()
        .into_inner();
    let meta = created.metadata.unwrap();
    assert_eq!(meta.namespace, "default");
    assert!(!meta.uid.is_empty());

    let get = pb::ResourceRef { kind: "Deployment".into(), namespace: "".into(), name: "web".into() };
    let got = h.cluster.get(req(get.clone(), &t)).await.unwrap().into_inner();
    let spec: serde_json::Value = serde_json::from_str(&got.spec_json).unwrap();
    assert_eq!(spec["replicas"], 2);

    let list = pb::ListRequest { kind: "Deployment".into(), ..Default::default() };
    assert_eq!(h.cluster.list(req(list, &t)).await.unwrap().into_inner().items.len(), 1);

    h.cluster.delete(req(get.clone(), &t)).await.unwrap();
    assert_eq!(h.cluster.get(req(get, &t)).await.unwrap_err().code(), Code::NotFound);
}

#[tokio::test]
async fn invalid_objects_are_rejected() {
    let h = Harness::new().await;
    let t = h.admin_token().await;
    let bad_json = pb::Resource { spec_json: "{nope".into(), ..resource("Pod", "", "p", json!({})) };
    let err = h.cluster.apply(req(pb::ApplyRequest { resource: Some(bad_json) }, &t)).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    let unknown = resource("Gizmo", "", "g", json!({}));
    let err = h.cluster.apply(req(pb::ApplyRequest { resource: Some(unknown) }, &t)).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn label_selector_filtering() {
    let h = Harness::new().await;
    let t = h.admin_token().await;
    for (name, app) in [("a", "web"), ("b", "db")] {
        let mut r = resource("ConfigMap", "", name, json!({"data": {"k": "v"}}));
        r.metadata.as_mut().unwrap().labels.insert("app".into(), app.into());
        h.cluster.apply(req(pb::ApplyRequest { resource: Some(r) }, &t)).await.unwrap();
    }
    let mut list = pb::ListRequest { kind: "ConfigMap".into(), ..Default::default() };
    list.label_selector.insert("app".into(), "db".into());
    let items = h.cluster.list(req(list, &t)).await.unwrap().into_inner().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].metadata.as_ref().unwrap().name, "b");
}

#[tokio::test]
async fn watch_streams_events_filtered_by_permission() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    h.cluster.apply(req(pb::ApplyRequest { resource: Some(resource("Namespace", "", "dev", json!({}))) }, &admin)).await.unwrap();
    h.admin
        .put_role(req(
            pb::Role {
                name: "dev-watcher".into(),
                rules: vec![pb::PolicyRule { verbs: vec!["watch".into()], kinds: vec!["*".into()], namespaces: vec!["dev".into()] }],
                builtin: false,
            },
            &admin,
        ))
        .await
        .unwrap();
    h.add_user("wanda", &["dev-watcher"]).await;
    let t = h.login("wanda", "password123").await;

    let mut stream = h
        .cluster
        .watch(req(pb::WatchRequest { kinds: vec!["ConfigMap".into()], namespace: String::new(), send_initial: false }, &t))
        .await
        .unwrap()
        .into_inner();

    for ns in ["default", "dev"] {
        let r = resource("ConfigMap", ns, "cm", json!({"data": {}}));
        h.cluster.apply(req(pb::ApplyRequest { resource: Some(r) }, &admin)).await.unwrap();
    }
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(ev.r#type, pb::EventType::Added as i32);
    // only the event from the permitted namespace arrives
    assert_eq!(ev.resource.unwrap().metadata.unwrap().namespace, "dev");
    assert!(tokio::time::timeout(std::time::Duration::from_millis(200), stream.next()).await.is_err());
}

#[tokio::test]
async fn watch_send_initial_lists_existing_objects() {
    let h = Harness::new().await;
    let t = h.admin_token().await;
    let mut stream = h
        .cluster
        .watch(req(pb::WatchRequest { kinds: vec!["Namespace".into()], namespace: String::new(), send_initial: true }, &t))
        .await
        .unwrap()
        .into_inner();
    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(ev.resource.unwrap().metadata.unwrap().name, "default");
}

#[tokio::test]
async fn list_kinds_includes_builtins() {
    let h = Harness::new().await;
    let t = h.admin_token().await;
    let kinds = h.cluster.list_kinds(req(pb::Empty {}, &t)).await.unwrap().into_inner().kinds;
    for k in ["Namespace", "Node", "Pod", "Deployment", "Service", "ConfigMap", "CustomResourceDefinition"] {
        assert!(kinds.iter().any(|x| x.kind == k), "missing {k}");
    }
}

// ---------------------------------------------------------------- operators

#[tokio::test]
async fn operator_registration_issues_scoped_token() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    let reg = h
        .operators
        .register(req(
            pb::RegisterOperatorRequest {
                name: "webapp-operator".into(),
                crds: vec![pb::CrdSpec {
                    kind: "WebApp".into(),
                    namespaced: true,
                    api_version: "example.com/v1".into(),
                    schema_json: r#"{"required":["image"]}"#.into(),
                }],
                extra_rules: vec![pb::PolicyRule {
                    verbs: vec!["create".into(), "update".into(), "get".into()],
                    kinds: vec!["Deployment".into()],
                    namespaces: vec![],
                }],
                token_ttl_seconds: 0,
            },
            &admin,
        ))
        .await
        .unwrap()
        .into_inner();
    let op_token = reg.token;
    assert_eq!(reg.operator.unwrap().kinds, vec!["WebApp"]);

    // the operator can manage its CRD kind and the extra kinds…
    let wa = resource("WebApp", "", "site", json!({"image": "nginx"}));
    h.cluster.apply(req(pb::ApplyRequest { resource: Some(wa) }, &op_token)).await.unwrap();
    let st = pb::UpdateStatusRequest {
        target: Some(pb::ResourceRef { kind: "WebApp".into(), namespace: "".into(), name: "site".into() }),
        status_json: r#"{"phase":"Ready"}"#.into(),
    };
    let updated = h.cluster.update_status(req(st, &op_token)).await.unwrap().into_inner();
    assert_eq!(updated.status_json, r#"{"phase":"Ready"}"#);
    let dep = resource("Deployment", "", "site", deployment("site", 1));
    h.cluster.apply(req(pb::ApplyRequest { resource: Some(dep) }, &op_token)).await.unwrap();

    // …but nothing else.
    let ns = resource("Namespace", "", "evil", json!({}));
    let err = h.cluster.apply(req(pb::ApplyRequest { resource: Some(ns) }, &op_token)).await.unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
    let del = pb::ResourceRef { kind: "Deployment".into(), namespace: "".into(), name: "site".into() };
    assert_eq!(h.cluster.delete(req(del, &op_token)).await.unwrap_err().code(), Code::PermissionDenied);

    // schema validation applies to custom resources
    let bad = resource("WebApp", "", "bad", json!({"replicas": 1}));
    let err = h.cluster.apply(req(pb::ApplyRequest { resource: Some(bad) }, &op_token)).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);

    // listing shows built-in and registered operators
    let ops = h.operators.list(req(pb::Empty {}, &admin)).await.unwrap().into_inner().operators;
    assert!(ops.iter().any(|o| o.name == "scheduler" && o.builtin));
    assert!(ops.iter().any(|o| o.name == "webapp-operator" && !o.builtin));

    // unregistering revokes the token
    h.operators.unregister(req(pb::NameRequest { name: "webapp-operator".into() }, &admin)).await.unwrap();
    let err = h.cluster.list_kinds(req(pb::Empty {}, &op_token)).await.unwrap_err();
    assert_eq!(err.code(), Code::Unauthenticated);
}

#[tokio::test]
async fn operator_extra_rules_cannot_exceed_caller_permissions() {
    let h = Harness::new().await;
    let admin = h.admin_token().await;
    h.admin
        .put_role(req(
            pb::Role {
                name: "op-registrar".into(),
                rules: vec![
                    pb::PolicyRule { verbs: vec!["create".into()], kinds: vec!["system:operators".into()], namespaces: vec![] },
                    pb::PolicyRule { verbs: vec!["create".into(), "update".into()], kinds: vec!["CustomResourceDefinition".into()], namespaces: vec![] },
                ],
                builtin: false,
            },
            &admin,
        ))
        .await
        .unwrap();
    h.add_user("olga", &["op-registrar"]).await;
    let t = h.login("olga", "password123").await;
    let err = h
        .operators
        .register(req(
            pb::RegisterOperatorRequest {
                name: "greedy".into(),
                crds: vec![],
                extra_rules: vec![pb::PolicyRule { verbs: vec!["*".into()], kinds: vec!["*".into()], namespaces: vec![] }],
                token_ttl_seconds: 0,
            },
            &t,
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::PermissionDenied);
}

#[tokio::test]
async fn logs_for_running_pod() {
    use lkube::controllers::{Kubelet, Scheduler};
    use lkube::operator::{reconcile_key, OperatorContext};
    let h = Harness::new().await;
    let t = h.admin_token().await;
    lkube::controllers::heartbeat_once(&h.state.api, "node-1", "127.0.0.1", 10).await.unwrap();
    let pod = resource("Pod", "", "hello", json!({"containers": [{"name": "c", "image": "busybox"}]}));
    h.cluster.apply(req(pb::ApplyRequest { resource: Some(pod) }, &t)).await.unwrap();

    let ctx = OperatorContext { api: h.state.api.clone() };
    let key = lkube::model::ResourceKey::new("Pod", "default", "hello");
    reconcile_key(&ctx, &Scheduler, key.clone()).await.unwrap();
    reconcile_key(&ctx, &Kubelet::new("node-1", h.runtime.clone()), key).await.unwrap();

    let logs = h
        .cluster
        .logs(req(pb::LogsRequest { namespace: "".into(), pod: "hello".into(), container: "".into(), tail: 10 }, &t))
        .await
        .unwrap()
        .into_inner();
    assert!(logs.text.contains("busybox"));
}
