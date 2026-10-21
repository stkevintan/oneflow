import type { components } from "./schema";

// Types are generated from contract/openapi.yaml. Never hand-edit schema.d.ts.
export type HealthResponse = components["schemas"]["HealthResponse"];
export type StatusResponse = components["schemas"]["StatusResponse"];
export type ModelConfig = components["schemas"]["ModelConfig"];
export type ModelConfigSet = components["schemas"]["ModelConfigSet"];
export type Daemon = components["schemas"]["Daemon"];
export type DaemonList = components["schemas"]["DaemonList"];
export type UsageEvent = components["schemas"]["UsageEvent"];
export type UsageEventPage = components["schemas"]["UsageEventPage"];
export type ErrorResponse = components["schemas"]["ErrorResponse"];

export interface UsageEventQuery {
  limit?: number;
  cursor?: string;
  daemon_id?: string;
  model_name?: string;
}

/** Control-plane origin; empty means same origin (dev/preview proxy). */
const BASE_URL = (import.meta.env.VITE_CONTROL_PLANE_URL ?? "").replace(/\/+$/, "");

export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

async function parseErrorMessage(response: Response): Promise<string> {
  try {
    const body = (await response.json()) as Partial<ErrorResponse>;
    if (typeof body.error === "string" && body.error.length > 0) {
      return body.error;
    }
  } catch {
    // Fall through to the generic message below.
  }
  return `request failed with HTTP ${response.status}`;
}

async function requestJson<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${BASE_URL}${path}`, {
    ...init,
    headers: {
      accept: "application/json",
      ...(init?.body ? { "content-type": "application/json" } : {}),
      ...init?.headers,
    },
  });

  if (!response.ok) {
    throw new ApiError(await parseErrorMessage(response), response.status);
  }

  return (await response.json()) as T;
}

/** `GET /v1/health` — unauthenticated liveness probe. */
export function getHealth(signal?: AbortSignal): Promise<HealthResponse> {
  return requestJson<HealthResponse>("/v1/health", { signal });
}

/** `GET /v1/status` — server summary. */
export function getStatus(signal?: AbortSignal): Promise<StatusResponse> {
  return requestJson<StatusResponse>("/v1/status", { signal });
}

/** `GET /v1/config` — the desired model configuration. */
export function getConfig(signal?: AbortSignal): Promise<ModelConfigSet> {
  return requestJson<ModelConfigSet>("/v1/config", { signal });
}

/** `PUT /v1/config` — replace the configuration and push it to every daemon. */
export function putConfig(config: ModelConfigSet, signal?: AbortSignal): Promise<ModelConfigSet> {
  return requestJson<ModelConfigSet>("/v1/config", {
    method: "PUT",
    body: JSON.stringify(config),
    signal,
  });
}

/** `GET /v1/daemons` — connected daemons. */
export function listDaemons(signal?: AbortSignal): Promise<DaemonList> {
  return requestJson<DaemonList>("/v1/daemons", { signal });
}

/** `GET /v1/daemons/{connection_id}` — one daemon connection. */
export function getDaemon(connectionId: string, signal?: AbortSignal): Promise<Daemon> {
  return requestJson<Daemon>(`/v1/daemons/${encodeURIComponent(connectionId)}`, { signal });
}

/** `GET /v1/usage/events` — newest-first, cursor-paginated usage events. */
export function listUsageEvents(
  query: UsageEventQuery = {},
  signal?: AbortSignal,
): Promise<UsageEventPage> {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== "") {
      params.set(key, String(value));
    }
  }
  const search = params.toString();
  return requestJson<UsageEventPage>(`/v1/usage/events${search ? `?${search}` : ""}`, {
    signal,
  });
}
