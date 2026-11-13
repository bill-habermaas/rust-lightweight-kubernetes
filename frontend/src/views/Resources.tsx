import { useEffect, useRef, useState } from "react";
import { clusterApi, errorMessage } from "../api";
import type { KindInfo, Resource } from "../gen/lkube";
import { age, fromResource, statusTone, summarize, toYaml } from "../manifest";
import { Badge, ErrorBox, Modal, Notice, PageHeader, useAsync } from "../ui";

interface Props {
  kind: KindInfo;
  namespaces: string[];
  onEdit: (yaml: string) => void;
}

export function Resources({ kind, namespaces, onEdit }: Props) {
  const [ns, setNs] = useState("");
  const [selector, setSelector] = useState("");
  const [viewing, setViewing] = useState<Resource>();
  const [logsFor, setLogsFor] = useState<Resource>();
  const [notice, setNotice] = useState<string>();
  const [actionError, setActionError] = useState<string>();

  const labelSelector = Object.fromEntries(
    selector
      .split(",")
      .map((s) => s.trim().split("="))
      .filter((p) => p.length === 2 && p[0]),
  );

  const { data, error, reload } = useAsync(
    async () => (await clusterApi.list({ kind: kind.kind, namespace: ns, labelSelector }).response).items,
    [kind.kind, ns, selector],
  );

  // Live updates: re-list whenever a watch event for this kind arrives.
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    const abort = new AbortController();
    let timer: number | undefined;
    const call = clusterApi.watch({ kinds: [kind.kind], namespace: ns, sendInitial: false }, { abort: abort.signal });
    call.responses.onMessage(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => reloadRef.current(), 250);
    });
    call.responses.onError(() => {});
    return () => {
      window.clearTimeout(timer);
      abort.abort();
    };
  }, [kind.kind, ns]);

  async function act(label: string, fn: () => Promise<unknown>) {
    setActionError(undefined);
    try {
      await fn();
      setNotice(label);
      setTimeout(() => setNotice(undefined), 2500);
      reload();
    } catch (e) {
      setActionError(errorMessage(e));
    }
  }

  const del = (r: Resource) => {
    const md = r.metadata!;
    if (!confirm(`Delete ${r.kind} ${md.namespace ? md.namespace + "/" : ""}${md.name}? Owned objects are deleted too.`)) return;
    act(`Deleted ${md.name}`, () => clusterApi.delete({ kind: r.kind, namespace: md.namespace, name: md.name }).response);
  };

  const scale = (r: Resource) => {
    const current = JSON.parse(r.specJson || "{}").replicas ?? 1;
    const value = prompt(`Replicas for ${r.metadata!.name}`, String(current));
    if (value === null) return;
    const replicas = Number(value);
    if (!Number.isInteger(replicas) || replicas < 0) return setActionError("Replicas must be a non-negative integer");
    act(`Scaled ${r.metadata!.name} to ${replicas}`, () =>
      clusterApi.scale({ namespace: r.metadata!.namespace, name: r.metadata!.name, replicas }).response,
    );
  };

  return (
    <>
      <PageHeader
        title={kind.kind}
        subtitle={`${kind.apiVersion} · ${kind.namespaced ? "namespaced" : "cluster-scoped"}${kind.builtin ? "" : " · custom resource"}`}
        actions={
          <>
            {kind.namespaced && (
              <select value={ns} onChange={(e) => setNs(e.target.value)} aria-label="Namespace">
                <option value="">All namespaces</option>
                {namespaces.map((n) => (
                  <option key={n}>{n}</option>
                ))}
              </select>
            )}
            <input
              className="selector"
              placeholder="label selector  app=web,tier=fe"
              value={selector}
              onChange={(e) => setSelector(e.target.value)}
            />
            <button className="btn" onClick={reload}>
              Refresh
            </button>
          </>
        }
      />
      <ErrorBox error={error ?? actionError} />
      <Notice text={notice} />
      <div className="card table-card">
        <table>
          <thead>
            <tr>
              <th>Name</th>
              {kind.namespaced && <th>Namespace</th>}
              <th>Status</th>
              <th>Labels</th>
              <th>Age</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(data ?? []).map((r) => {
              const md = r.metadata!;
              return (
                <tr key={md.uid}>
                  <td>
                    <button className="link" onClick={() => setViewing(r)}>
                      {md.name}
                    </button>
                  </td>
                  {kind.namespaced && <td>{md.namespace}</td>}
                  <td>
                    <Badge tone={statusTone(r)}>{summarize(r) || "—"}</Badge>
                  </td>
                  <td className="labels">
                    {Object.entries(md.labels).map(([k, v]) => (
                      <span key={k} className="chip">
                        {k}={v}
                      </span>
                    ))}
                  </td>
                  <td className="muted">{age(md.createdAt)}</td>
                  <td className="actions">
                    {r.kind === "Deployment" && (
                      <button className="btn btn-sm" onClick={() => scale(r)}>
                        Scale
                      </button>
                    )}
                    {r.kind === "Pod" && (
                      <button className="btn btn-sm" onClick={() => setLogsFor(r)}>
                        Logs
                      </button>
                    )}
                    <button className="btn btn-sm" onClick={() => onEdit(toYaml(fromResource(r, false)))}>
                      Edit
                    </button>
                    <button className="btn btn-sm btn-danger" onClick={() => del(r)}>
                      Delete
                    </button>
                  </td>
                </tr>
              );
            })}
            {data?.length === 0 && (
              <tr>
                <td colSpan={6} className="empty">
                  No {kind.kind} objects{ns ? ` in ${ns}` : ""}.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {viewing && (
        <Modal title={`${viewing.kind} ${viewing.metadata!.name}`} onClose={() => setViewing(undefined)} wide>
          <pre className="code">{toYaml(fromResource(viewing))}</pre>
        </Modal>
      )}
      {logsFor && <LogsModal pod={logsFor} onClose={() => setLogsFor(undefined)} />}
    </>
  );
}

function LogsModal({ pod, onClose }: { pod: Resource; onClose: () => void }) {
  const containers: string[] = (JSON.parse(pod.specJson || "{}").containers ?? []).map((c: { name: string }) => c.name);
  const [container, setContainer] = useState(containers[0] ?? "");
  const { data, error, reload, loading } = useAsync(
    async () =>
      (await clusterApi.logs({ namespace: pod.metadata!.namespace, pod: pod.metadata!.name, container, tail: 500 }).response).text,
    [container],
  );
  return (
    <Modal title={`Logs · ${pod.metadata!.name}`} onClose={onClose} wide>
      <div className="row">
        {containers.length > 1 && (
          <select value={container} onChange={(e) => setContainer(e.target.value)}>
            {containers.map((c) => (
              <option key={c}>{c}</option>
            ))}
          </select>
        )}
        <button className="btn btn-sm" onClick={reload} disabled={loading}>
          Refresh
        </button>
      </div>
      <ErrorBox error={error} />
      <pre className="code logs">{data || (loading ? "Loading…" : "(no output)")}</pre>
    </Modal>
  );
}
