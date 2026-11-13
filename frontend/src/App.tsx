import { useCallback, useEffect, useState } from "react";
import { authApi, clusterApi, getToken, onSessionExpired, setToken } from "./api";
import type { KindInfo, WhoAmIResponse } from "./gen/lkube";
import { TEMPLATES } from "./manifest";
import { Access } from "./views/Access";
import { ApplyView } from "./views/ApplyView";
import { Events } from "./views/Events";
import { Login } from "./views/Login";
import { Operators } from "./views/Operators";
import { Overview } from "./views/Overview";
import { Resources } from "./views/Resources";

type View = { page: "overview" | "apply" | "events" | "operators" | "access" } | { page: "kind"; kind: string };

const GROUPS: [string, string[]][] = [
  ["Workloads", ["Deployment", "Pod"]],
  ["Network", ["Service"]],
  ["Config", ["ConfigMap"]],
  ["Cluster", ["Namespace", "Node", "CustomResourceDefinition"]],
];

export default function App() {
  const [me, setMe] = useState<WhoAmIResponse>();
  const [authed, setAuthed] = useState(Boolean(getToken()));
  const [view, setView] = useState<View>({ page: "overview" });
  const [kinds, setKinds] = useState<KindInfo[]>([]);
  const [namespaces, setNamespaces] = useState<string[]>([]);
  const [editorText, setEditorText] = useState(TEMPLATES.Deployment);

  const logout = useCallback(async () => {
    try {
      await authApi.revokeToken({ tokenId: "" }).response;
    } catch {
      /* token may already be invalid */
    }
    setToken(null);
    setAuthed(false);
    setMe(undefined);
  }, []);

  useEffect(() => {
    onSessionExpired(() => {
      setToken(null);
      setAuthed(false);
    });
  }, []);

  const refreshMeta = useCallback(async () => {
    try {
      const [who, k] = await Promise.all([authApi.whoAmI({}).response, clusterApi.listKinds({}).response]);
      setMe(who);
      setKinds(k.kinds);
      try {
        const ns = await clusterApi.list({ kind: "Namespace", namespace: "", labelSelector: {} }).response;
        setNamespaces(ns.items.map((n) => n.metadata!.name));
      } catch {
        setNamespaces(["default"]);
      }
    } catch {
      /* handled by onSessionExpired */
    }
  }, []);

  useEffect(() => {
    if (authed) refreshMeta();
  }, [authed, refreshMeta]);

  if (!authed) return <Login onLogin={() => setAuthed(true)} />;

  const isAdmin = me?.roles.includes("admin");
  const customKinds = kinds.filter((k) => !k.builtin);
  const kindInfo = view.page === "kind" ? kinds.find((k) => k.kind === view.kind) : undefined;
  const nav = (v: View) => () => setView(v);
  const active = (v: View) =>
    v.page === view.page && (v.page !== "kind" || (view.page === "kind" && v.kind === view.kind)) ? "nav-item active" : "nav-item";

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="logo" />
          <span>lkube</span>
        </div>
        <nav>
          <button className={active({ page: "overview" })} onClick={nav({ page: "overview" })}>
            Overview
          </button>
          {GROUPS.map(([group, ks]) => (
            <div key={group} className="nav-group">
              <div className="nav-title">{group}</div>
              {ks.map((k) => (
                <button key={k} className={active({ page: "kind", kind: k })} onClick={nav({ page: "kind", kind: k })}>
                  {k === "CustomResourceDefinition" ? "CRDs" : `${k}s`}
                </button>
              ))}
            </div>
          ))}
          {customKinds.length > 0 && (
            <div className="nav-group">
              <div className="nav-title">Custom resources</div>
              {customKinds.map((k) => (
                <button key={k.kind} className={active({ page: "kind", kind: k.kind })} onClick={nav({ page: "kind", kind: k.kind })}>
                  {k.kind}
                </button>
              ))}
            </div>
          )}
          <div className="nav-group">
            <div className="nav-title">Tools</div>
            <button className={active({ page: "apply" })} onClick={nav({ page: "apply" })}>
              Apply manifest
            </button>
            <button className={active({ page: "events" })} onClick={nav({ page: "events" })}>
              Events
            </button>
            <button className={active({ page: "operators" })} onClick={nav({ page: "operators" })}>
              Operators
            </button>
            {isAdmin && (
              <button className={active({ page: "access" })} onClick={nav({ page: "access" })}>
                Access control
              </button>
            )}
          </div>
        </nav>
        <div className="me">
          <div>
            <strong>{me?.subject ?? "…"}</strong>
            <div className="muted small">{me?.roles.join(", ")}</div>
          </div>
          <button className="btn btn-sm" onClick={logout}>
            Sign out
          </button>
        </div>
      </aside>
      <main className="content">
        {view.page === "overview" && <Overview onOpenKind={(k) => setView({ page: "kind", kind: k })} />}
        {view.page === "kind" && kindInfo && (
          <Resources
            key={kindInfo.kind}
            kind={kindInfo}
            namespaces={namespaces}
            onEdit={(yaml) => {
              setEditorText(yaml);
              setView({ page: "apply" });
            }}
          />
        )}
        {view.page === "apply" && <ApplyView text={editorText} setText={setEditorText} onApplied={refreshMeta} />}
        {view.page === "events" && <Events />}
        {view.page === "operators" && <Operators onChanged={refreshMeta} />}
        {view.page === "access" && <Access />}
      </main>
    </div>
  );
}
