// gRPC-Web clients. Every call carries "authorization: Bearer <token>".
import { GrpcWebFetchTransport } from "@protobuf-ts/grpcweb-transport";
import type { RpcInterceptor } from "@protobuf-ts/runtime-rpc";
import { RpcError } from "@protobuf-ts/runtime-rpc";
import {
  AdminServiceClient,
  AuthServiceClient,
  ClusterServiceClient,
  OperatorServiceClient,
} from "./gen/lkube.client";

const TOKEN_KEY = "lkube.token";

function readToken(): string | null {
  try {
    return sessionStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

let token: string | null = readToken();
let onUnauthenticated: () => void = () => {};

export function setToken(t: string | null) {
  token = t;
  try {
    if (t) sessionStorage.setItem(TOKEN_KEY, t);
    else sessionStorage.removeItem(TOKEN_KEY);
  } catch {
    /* storage unavailable: keep in memory only */
  }
}

export function getToken() {
  return token;
}

export function onSessionExpired(cb: () => void) {
  onUnauthenticated = cb;
}

const withAuth = <T extends { meta?: Record<string, string | string[]> }>(options: T): T => {
  if (token) options.meta = { ...(options.meta ?? {}), authorization: `Bearer ${token}` };
  return options;
};

const authInterceptor: RpcInterceptor = {
  interceptUnary(next, method, input, options) {
    const call = next(method, input, withAuth(options));
    call.response.catch((e) => {
      if (e instanceof RpcError && e.code === "UNAUTHENTICATED" && method.name !== "Login") onUnauthenticated();
    });
    return call;
  },
  interceptServerStreaming(next, method, input, options) {
    return next(method, input, withAuth(options));
  },
};

const baseUrl = (import.meta.env.VITE_API_URL as string | undefined) ?? window.location.origin;
const transport = new GrpcWebFetchTransport({ baseUrl, format: "binary", interceptors: [authInterceptor] });

export const authApi = new AuthServiceClient(transport);
export const adminApi = new AdminServiceClient(transport);
export const clusterApi = new ClusterServiceClient(transport);
export const operatorApi = new OperatorServiceClient(transport);

export function errorMessage(e: unknown): string {
  if (e instanceof RpcError) return `${e.code}: ${decodeURIComponent(e.message)}`;
  if (e instanceof Error) return e.message;
  return String(e);
}
