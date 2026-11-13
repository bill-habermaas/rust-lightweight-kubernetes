# lkube — a lightweight Kubernetes-like orchestrator in Rust

lkube is a single-binary container orchestrator with the core Kubernetes model
(declarative objects, reconcile loops, scheduling, a node agent, CRDs and operators).
All commands use **gRPC**, are **authenticated with bearer tokens** and **authorized with RBAC**.
State is kept in **MongoDB**. A **React** console drives everything through gRPC-Web.

```
 ┌──────────────┐  gRPC-Web   ┌──────────┐                ┌──────────────────────────────────────────┐
 │ React UI     │────────────▶│  nginx   │──HTTP/1.1────▶│ lkube-server (tonic, :50051)             │
 └──────────────┘             └──────────┘                │  AuthService  AdminService               │
 ┌──────────────┐  native gRPC (HTTP/2)                   │  ClusterService  OperatorService         │
 │ CLI / grpcurl│────────────────────────────────────────▶│    │ authn (JWT) → authz (RBAC)           │
 │ operators    │                                          │    ▼                                     │
 └──────────────┘                                          │  Api: validate · apply · cascade · watch │
                                                           │    │                  │ events           │
                                                           │    ▼                  ▼                  │
                                                           │  Store ─▶ MongoDB   OperatorManager      │
                                                           │                      scheduler           │
                                                           │                      deployment-ctrl     │
                                                           │                      service-ctrl        │
                                                           │                      kubelet ─▶ Docker   │
                                                           └──────────────────────────────────────────┘
```

## Quick start (Docker Compose)

```bash
cp .env.example .env            # set LKUBE_JWT_SECRET and the admin password
docker compose up -d --build
open http://localhost:8080      # sign in as admin / admin12345 (or what you set)
```

Services: `mongo` (persistence), `lkube-server` (API + control plane + node agent;
mounts `/var/run/docker.sock` to run pods as containers on the `lkube-pods` network),
`ui` (nginx serving the React app and proxying gRPC-Web).

Add the example external operator: `docker compose --profile operators up -d`.
No Docker socket available? Set `LKUBE_RUNTIME=mock` to simulate containers.

## Open in VS Code

Open the folder and install the recommended extensions (rust-analyzer, CodeLLDB, Docker, …).

* **Run → "Debug lkube-server (memory store, mock runtime)"** — no dependencies needed.
* **Run → "Server + UI"** — together with the `ui: dev server` task (Vite on :5173, proxies gRPC-Web to :50051).
* **Tasks**: `cargo test`, `cargo clippy`, `ui: test`, `compose: up/down`, `mongo: start (docker)`, `mongo: store tests`,
  `ui: regenerate gRPC client`.

## Resource model

Objects look like Kubernetes manifests (`kind`, `metadata`, `spec`, `status`). Built-in kinds:

| Kind | Scope | What the control plane does |
|---|---|---|
| `Namespace` | cluster | deleting one deletes everything in it (`default` is protected) |
| `Node` | cluster | registered + heartbeated by each node agent; stale after 40 s |
| `Pod` | namespaced | scheduled to a node, run as containers, restart policy `Always`/`OnFailure`/`Never` |
| `Deployment` | namespaced | keeps `replicas` pods; template changes replace pods (recreate) |
| `Service` | namespaced | `status.endpoints` = running pods matching `selector` |
| `ConfigMap` | namespaced | key/value data |
| `CustomResourceDefinition` | cluster | registers a new kind (+ optional schema) |

Semantics: `Apply` is create-or-update (status preserved, `generation` bumped on spec change);
every write uses optimistic concurrency on `resourceVersion`; objects with `ownerUid` are
garbage-collected with their owner; every change is published to `Watch` streams.

```yaml
kind: Deployment
metadata: { name: web, namespace: default }
spec:
  replicas: 2
  template:
    labels: { app: web }
    spec:
      containers:
        - name: nginx
          image: nginx:1.27
          ports: [{ containerPort: 80, hostPort: 8081 }]
```

## gRPC API

Defined in [`proto/lkube.proto`](proto/lkube.proto). Every call except `AuthService.Login` needs
`authorization: Bearer <token>` metadata.

| Service | RPCs |
|---|---|
| `AuthService` | `Login`, `WhoAmI`, `CreateToken`, `RevokeToken` |
| `AdminService` | `CreateUser`, `ListUsers`, `SetUserRoles`, `DeleteUser`, `PutRole`, `ListRoles`, `DeleteRole` |
| `ClusterService` | `Apply`, `Get`, `List`, `Delete`, `Scale`, `UpdateStatus`, `Watch` (server stream), `Logs`, `ListKinds` |
| `OperatorService` | `Register`, `List`, `Unregister` |

```bash
TOKEN=$(grpcurl -plaintext -import-path proto -proto lkube.proto \
  -d '{"username":"admin","password":"admin12345"}' localhost:50051 lkube.v1.AuthService/Login | jq -r .token)

grpcurl -plaintext -import-path proto -proto lkube.proto -H "authorization: Bearer $TOKEN" \
  -d '{"kind":"Pod"}' localhost:50051 lkube.v1.ClusterService/List

grpcurl -plaintext -import-path proto -proto lkube.proto -H "authorization: Bearer $TOKEN" \
  -d '{"namespace":"default","name":"web","replicas":5}' localhost:50051 lkube.v1.ClusterService/Scale
```

The same port serves native gRPC (HTTP/2) and gRPC-Web (HTTP/1.1, used by the browser).

## Authentication

* **Session tokens** — `Login` with username/password (Argon2id hashes in MongoDB) returns an
  HS256 JWT (`LKUBE_TOKEN_TTL_SECS`, default 1 h). Roles are re-read from the user record on each
  request, so role changes and user deletion take effect immediately.
* **API tokens** — `CreateToken` issues long-lived tokens for CI/scripts with fixed roles; a caller
  can only grant roles they hold (admins excepted).
* **Operator tokens** — issued by `OperatorService.Register`.
* **Revocation** — `RevokeToken` (empty id = log out) stores the token id in MongoDB with a TTL index.

## Authorization (RBAC)

Each command maps to `(verb, kind, namespace)`:

| Command | Verb | Kind |
|---|---|---|
| Apply | `create` (new) / `update` (existing) | object kind |
| Get / List / Watch / Delete | `get` / `list` / `watch` / `delete` | object kind |
| Scale | `scale` | `Deployment` |
| UpdateStatus | `status` | object kind |
| Logs | `logs` | `Pod` |
| user / role / token / operator administration | `create`/`list`/`update`/`delete` | `system:users` / `system:roles` / `system:tokens` / `system:operators` |

A role is a list of rules `{verbs, kinds, namespaces}`; `*` is a wildcard. `*` in `kinds` covers all
resource kinds but **not** `system:*` kinds. Empty `namespaces` = all. Listing all namespaces needs
a rule that covers every namespace. Watch streams only deliver events the caller may `watch`.

Built-in roles: `admin` (everything), `editor` (all verbs on resources, no identity admin),
`viewer` (get/list/watch/logs), `operator` (read-only base for operators). Custom roles are
managed in the UI (Access control → Roles) or with `AdminService.PutRole`.

## Operators

lkube exposes the operator pattern in two ways.

**In-process** — implement `lkube::operator::Operator` and register it with `OperatorManager`.
The built-in scheduler, deployment and service controllers, and the kubelet all use this trait.

```rust
#[async_trait]
impl Operator for EchoOperator {
    fn name(&self) -> &str { "echo" }
    fn primary_kind(&self) -> &str { "Echo" }          // resynced periodically
    // fn watches(&self) -> Vec<String>                // extra kinds that trigger reconciles
    // async fn map_event(&self, ctx, ev) -> Vec<ResourceKey>  // map child events to parents
    async fn reconcile(&self, ctx: &OperatorContext, req: ReconcileRequest) -> anyhow::Result<ReconcileAction> {
        if let Some(r) = req.resource {
            ctx.api.update_status(&r.key(), json!({"echo": r.spec["message"]})).await?;
        }
        Ok(ReconcileAction::Done)
    }
}
```

The manager feeds each operator from watch events and a periodic resync into a de-duplicating work
queue, and requeues on errors or `RequeueAfter`.

**Out-of-process (any language)** — call `OperatorService.Register` with your CRDs and the extra
permissions you need. The server creates the CRDs, a role `operator:<name>` (full control of your
kinds plus the requested rules, which must not exceed the caller's own permissions) and returns a
scoped token. The operator then uses `ClusterService.Watch` / `Apply` / `UpdateStatus`. Set
`metadata.ownerUid` on child objects so they are deleted with the custom resource.

`lkube::sdk` provides `LkubeClient` and `run_remote_operator` for Rust; see
[`examples/webapp_operator.rs`](examples/webapp_operator.rs), which turns a `WebApp` custom resource into a Deployment + Service and
reports readiness in the WebApp's status:

```bash
cargo run --example webapp_operator     # registers itself using LKUBE_USER / LKUBE_PASSWORD
```

## Configuration

| Variable | Default | |
|---|---|---|
| `LKUBE_LISTEN` | `0.0.0.0:50051` | gRPC + gRPC-Web listen address |
| `LKUBE_STORE` | `mongo` | `mongo` or `memory` |
| `LKUBE_MONGO_URI` / `LKUBE_MONGO_DB` | `mongodb://localhost:27017` / `lkube` | |
| `LKUBE_JWT_SECRET` | dev secret (warns) | **set this** |
| `LKUBE_TOKEN_TTL_SECS` | `3600` | session token lifetime |
| `LKUBE_ADMIN_USER` / `LKUBE_ADMIN_PASSWORD` | `admin` / `admin12345` | created only when no users exist |
| `LKUBE_RUNTIME` | `docker` | `docker` or `mock` |
| `LKUBE_DOCKER_NETWORK` | — | network pod containers join |
| `LKUBE_NODE_NAME` / `LKUBE_NODE_ADDRESS` / `LKUBE_NODE_CAPACITY` | hostname / hostname / `110` | this node |
| `LKUBE_CONTROLLERS` | `true` | run scheduler + controllers on this instance |

## Tests

```bash
cargo test                       # unit + gRPC API + control-plane tests (no external deps)
cd frontend && npm test          # manifest parsing / conversion tests
LKUBE_TEST_MONGO_URI=mongodb://localhost:27017 cargo test --test mongo_store
```

* `src/**` unit tests — token issue/verify/expiry/signature, password hashing, RBAC matching,
  validation, apply/generation semantics, cascading deletes, CRD schemas, scheduling, scale planning,
  restart policies, the in-memory store.
* `tests/grpc_api.rs` — every service through the gRPC layer: authentication failures, revocation,
  per-namespace roles, privilege-escalation guards, watch filtering, operator registration/tokens.
* `tests/control_plane.rs` — Deployment → Pods → scheduling → containers → Service endpoints;
  scaling, rollout, cascade, restart policies, image-pull failures, GC, and the `OperatorManager` loop
  with a custom operator.
* `tests/mongo_store.rs` — MongoDB store (only runs when `LKUBE_TEST_MONGO_URI` is set).

## Project layout

```
proto/lkube.proto        gRPC contract
src/server.rs            gRPC services (authn + authz per command)
src/auth.rs, authz.rs, security.rs   tokens, RBAC rules, request guards
src/api.rs               validation, apply, cascade delete, watch events
src/store/               MongoStore + MemoryStore
src/operator.rs          Operator trait + OperatorManager
src/controllers/         scheduler, deployment, service, kubelet
src/runtime/             DockerRuntime (bollard) + MockRuntime
src/sdk.rs               client SDK for external operators
examples/webapp_operator.rs
frontend/                React + Vite console (gRPC-Web via protobuf-ts; src/gen is generated)
Dockerfile, frontend/Dockerfile, docker-compose.yml
```

## Differences from Kubernetes

This is deliberately small. It does not include overlay networking, a service proxy or DNS (Services
publish endpoints; use `hostPort` or the `lkube-pods` network), volumes, rolling updates (template
changes recreate pods), admission webhooks, or HA control planes. Events are broadcast in process,
so for more than one node run extra instances with `LKUBE_CONTROLLERS=false` and a distinct
`LKUBE_NODE_NAME`; their kubelets pick up work through the periodic resync (pod logs are then served by the instance on that node).
