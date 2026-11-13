import { useState } from "react";
import { clusterApi, errorMessage } from "../api";
import { parseManifests, TEMPLATES, toResource } from "../manifest";
import { PageHeader } from "../ui";

interface Result {
  ok: boolean;
  text: string;
}

export function ApplyView({ text, setText, onApplied }: { text: string; setText: (t: string) => void; onApplied: () => void }) {
  const [results, setResults] = useState<Result[]>([]);
  const [busy, setBusy] = useState(false);

  async function apply() {
    setBusy(true);
    const out: Result[] = [];
    try {
      for (const m of parseManifests(text)) {
        const label = `${m.kind}/${m.metadata.namespace ? m.metadata.namespace + "/" : ""}${m.metadata.name}`;
        try {
          const { response } = await clusterApi.apply({ resource: toResource(m) });
          out.push({ ok: true, text: `${label} applied (generation ${response.metadata?.generation})` });
        } catch (e) {
          out.push({ ok: false, text: `${label}: ${errorMessage(e)}` });
        }
      }
      onApplied();
    } catch (e) {
      out.push({ ok: false, text: errorMessage(e) });
    } finally {
      setResults(out);
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader
        title="Apply manifests"
        subtitle="YAML or JSON; separate multiple objects with ---. Existing objects are updated (like kubectl apply)."
        actions={
          <>
            <select value="" onChange={(e) => e.target.value && setText(TEMPLATES[e.target.value])} aria-label="Insert template">
              <option value="">Insert template…</option>
              {Object.keys(TEMPLATES).map((k) => (
                <option key={k}>{k}</option>
              ))}
            </select>
            <button className="btn btn-primary" onClick={apply} disabled={busy || !text.trim()}>
              {busy ? "Applying…" : "Apply"}
            </button>
          </>
        }
      />
      <textarea className="editor" spellCheck={false} value={text} onChange={(e) => setText(e.target.value)} />
      {results.length > 0 && (
        <ul className="results">
          {results.map((r, i) => (
            <li key={i} className={r.ok ? "ok" : "bad"}>
              {r.ok ? "✓" : "✗"} {r.text}
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
