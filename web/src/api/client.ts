import type { components } from "./schema";

// Types are generated from contract/openapi.yaml. Never hand-edit schema.d.ts.
export type StatusResponse = components["schemas"]["StatusResponse"];
export type ServerInfo = components["schemas"]["ServerInfo"];
export type DaemonRecord = components["schemas"]["DaemonRecord"];
export type TelemetryRecord = components["schemas"]["TelemetryRecord"];
export type ModelConfig = components["schemas"]["ModelConfig"];
export type ConfigUpdate = components["schemas"]["ConfigUpdate"];
export type ErrorResponse = components["schemas"]["ErrorResponse"];

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

async function requestJson<T>(input: RequestInfo, init?: RequestInit): Promise<T> {
  const response = await fetch(input, {
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

/** `GET /api/status` — snapshot of the control plane and its daemons. */
export function fetchStatus(signal?: AbortSignal): Promise<StatusResponse> {
  return requestJson<StatusResponse>("/api/status", { signal });
}

/** `POST /api/config` — replace and broadcast the model configuration. */
export function pushConfig(config: ConfigUpdate, signal?: AbortSignal): Promise<ConfigUpdate> {
  return requestJson<ConfigUpdate>("/api/config", {
    method: "POST",
    body: JSON.stringify(config),
    signal,
  });
}
