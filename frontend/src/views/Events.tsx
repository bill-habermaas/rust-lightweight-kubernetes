import { useEffect, useState } from "react";
import { clusterApi, errorMessage } from "../api";
import { EventType } from "../gen/lkube";
import { summarize } from "../manifest";
import { ErrorBox, PageHeader } from "../ui";

interface Row {
  id: number;
  at: string;
  type: string;
  kind: string;
  name: string;
  summary: string;
}

let seq = 0;

export function Events() {
  const [rows, setRows] = useState<Row[]>([]);
  const [paused, setPaused] = useState(false);
  const [filter, setFilter] = useState("");
  const [error, setError] = useState<string>();

  useEffect(() => {
    if (paused) return;
    const abort = new AbortController();
    const call = clusterApi.watch({ kinds: [], namespace: "", sendInitial: false }, { abort: abort.signal });
    call.responses.onMessage((ev) => {
      const r = ev.resource!;
      const md = r.metadata!;
      setRows((prev) =>
        [
          {
            id: ++seq,
            at: new Date().toLocaleTimeString(),
            type: EventType[ev.type],
            kind: r.kind,
            name: md.namespace ? `${md.namespace}/${md.name}` : md.name,
            summary: summarize(r),
          },
          ...prev,
        ].slice(0, 500),
      );
    });
    call.responses.onError((e) => {
      if (!abort.signal.aborted) setError(errorMessage(e));
    });
    return () => abort.abort();
  }, [paused]);

  const shown = rows.filter((r) => !filter || `${r.kind} ${r.name}`.toLowerCase().includes(filter.toLowerCase()));

  return (
    <>
      <PageHeader
        title="Events"
        subtitle="Live watch stream (gRPC server streaming) of every object you are allowed to watch"
        actions={
          <>
            <input placeholder="filter" value={filter} onChange={(e) => setFilter(e.target.value)} />
            <button className="btn" onClick={() => setPaused(!paused)}>
              {paused ? "Resume" : "Pause"}
            </button>
            <button className="btn" onClick={() => setRows([])}>
              Clear
            </button>
          </>
        }
      />
      <ErrorBox error={error} />
      <div className="card table-card">
        <table>
          <thead>
            <tr>
              <th>Time</th>
              <th>Event</th>
              <th>Kind</th>
              <th>Object</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((r) => (
              <tr key={r.id}>
                <td className="muted mono">{r.at}</td>
                <td>
                  <span className={`ev ev-${r.type}`}>{r.type}</span>
                </td>
                <td>{r.kind}</td>
                <td className="mono">{r.name}</td>
                <td className="muted">{r.summary}</td>
              </tr>
            ))}
            {shown.length === 0 && (
              <tr>
                <td colSpan={5} className="empty">
                  {paused ? "Paused." : "Waiting for events… apply something to see it here."}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  );
}
