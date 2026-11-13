import { describe, expect, it } from "vitest";
import { age, fromResource, parseManifests, summarize, toResource } from "./manifest";
import type { Resource } from "./gen/lkube";

const res = (kind: string, spec: unknown, status?: unknown): Resource => ({
  apiVersion: "lkube.io/v1",
  kind,
  metadata: {
    name: "x",
    namespace: "default",
    labels: {},
    annotations: {},
    uid: "u",
    resourceVersion: 3,
    generation: 1,
    createdAt: "2026-01-01T00:00:00Z",
    ownerUid: "",
  },
  specJson: JSON.stringify(spec),
  statusJson: status ? JSON.stringify(status) : "",
});

describe("parseManifests", () => {
  it("parses multiple YAML documents", () => {
    const ms = parseManifests("kind: Namespace\nmetadata: {name: a}\n---\nkind: Namespace\nmetadata: {name: b}\n");
    expect(ms.map((m) => m.metadata.name)).toEqual(["a", "b"]);
  });
  it("accepts JSON", () => {
    expect(parseManifests('{"kind":"Pod","metadata":{"name":"p"},"spec":{}}')[0].kind).toBe("Pod");
  });
  it("rejects missing kind or name", () => {
    expect(() => parseManifests("metadata: {name: a}")).toThrow(/kind/);
    expect(() => parseManifests("kind: Pod\nmetadata: {}")).toThrow(/metadata.name/);
    expect(() => parseManifests("")).toThrow(/no manifests/);
  });
});

describe("resource conversion", () => {
  it("round-trips through the gRPC message", () => {
    const [m] = parseManifests("kind: Deployment\nmetadata:\n  name: web\n  labels: {app: web}\nspec:\n  replicas: 2\n");
    const r = toResource(m);
    expect(r.specJson).toBe('{"replicas":2}');
    expect(r.metadata?.namespace).toBe("");
    const back = fromResource({ ...r, apiVersion: "lkube.io/v1" }, false);
    expect(back.spec).toEqual({ replicas: 2 });
    expect(back.metadata.labels).toEqual({ app: "web" });
  });
});

describe("summarize", () => {
  it("summarizes built-in kinds", () => {
    expect(summarize(res("Pod", { nodeName: "n1" }, { phase: "Running", containerStatuses: [{ restartCount: 2 }] }))).toBe(
      "Running · on n1 · 2 restarts",
    );
    expect(summarize(res("Deployment", { replicas: 3 }, { readyReplicas: 1 }))).toBe("1/3 ready");
    expect(summarize(res("Service", {}, { endpoints: [{}, {}] }))).toBe("2 endpoints");
    expect(summarize(res("Node", {}, { ready: false }))).toBe("NotReady");
  });
});

describe("age", () => {
  it("formats durations", () => {
    const t0 = Date.parse("2026-01-01T00:00:00Z");
    expect(age("2026-01-01T00:00:00Z", t0 + 30_000)).toBe("30s");
    expect(age("2026-01-01T00:00:00Z", t0 + 5 * 60_000)).toBe("5m");
    expect(age("2026-01-01T00:00:00Z", t0 + 3 * 3600_000)).toBe("3h");
    expect(age("bad")).toBe("");
  });
});
