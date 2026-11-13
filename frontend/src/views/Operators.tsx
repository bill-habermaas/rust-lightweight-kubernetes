import { useState, type FormEvent } from "react";
import { errorMessage, operatorApi } from "../api";
import { ErrorBox, Modal, PageHeader, useAsync } from "../ui";

const EXAMPLE_SCHEMA = `{
  "required": ["image"],
  "properties": {
    "image": { "type": "string" },
    "replicas": { "type": "integer" }
  }
}`;

export function Operators({ onChanged }: { onChanged: () => void }) {
  const { data, error, reload } = useAsync(async () => (await operatorApi.list({}).response).operators, []);
  const [showForm, setShowForm] = useState(false);
  const [issuedToken, setIssuedToken] = useState<string>();
  const [actionError, setActionError] = useState<string>();

  async function unregister(name: string) {
    if (!confirm(`Unregister ${name}? Its token is revoked. CRDs and their objects are kept.`)) return;
    try {
      await operatorApi.unregister({ name }).response;
      reload();
    } catch (e) {
      setActionError(errorMessage(e));
    }
  }

  return (
    <>
      <PageHeader
        title="Operators"
        subtitle="Built-in controllers and registered operators. Registering creates CRDs and a scoped token for an external operator."
        actions={
          <button className="btn btn-primary" onClick={() => setShowForm(true)}>
            Register operator
          </button>
        }
      />
      <ErrorBox error={error ?? actionError} />
      <div className="card table-card">
        <table>
          <thead>
            <tr>
              <th>Name</th>
              <th>Type</th>
              <th>Kinds</th>
              <th>Role</th>
              <th>Registered</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(data ?? []).map((o) => (
              <tr key={o.name}>
                <td>
                  <strong>{o.name}</strong>
                </td>
                <td>{o.builtin ? <span className="chip">built-in</span> : <span className="chip chip-accent">external</span>}</td>
                <td>{o.kinds.join(", ")}</td>
                <td className="mono">{o.role}</td>
                <td className="muted">{o.registeredAt ? new Date(o.registeredAt).toLocaleString() : "—"}</td>
                <td className="actions">
                  {!o.builtin && (
                    <button className="btn btn-sm btn-danger" onClick={() => unregister(o.name)}>
                      Unregister
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {showForm && (
        <RegisterForm
          onClose={() => setShowForm(false)}
          onDone={(token) => {
            setShowForm(false);
            setIssuedToken(token);
            reload();
            onChanged();
          }}
        />
      )}
      {issuedToken && (
        <Modal title="Operator token" onClose={() => setIssuedToken(undefined)}>
          <p>Store this token now — it is shown only once. Pass it to your operator as <code>LKUBE_OPERATOR_TOKEN</code>.</p>
          <textarea readOnly rows={6} className="mono" value={issuedToken} onFocus={(e) => e.target.select()} />
        </Modal>
      )}
    </>
  );
}

function RegisterForm({ onClose, onDone }: { onClose: () => void; onDone: (token: string) => void }) {
  const [name, setName] = useState("webapp-operator");
  const [kind, setKind] = useState("WebApp");
  const [apiVersion, setApiVersion] = useState("example.com/v1");
  const [namespaced, setNamespaced] = useState(true);
  const [schema, setSchema] = useState(EXAMPLE_SCHEMA);
  const [extraKinds, setExtraKinds] = useState("Deployment, Service");
  const [error, setError] = useState<string>();

  async function submit(e: FormEvent) {
    e.preventDefault();
    try {
      if (schema.trim()) JSON.parse(schema);
      const kinds = extraKinds.split(",").map((k) => k.trim()).filter(Boolean);
      const response = await operatorApi.register({
        name,
        crds: kind ? [{ kind, namespaced, apiVersion, schemaJson: schema.trim() }] : [],
        extraRules: kinds.length ? [{ verbs: ["get", "list", "watch", "create", "update"], kinds, namespaces: [] }] : [],
        tokenTtlSeconds: 0,
      }).response;
      onDone(response.token);
    } catch (err) {
      setError(errorMessage(err));
    }
  }

  return (
    <Modal title="Register operator" onClose={onClose} wide>
      <form className="form" onSubmit={submit}>
        <label>
          Operator name
          <input value={name} onChange={(e) => setName(e.target.value)} required />
        </label>
        <div className="grid-3">
          <label>
            Custom kind
            <input value={kind} onChange={(e) => setKind(e.target.value)} placeholder="WebApp" />
          </label>
          <label>
            API version
            <input value={apiVersion} onChange={(e) => setApiVersion(e.target.value)} />
          </label>
          <label className="check">
            <input type="checkbox" checked={namespaced} onChange={(e) => setNamespaced(e.target.checked)} /> Namespaced
          </label>
        </div>
        <label>
          Schema (JSON, optional)
          <textarea rows={8} className="mono" value={schema} onChange={(e) => setSchema(e.target.value)} />
        </label>
        <label>
          Additional kinds the operator may manage (get/list/watch/create/update)
          <input value={extraKinds} onChange={(e) => setExtraKinds(e.target.value)} />
        </label>
        <ErrorBox error={error} />
        <div className="row end">
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary">Register</button>
        </div>
      </form>
    </Modal>
  );
}
