import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import type { StatusResponse } from "./api/client";

const STATUS: StatusResponse = {
  server: {
    version: "0.1.0",
    uptime_seconds: 42,
    connected_daemons: 1,
    config_version: "smoke-v1",
  },
  daemons: [
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
  telemetry: [
    {
      daemon_id: "daemon-poc-001",
      model_name: "demo-model",
      prompt_tokens: 12,
      completion_tokens: 34,
      success: true,
      received_at: 1_700_000_011,
      request_id: "req-1",
      latency_ms: 412,
      status_code: 200,
      streamed: false,
      usage_reported: true,
    },
    {
      daemon_id: "daemon-poc-001",
      model_name: "demo-model",
      prompt_tokens: 0,
      completion_tokens: 0,
      success: false,
      received_at: 1_700_000_012,
      request_id: "req-2",
      latency_ms: 1500,
      status_code: null,
      streamed: true,
      usage_reported: false,
    },
  ],
  config: {
    version: "smoke-v1",
    models: [
      {
        model_name: "demo-model",
        provider: "custom_openai",
        api_base: "https://example.test/v1",
        parameters: {},
      },
    ],
  },
};

function stubFetch(payload: unknown, status = 200) {
  const fetchMock = vi.fn(async () => ({
    ok: status >= 200 && status < 300,
    status,
    json: async () => payload,
  }));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("dashboard", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("renders daemons, telemetry and the configuration from /api/status", async () => {
    const fetchMock = stubFetch(STATUS);

    render(<App />);

    // The daemon id is expected in both the daemon table and the telemetry row.
    const daemonIds = await screen.findAllByText("daemon-poc-001");
    expect(daemonIds.length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("cluster-ssh198")).toBeTruthy();
    expect(screen.getAllByText("demo-model").length).toBeGreaterThan(0);
    // Config editor is seeded with the contract-shaped configuration payload.
    await waitFor(() => {
      const editor = screen.getByLabelText("Model configuration JSON") as HTMLTextAreaElement;
      expect(JSON.parse(editor.value).version).toBe("smoke-v1");
    });
    expect(fetchMock).toHaveBeenCalledWith("/api/status", expect.anything());
    // usage_reported=false must read as unknown, not as zero tokens.
    expect(screen.getAllByText("—").length).toBe(2);
    expect(screen.getByText("failed")).toBeTruthy();
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
