import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { DaemonList, ModelConfigSet, StatusResponse, UsageEventPage } from "./api/client";

const STATUS: StatusResponse = {
  version: "0.1.0",
  uptime_seconds: 42,
  connected_daemons: 1,
  config_version: "smoke-v1",
};

const DAEMONS: DaemonList = {
  items: [
    {
      connection_id: "conn-1",
      daemon_id: "daemon-poc-001",
      cluster_name: "cluster-ssh198",
      version: "0.1.0",
      hostname: "ssh198.example.com",
      capabilities: ["openai-compatible"],
      connected_at: 1_700_000_000,
      last_seen: 1_700_000_010,
      active_requests: 2,
      loaded_models: ["demo-model"],
      total_requests: 7,
      failed_requests: 1,
      total_prompt_tokens: 120,
      total_completion_tokens: 240,
    },
  ],
};

const USAGE: UsageEventPage = {
  items: [
    {
      request_id: "req-2",
      daemon_id: "daemon-poc-001",
      model_name: "demo-model",
      prompt_tokens: 0,
      completion_tokens: 0,
      usage_reported: false,
      success: false,
      status_code: null,
      latency_ms: 1500,
      streamed: true,
      received_at: 1_700_000_012,
    },
    {
      request_id: "req-1",
      daemon_id: "daemon-poc-001",
      model_name: "demo-model",
      prompt_tokens: 12,
      completion_tokens: 34,
      usage_reported: true,
      success: true,
      status_code: 200,
      latency_ms: 412,
      streamed: false,
      received_at: 1_700_000_011,
    },
  ],
  next_cursor: null,
};

const CONFIG: ModelConfigSet = {
  version: "smoke-v1",
  models: [
    {
      model_name: "demo-model",
      provider: "custom_openai",
      api_base: "https://example.test/v1",
      parameters: {},
    },
  ],
};

function jsonResponse(payload: unknown, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => payload,
  };
}

/** Routes stubbed `fetch` calls by path, the way the control plane would. */
function stubApi() {
  const fetchMock = vi.fn(async (input: string, init?: RequestInit) => {
    const path = input.split("?")[0];
    if (path === "/v1/status") return jsonResponse(STATUS);
    if (path === "/v1/daemons") return jsonResponse(DAEMONS);
    if (path === "/v1/usage/events") return jsonResponse(USAGE);
    if (path === "/v1/config" && init?.method === "PUT") {
      return jsonResponse(JSON.parse(String(init.body)));
    }
    if (path === "/v1/config") return jsonResponse(CONFIG);
    return jsonResponse({ error: `no API route matches ${path}` }, 404);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("dashboard", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("renders daemons, usage and the configuration from the /v1 API", async () => {
    const fetchMock = stubApi();

    render(<App />);

    // The daemon id is expected in both the daemon table and the usage rows.
    const daemonIds = await screen.findAllByText("daemon-poc-001");
    expect(daemonIds.length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("cluster-ssh198")).toBeTruthy();
    expect(screen.getAllByText("demo-model").length).toBeGreaterThan(0);
    await waitFor(() => {
      const editor = screen.getByLabelText("Model configuration JSON") as HTMLTextAreaElement;
      expect(JSON.parse(editor.value).version).toBe("smoke-v1");
    });

    const urls = fetchMock.mock.calls.map(([url]) => url);
    expect(urls).toContain("/v1/status");
    expect(urls).toContain("/v1/daemons");
    expect(urls).toContain("/v1/usage/events?limit=25");
    expect(urls).toContain("/v1/config");
    // usage_reported=false must read as unknown, not as zero tokens.
    expect(screen.getAllByText("—").length).toBe(2);
    expect(screen.getByText("failed")).toBeTruthy();
  });

  it("pushes the edited configuration with PUT /v1/config", async () => {
    const fetchMock = stubApi();

    render(<App />);

    const editor = (await screen.findByLabelText("Model configuration JSON")) as HTMLTextAreaElement;
    await waitFor(() => expect(editor.value).not.toBe(""));
    const edited = { ...CONFIG, version: "smoke-v2" };
    fireEvent.change(editor, { target: { value: JSON.stringify(edited) } });
    fireEvent.click(screen.getByRole("button", { name: "Push configuration" }));

    await screen.findByText("Configuration pushed to all connected daemons.");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(put?.[0]).toBe("/v1/config");
    expect(JSON.parse(String(put?.[1]?.body)).version).toBe("smoke-v2");
  });

  it("shows an offline banner when the control plane is unreachable", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("connection refused");
      }),
    );

    render(<App />);

    expect(await screen.findByRole("alert")).toHaveProperty("textContent");
    expect(screen.getByText(/Cannot reach the control plane/)).toBeTruthy();
  });
});
