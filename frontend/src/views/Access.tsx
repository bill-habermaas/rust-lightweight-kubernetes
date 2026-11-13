import { useState, type FormEvent } from "react";
import { adminApi, authApi, errorMessage } from "../api";
import type { Role } from "../gen/lkube";
import { ErrorBox, Modal, Notice, PageHeader, useAsync } from "../ui";

export function Access() {
  const [tab, setTab] = useState<"users" | "roles" | "tokens">("users");
  return (
    <>
      <PageHeader title="Access control" subtitle="Users, RBAC roles and API tokens" />
      <div className="tabs">
        {(["users", "roles", "tokens"] as const).map((t) => (
          <button key={t} className={tab === t ? "tab active" : "tab"} onClick={() => setTab(t)}>
            {t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </div>
      {tab === "users" && <Users />}
      {tab === "roles" && <Roles />}
      {tab === "tokens" && <Tokens />}
    </>
  );
}

function Users() {
  const users = useAsync(async () => (await adminApi.listUsers({}).response).users, []);
  const roles = useAsync(async () => (await adminApi.listRoles({}).response).roles, []);
  const [error, setError] = useState<string>();
  const [form, setForm] = useState({ username: "", password: "", roles: ["viewer"] as string[] });

  const run = async (fn: () => Promise<unknown>) => {
    setError(undefined);
    try {
      await fn();
      users.reload();
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  const create = (e: FormEvent) => {
    e.preventDefault();
    run(async () => {
      await adminApi.createUser(form).response;
      setForm({ username: "", password: "", roles: ["viewer"] });
    });
  };

  const roleNames = (roles.data ?? []).map((r) => r.name);

  return (
    <>
      <ErrorBox error={users.error ?? roles.error ?? error} />
      <form className="card form inline-form" onSubmit={create}>
        <input placeholder="username" value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value })} required />
        <input
          placeholder="password (min 8)"
          type="password"
          value={form.password}
          onChange={(e) => setForm({ ...form, password: e.target.value })}
          required
          minLength={8}
        />
        <select
          multiple
          value={form.roles}
          onChange={(e) => setForm({ ...form, roles: Array.from(e.target.selectedOptions).map((o) => o.value) })}
        >
          {roleNames.map((r) => (
            <option key={r}>{r}</option>
          ))}
        </select>
        <button className="btn btn-primary">Add user</button>
      </form>
      <div className="card table-card">
        <table>
          <thead>
            <tr>
              <th>User</th>
              <th>Roles</th>
              <th>Created</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(users.data ?? []).map((u) => (
              <tr key={u.username}>
                <td>
                  <strong>{u.username}</strong>
                </td>
                <td>
                  <select
                    multiple
                    className="role-select"
                    value={u.roles}
                    onChange={(e) =>
                      run(() =>
                        adminApi.setUserRoles({ username: u.username, roles: Array.from(e.target.selectedOptions).map((o) => o.value) })
                          .response,
                      )
                    }
                  >
                    {roleNames.map((r) => (
                      <option key={r}>{r}</option>
                    ))}
                  </select>
                </td>
                <td className="muted">{u.createdAt ? new Date(u.createdAt).toLocaleString() : ""}</td>
                <td className="actions">
                  <button
                    className="btn btn-sm btn-danger"
                    onClick={() => confirm(`Delete user ${u.username}?`) && run(() => adminApi.deleteUser({ name: u.username }).response)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </>
  );
}

const NEW_ROLE = `{
  "name": "dev-deployer",
  "rules": [
    { "verbs": ["get", "list", "watch", "create", "update", "scale", "logs"],
      "kinds": ["Deployment", "Pod", "Service"],
      "namespaces": ["dev"] }
  ]
}`;

function Roles() {
  const { data, error, reload } = useAsync(async () => (await adminApi.listRoles({}).response).roles, []);
  const [editing, setEditing] = useState<string>();
  const [actionError, setActionError] = useState<string>();

  const save = async () => {
    setActionError(undefined);
    try {
      const parsed = JSON.parse(editing!);
      const role: Role = {
        name: parsed.name,
        builtin: false,
        rules: (parsed.rules ?? []).map((r: { verbs?: string[]; kinds?: string[]; namespaces?: string[] }) => ({
          verbs: r.verbs ?? [],
          kinds: r.kinds ?? [],
          namespaces: r.namespaces ?? [],
        })),
      };
      await adminApi.putRole(role).response;
      setEditing(undefined);
      reload();
    } catch (e) {
      setActionError(errorMessage(e));
    }
  };

  return (
    <>
      <ErrorBox error={error ?? actionError} />
      <div className="row end">
        <button className="btn btn-primary" onClick={() => setEditing(NEW_ROLE)}>
          New role
        </button>
      </div>
      <div className="role-grid">
        {(data ?? []).map((r) => (
          <div key={r.name} className="card role-card">
            <div className="row between">
              <strong>{r.name}</strong>
              {r.builtin ? <span className="chip">built-in</span> : null}
            </div>
            <table className="rules">
              <tbody>
                {r.rules.map((rule, i) => (
                  <tr key={i}>
                    <td className="mono">{rule.verbs.join(", ")}</td>
                    <td className="mono">{rule.kinds.join(", ")}</td>
                    <td className="mono muted">{rule.namespaces.length ? rule.namespaces.join(", ") : "all ns"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {!r.builtin && (
              <div className="row end">
                <button className="btn btn-sm" onClick={() => setEditing(JSON.stringify({ name: r.name, rules: r.rules }, null, 2))}>
                  Edit
                </button>
                <button
                  className="btn btn-sm btn-danger"
                  onClick={async () => {
                    if (!confirm(`Delete role ${r.name}?`)) return;
                    try {
                      await adminApi.deleteRole({ name: r.name }).response;
                      reload();
                    } catch (e) {
                      setActionError(errorMessage(e));
                    }
                  }}
                >
                  Delete
                </button>
              </div>
            )}
          </div>
        ))}
      </div>
      {editing !== undefined && (
        <Modal title="Role" onClose={() => setEditing(undefined)} wide>
          <p className="muted small">
            Verbs: get, list, watch, create, update, delete, scale, logs, status, or *. Kinds: any kind, * (all resource kinds), or
            system:users / system:roles / system:tokens / system:operators / system:*.
          </p>
          <textarea rows={14} className="mono" value={editing} onChange={(e) => setEditing(e.target.value)} />
          <ErrorBox error={actionError} />
          <div className="row end">
            <button className="btn btn-primary" onClick={save}>
              Save role
            </button>
          </div>
        </Modal>
      )}
    </>
  );
}

function Tokens() {
  const me = useAsync(async () => (await authApi.whoAmI({}).response), []);
  const [subject, setSubject] = useState("ci-bot");
  const [roles, setRoles] = useState("viewer");
  const [ttl, setTtl] = useState(86400);
  const [issued, setIssued] = useState<{ token: string; id: string }>();
  const [revokeId, setRevokeId] = useState("");
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();

  const create = async (e: FormEvent) => {
    e.preventDefault();
    setError(undefined);
    try {
      const r = await authApi.createToken({
        subject,
        roles: roles.split(",").map((s) => s.trim()).filter(Boolean),
        ttlSeconds: ttl,
      }).response;
      setIssued({ token: r.token, id: r.tokenId });
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const revoke = async (e: FormEvent) => {
    e.preventDefault();
    setError(undefined);
    try {
      await authApi.revokeToken({ tokenId: revokeId.trim() }).response;
      setNotice(`Token ${revokeId} revoked`);
      setRevokeId("");
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  return (
    <>
      <ErrorBox error={me.error ?? error} />
      <Notice text={notice} />
      {me.data && (
        <div className="card">
          Signed in as <strong>{me.data.subject}</strong> with a <strong>{me.data.tokenKind}</strong> token · roles{" "}
          {me.data.roles.map((r) => (
            <span key={r} className="chip">
              {r}
            </span>
          ))}{" "}
          · expires {new Date(me.data.expiresAt * 1000).toLocaleString()}
        </div>
      )}
      <div className="two-col">
        <form className="card form" onSubmit={create}>
          <h3>Create API token</h3>
          <label>
            Subject
            <input value={subject} onChange={(e) => setSubject(e.target.value)} />
          </label>
          <label>
            Roles (comma separated; must be roles you hold unless admin)
            <input value={roles} onChange={(e) => setRoles(e.target.value)} />
          </label>
          <label>
            TTL (seconds)
            <input type="number" min={60} value={ttl} onChange={(e) => setTtl(Number(e.target.value))} />
          </label>
          <button className="btn btn-primary">Create token</button>
        </form>
        <form className="card form" onSubmit={revoke}>
          <h3>Revoke token</h3>
          <label>
            Token id (jti)
            <input value={revokeId} onChange={(e) => setRevokeId(e.target.value)} required />
          </label>
          <button className="btn btn-danger">Revoke</button>
        </form>
      </div>
      {issued && (
        <Modal title="API token created" onClose={() => setIssued(undefined)}>
          <p>
            Token id <code>{issued.id}</code>. Copy the token now — it is not stored.
          </p>
          <textarea readOnly rows={6} className="mono" value={issued.token} onFocus={(e) => e.target.select()} />
        </Modal>
      )}
    </>
  );
}
