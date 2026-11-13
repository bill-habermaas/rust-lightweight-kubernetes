import { clusterApi } from "../api";
import type { Resource } from "../gen/lkube";
import { age, summarize, statusTone } from "../manifest";
import { Badge, ErrorBox, PageHeader, useAsync } from "../ui";

const KINDS = ["Namespace", "Node", "Deployment", "Pod", "Service", "ConfigMap"];

export function Overview({ onOpenKind }: { onOpenKind: (kind: string) => void }) {
  const { data, error, reload } = useAsync(async () => {
    const lists = await Promise.all(
      KINDS.map(async (kind) => {
        try {
          return [kind, (await clusterApi.list({ kind, namespace: "", labelSelector: {} }).response).items] as const;
        } catch {
          return [kind, null] as const; // not permitted
        }
      }),
    );
    return Object.fromEntries(lists) as Record<string, Resource[] | null>;
  }, []);

  const pods = data?.Pod ?? [];
  const phases = pods.reduce<Record<string, number>>((acc, p) => {
    const phase = (p.statusJson ? JSON.parse(p.statusJson).phase : "Pending") ?? "Pending";
    acc[phase] = (acc[phase] ?? 0) + 1;
    return acc;
  }, {});

  return (
    <>
      <PageHeader title="Overview" subtitle="Cluster health at a glance" actions={<button className="btn" onClick={reload}>Refresh</button>} />
      <ErrorBox error={error} />
      <div className="stat-grid">
        {KINDS.map((k) => (
          <button key={k} className="stat" onClick={() => onOpenKind(k)}>
            <span className="stat-num">{data?.[k] === null ? "—" : data?.[k]?.length ?? "…"}</span>
            <span className="stat-label">{k}s</span>
          </button>
        ))}
      </div>

      <div className="two-col">
        <section className="card">
          <h3>Nodes</h3>
          {(data?.Node ?? []).map((n) => {
            const spec = JSON.parse(n.specJson || "{}");
            const st = JSON.parse(n.statusJson || "{}");
            return (
              <div key={n.metadata!.name} className="node-row">
                <div>
                  <strong>{n.metadata!.name}</strong>
                  <div className="muted small">
                    {spec.address} · capacity {spec.capacityPods} pods · heartbeat {age(st.lastHeartbeat ?? "")} ago
                  </div>
                </div>
                <Badge tone={statusTone(n)}>{summarize(n)}</Badge>
              </div>
            );
          })}
          {data?.Node?.length === 0 && <p className="muted">No nodes registered.</p>}
        </section>
        <section className="card">
          <h3>Pods by phase</h3>
          {Object.keys(phases).length === 0 && <p className="muted">No pods yet.</p>}
          {Object.entries(phases).map(([phase, n]) => (
            <div key={phase} className="bar-row">
              <span className="bar-label">{phase}</span>
              <span className="bar">
                <span className={`bar-fill phase-${phase}`} style={{ width: `${(n / pods.length) * 100}%` }} />
              </span>
              <span className="bar-num">{n}</span>
            </div>
          ))}
        </section>
      </div>
    </>
  );
}
