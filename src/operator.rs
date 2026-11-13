//! The operator interface.
//!
//! An operator watches one or more kinds and reconciles the actual state of the world
//! towards the desired state declared in a resource's spec. lkube supports operators in
//! two flavours:
//!
//! * **In-process** — implement [`Operator`] and register it with [`OperatorManager`].
//!   The built-in scheduler, deployment/service controllers and kubelet are all
//!   operators built on this interface.
//! * **Out-of-process** — register via `OperatorService.Register` (creates CRDs, issues
//!   a scoped token), then use `ClusterService.Watch/Apply/UpdateStatus` over gRPC.
//!   See [`crate::sdk`] and `examples/webapp_operator.rs`.

use crate::api::{Api, WatchEvent};
use crate::model::{Labels, Resource, ResourceKey};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast::error::RecvError, mpsc};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct ReconcileRequest {
    pub key: ResourceKey,
    /// Current state of the object, or `None` if it has been deleted.
    pub resource: Option<Resource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    Done,
    RequeueAfter(Duration),
}

/// Context handed to operators. Operators act with system privileges.
pub struct OperatorContext {
    pub api: Arc<Api>,
}

#[async_trait]
pub trait Operator: Send + Sync + 'static {
    /// Unique operator name.
    fn name(&self) -> &str;

    /// The kind this operator owns; all objects of this kind are periodically resynced.
    fn primary_kind(&self) -> &str;

    /// Kinds whose events may trigger a reconcile (defaults to the primary kind).
    fn watches(&self) -> Vec<String> {
        vec![self.primary_kind().to_string()]
    }

    /// Map an event on a watched kind to the primary objects that must be reconciled.
    /// Default: events on the primary kind reconcile that object; others are ignored.
    async fn map_event(&self, _ctx: &OperatorContext, event: &WatchEvent) -> Vec<ResourceKey> {
        if event.resource.kind == self.primary_kind() {
            vec![event.resource.key()]
        } else {
            vec![]
        }
    }

    fn resync_interval(&self) -> Duration {
        Duration::from_secs(10)
    }

    /// Drive the object towards its desired state. Must be idempotent.
    async fn reconcile(&self, ctx: &OperatorContext, req: ReconcileRequest) -> anyhow::Result<ReconcileAction>;
}

pub struct OperatorManager {
    ctx: Arc<OperatorContext>,
    operators: Vec<Arc<dyn Operator>>,
}

impl OperatorManager {
    pub fn new(api: Arc<Api>) -> Self {
        Self { ctx: Arc::new(OperatorContext { api }), operators: vec![] }
    }

    pub fn register(&mut self, op: Arc<dyn Operator>) {
        self.operators.push(op);
    }

    pub fn names(&self) -> Vec<(String, String)> {
        self.operators.iter().map(|o| (o.name().to_string(), o.primary_kind().to_string())).collect()
    }

    pub fn start(self) -> Vec<JoinHandle<()>> {
        self.operators
            .into_iter()
            .map(|op| tokio::spawn(run_operator(self.ctx.clone(), op)))
            .collect()
    }
}

/// Reconcile a single key once (fetching its current state). Useful in tests.
pub async fn reconcile_key(
    ctx: &OperatorContext,
    op: &dyn Operator,
    key: ResourceKey,
) -> anyhow::Result<ReconcileAction> {
    let resource = ctx.api.try_get(&key).await?;
    op.reconcile(ctx, ReconcileRequest { key, resource }).await
}

/// The control loop: events + periodic resync feed a de-duplicating work queue.
pub async fn run_operator(ctx: Arc<OperatorContext>, op: Arc<dyn Operator>) {
    let mut events = ctx.api.subscribe();
    let (queue_tx, mut queue_rx) = mpsc::unbounded_channel::<ResourceKey>();
    let mut resync = tokio::time::interval(op.resync_interval());
    let watches = op.watches();
    tracing::info!(operator = op.name(), kind = op.primary_kind(), "operator started");

    let enqueue_all = |ctx: Arc<OperatorContext>, op: Arc<dyn Operator>, tx: mpsc::UnboundedSender<ResourceKey>| async move {
        match ctx.api.list(op.primary_kind(), None, &Labels::new()).await {
            Ok(items) => items.iter().for_each(|r| {
                let _ = tx.send(r.key());
            }),
            Err(e) => tracing::warn!(operator = op.name(), error = %e, "resync list failed"),
        }
    };

    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(ev) => {
                    if watches.iter().any(|k| k == &ev.resource.kind) {
                        for key in op.map_event(&ctx, &ev).await {
                            let _ = queue_tx.send(key);
                        }
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(operator = op.name(), skipped = n, "event stream lagged; resyncing");
                    enqueue_all(ctx.clone(), op.clone(), queue_tx.clone()).await;
                }
                Err(RecvError::Closed) => break,
            },
            _ = resync.tick() => enqueue_all(ctx.clone(), op.clone(), queue_tx.clone()).await,
            Some(first) = queue_rx.recv() => {
                let mut batch = BTreeSet::from([first]);
                while let Ok(k) = queue_rx.try_recv() {
                    batch.insert(k);
                }
                for key in batch {
                    let result = reconcile_key(&ctx, op.as_ref(), key.clone()).await;
                    let requeue = match result {
                        Ok(ReconcileAction::Done) => None,
                        Ok(ReconcileAction::RequeueAfter(d)) => Some(d),
                        Err(e) => {
                            tracing::warn!(operator = op.name(), %key, error = %e, "reconcile failed");
                            Some(Duration::from_secs(5))
                        }
                    };
                    if let Some(delay) = requeue {
                        let tx = queue_tx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = tx.send(key);
                        });
                    }
                }
            }
        }
    }
}
