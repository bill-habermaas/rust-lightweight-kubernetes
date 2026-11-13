//! MongoDB store tests. Skipped unless LKUBE_TEST_MONGO_URI is set, e.g.
//!   docker compose up -d mongo   (and publish 27017, or run inside the network)
//!   LKUBE_TEST_MONGO_URI=mongodb://localhost:27017 cargo test --test mongo_store

use lkube::authz::builtin_roles;
use lkube::error::ApiError;
use lkube::model::{kinds, Resource};
use lkube::store::{MongoStore, OperatorRecord, Store, UserRecord};
use serde_json::json;

async fn store() -> Option<MongoStore> {
    let uri = std::env::var("LKUBE_TEST_MONGO_URI").ok()?;
    let db = format!("lkube_test_{}", lkube::model::random_suffix(8));
    let s = MongoStore::connect(&uri, &db).await.expect("connect to MongoDB");
    Some(s)
}

#[tokio::test]
async fn mongo_resources_crud_and_cas() {
    let Some(s) = store().await else {
        eprintln!("LKUBE_TEST_MONGO_URI not set; skipping");
        return;
    };
    let mut r = Resource::new(kinds::POD, "default", "p1", json!({"containers": [{"name": "c", "image": "nginx"}]}));
    r.metadata.labels.insert("lkube.io/deployment".into(), "web".into()); // dotted keys
    let v1 = s.create(r.clone()).await.unwrap();
    assert_eq!(v1.metadata.resource_version, 1);
    assert!(matches!(s.create(r.clone()).await, Err(ApiError::AlreadyExists(_))));

    let got = s.get(&r.key()).await.unwrap().unwrap();
    assert_eq!(got, v1);

    let v2 = s.update(v1.clone()).await.unwrap();
    assert_eq!(v2.metadata.resource_version, 2);
    assert!(matches!(s.update(v1).await, Err(ApiError::Conflict(_))));

    s.create(Resource::new(kinds::POD, "other", "p2", json!({}))).await.unwrap();
    assert_eq!(s.list(kinds::POD, None).await.unwrap().len(), 2);
    assert_eq!(s.list(kinds::POD, Some("default")).await.unwrap().len(), 1);

    assert!(s.delete(&r.key()).await.unwrap().is_some());
    assert!(s.get(&r.key()).await.unwrap().is_none());
    s.database().drop().await.unwrap();
}

#[tokio::test]
async fn mongo_identity_and_operators() {
    let Some(s) = store().await else {
        eprintln!("LKUBE_TEST_MONGO_URI not set; skipping");
        return;
    };
    for r in builtin_roles() {
        s.put_role(r).await.unwrap();
    }
    assert_eq!(s.list_roles().await.unwrap().len(), builtin_roles().len());
    let u = UserRecord { username: "alice".into(), password_hash: "h".into(), roles: vec!["viewer".into()], created_at: "now".into() };
    s.put_user(u.clone()).await.unwrap();
    assert_eq!(s.get_user("alice").await.unwrap(), Some(u));
    assert!(s.delete_user("alice").await.unwrap());

    s.revoke_token("jti-1", chrono::Utc::now().timestamp() + 60).await.unwrap();
    assert!(s.is_token_revoked("jti-1").await.unwrap());
    assert!(!s.is_token_revoked("jti-2").await.unwrap());

    let op = OperatorRecord {
        name: "op".into(),
        kinds: vec!["WebApp".into()],
        role: "operator:op".into(),
        token_id: "t".into(),
        token_expires_at: 0,
        registered_at: "now".into(),
    };
    s.put_operator(op.clone()).await.unwrap();
    assert_eq!(s.list_operators().await.unwrap(), vec![op]);
    s.database().drop().await.unwrap();
}
