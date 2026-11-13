// Converting between YAML/JSON manifests (what users type) and gRPC Resource messages.
import YAML from "yaml";
import type { Resource } from "./gen/lkube";

export interface Manifest {
  apiVersion?: string;
  kind: string;
  metadata: {
    name: string;
    namespace?: string;
    labels?: Record<string, string>;
    annotations?: Record<string, string>;
    uid?: string;
    resourceVersion?: number;
    generation?: number;
    createdAt?: string;
    ownerUid?: string;
  };
  spec?: unknown;
  status?: unknown;
}

/** Parse one or more YAML/JSON documents separated by `---`. */
export function parseManifests(text: string): Manifest[] {
  const docs = YAML.parseAllDocuments(text);
  const out: Manifest[] = [];
  docs.forEach((doc, i) => {
    if (doc.errors.length) throw new Error(`document ${i + 1}: ${doc.errors[0].message}`);
    const value = doc.toJS();
    if (value == null) return;
    if (typeof value !== "object" || Array.isArray(value)) throw new Error(`document ${i + 1}: expected an object`);
    if (!value.kind) throw new Error(`document ${i + 1}: 'kind' is required`);
    if (!value.metadata?.name) throw new Error(`document ${i + 1}: 'metadata.name' is required`);
    out.push(value as Manifest);
  });
  if (!out.length) throw new Error("no manifests found");
  return out;
}

export function toResource(m: Manifest): Resource {
  return {
    apiVersion: m.apiVersion ?? "",
    kind: m.kind,
    metadata: {
      name: m.metadata.name,
      namespace: m.metadata.namespace ?? "",
      labels: m.metadata.labels ?? {},
      annotations: m.metadata.annotations ?? {},
      uid: "",
      resourceVersion: 0,
      generation: 0,
      createdAt: "",
      ownerUid: m.metadata.ownerUid ?? "",
    },
    specJson: m.spec === undefined ? "" : JSON.stringify(m.spec),
    statusJson: "",
  };
}

const parse = (s: string): unknown => (s ? JSON.parse(s) : undefined);

export function fromResource(r: Resource, includeStatus = true): Manifest {
  const md = r.metadata!;
  const metadata: Manifest["metadata"] = { name: md.name };
  if (md.namespace) metadata.namespace = md.namespace;
  if (Object.keys(md.labels).length) metadata.labels = md.labels;
  if (Object.keys(md.annotations).length) metadata.annotations = md.annotations;
  if (includeStatus) {
    metadata.uid = md.uid;
    metadata.resourceVersion = md.resourceVersion;
    metadata.generation = md.generation;
    metadata.createdAt = md.createdAt;
    if (md.ownerUid) metadata.ownerUid = md.ownerUid;
  }
  const m: Manifest = { apiVersion: r.apiVersion, kind: r.kind, metadata, spec: parse(r.specJson) };
  if (includeStatus && r.statusJson) m.status = parse(r.statusJson);
  return m;
}

export const toYaml = (m: Manifest) => YAML.stringify(m, { indent: 2 });

/** One-line status summary per kind, for tables. */
export function summarize(r: Resource): string {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const s: any = r.statusJson ? JSON.parse(r.statusJson) : {};
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const spec: any = r.specJson ? JSON.parse(r.specJson) : {};
  switch (r.kind) {
    case "Pod": {
      const restarts = (s.containerStatuses ?? []).reduce((n: number, c: { restartCount?: number }) => n + (c.restartCount ?? 0), 0);
      return [s.phase ?? "Pending", spec.nodeName ? `on ${spec.nodeName}` : "unscheduled", restarts ? `${restarts} restarts` : ""]
        .filter(Boolean)
        .join(" · ");
    }
    case "Deployment":
      return `${s.readyReplicas ?? 0}/${spec.replicas ?? 0} ready`;
    case "Service":
      return `${(s.endpoints ?? []).length} endpoints`;
    case "Node":
      return s.ready ? "Ready" : "NotReady";
    case "CustomResourceDefinition":
      return `${spec.kind}${spec.operator ? ` (by ${spec.operator})` : ""}`;
    default:
      return s.phase ?? (r.statusJson ? "has status" : "");
  }
}

export function statusTone(r: Resource): "ok" | "warn" | "bad" | "none" {
  const text = summarize(r);
  if (r.kind === "Deployment") {
    const [ready, want] = text.split(" ")[0].split("/").map(Number);
    return ready === want ? "ok" : "warn";
  }
  if (/NotReady|Failed/.test(text)) return "bad";
  if (/Running|Ready|Succeeded/.test(text)) return "ok";
  if (/Pending|Progressing/.test(text)) return "warn";
  return "none";
}

export function age(createdAt: string, now = Date.now()): string {
  const t = Date.parse(createdAt);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.floor((now - t) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

export const TEMPLATES: Record<string, string> = {
  Deployment: `kind: Deployment
metadata:
  name: web
  namespace: default
spec:
  replicas: 2
  template:
    labels:
      app: web
    spec:
      containers:
        - name: nginx
          image: nginx:1.27
          ports:
            - containerPort: 80
---
kind: Service
metadata:
  name: web
  namespace: default
spec:
  selector:
    app: web
  ports:
    - port: 80
`,
  Pod: `kind: Pod
metadata:
  name: hello
  namespace: default
spec:
  restartPolicy: Never
  containers:
    - name: hello
      image: busybox:1.36
      command: ["sh", "-c"]
      args: ["echo hello from lkube; sleep 5"]
`,
  ConfigMap: `kind: ConfigMap
metadata:
  name: settings
  namespace: default
spec:
  data:
    LOG_LEVEL: info
`,
  Namespace: `kind: Namespace
metadata:
  name: dev
`,
  CustomResourceDefinition: `kind: CustomResourceDefinition
metadata:
  name: webapp
spec:
  kind: WebApp
  namespaced: true
  apiVersion: example.com/v1
  schema:
    required: [image]
    properties:
      image: { type: string }
      replicas: { type: integer }
      port: { type: integer }
`,
  WebApp: `kind: WebApp
metadata:
  name: hello
  namespace: default
spec:
  image: nginx:1.27
  replicas: 2
  port: 80
`,
};
