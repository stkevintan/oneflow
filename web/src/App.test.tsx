import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import { setAdminToken } from "./api/client";
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
      auth_policy_revision: null,
      auth_policy_valid_until: null,
    },
  ],
};

const USAGE: UsageEventPage = {
  items: [
    {
      request_id: "req-2",
      virtual_key_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
      virtual_key_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
      api_key_configured: true,
      parameters: { upstream_model: "demo-upstream" },
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
    if (path === "/v1/daemons/credentials") return jsonResponse({ items: [{
      daemon_id: "daemon-poc-001", created_at: 1_700_000_000, revoked_at: null, rotated_at: null,
    }] });
    if (path === "/v1/daemons") return jsonResponse(DAEMONS);
    if (path === "/v1/usage/events") return jsonResponse(USAGE);
    if (path === "/v1/config" && init?.method === "PUT") {
      return jsonResponse(JSON.parse(String(init.body)));
    }
    if (path === "/v1/config") return jsonResponse(CONFIG);
    if (path === "/key/list") return jsonResponse({
      keys: [], total_count: 0, current_page: 1, total_pages: 0,
    });
    if (path === "/key/generate") return jsonResponse({
      key: "sk-one-time-consumer", token: "a".repeat(64), token_id: "b".repeat(32),
      daemon_id: "daemon-poc-001", key_alias: "external-app", models: ["demo-model"],
      created_at: "2026-10-22T09:00:00Z", expires: null, blocked: false,
    });
    return jsonResponse({ error: `no API route matches ${path}` }, 404);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("dashboard", () => {
  afterEach(() => {
    setAdminToken("");
    cleanup();
    window.history.replaceState(null, "", "/");
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

  it("round-trips a redacted configuration without sending an api_key", async () => {
    const fetchMock = stubApi();
    render(<App />);

    const editor = (await screen.findByLabelText("Model configuration JSON")) as HTMLTextAreaElement;
    await waitFor(() => expect(editor.value).not.toBe(""));
    expect(editor.value).toContain('"api_key_configured": true');
    expect(editor.value).not.toContain('"api_key":');
    expect(screen.getByText(/values are write-only and never shown here/)).toBeTruthy();

    const edited = { ...JSON.parse(editor.value), version: "smoke-v2" };
    fireEvent.change(editor, { target: { value: JSON.stringify(edited) } });
    fireEvent.click(screen.getByRole("button", { name: "Push configuration" }));

    await screen.findByText("Configuration pushed to all connected daemons.");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    const sent = JSON.parse(String(put?.[1]?.body)) as ModelConfigSet;
    // Omitting api_key is what tells the control plane to keep the stored one.
    expect(sent.models[0]?.parameters).toEqual({ upstream_model: "demo-upstream" });
  });

  it("sends an administrator token without persisting it in the form", async () => {
    const fetchMock = stubApi();
    render(<App />);
    const input = screen.getByLabelText("Administrator token") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "test-admin-token" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply token" }));
    await waitFor(() => {
      expect(fetchMock.mock.calls.some(([, init]) =>
        (init?.headers as Record<string, string> | undefined)?.authorization === "Bearer test-admin-token"
      )).toBe(true);
    });
    expect(input.value).toBe("");
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

  it("opens daemon details from a daemon link and creates a consumer key using the typed API", async () => {
    const fetchMock = stubApi();
    render(<App />);
    const links = await screen.findAllByRole("link", { name: "daemon-poc-001" });
    expect(links.length).toBe(2);
    fireEvent.click(links[0]!);
    await screen.findByRole("heading", { name: "Daemon: daemon-poc-001" });
    await screen.findByText("No consumer keys for this daemon.");
    expect(fetchMock.mock.calls.some(([url]) =>
      url === "/key/list?daemon_id=daemon-poc-001&page=1&size=10&return_full_object=true"
    )).toBe(true);
    fireEvent.change(screen.getByLabelText("Consumer name"), { target: { value: "external-app" } });
    fireEvent.click(screen.getByRole("button", { name: "Create virtual key" }));
    expect((await screen.findByLabelText("One-time consumer key") as HTMLInputElement).value).toBe("sk-one-time-consumer");
    const generation = fetchMock.mock.calls.find(([url]) => url === "/key/generate");
    expect(generation?.[1]?.method).toBe("POST");
    expect(generation?.[1]?.cache).toBe("no-store");
    expect(JSON.parse(String(generation?.[1]?.body))).toEqual({
      daemon_id: "daemon-poc-001", key_alias: "external-app", models: ["demo-model"], duration: null,
    });
    fireEvent.click(screen.getByRole("link", { name: "Back to dashboard" }));
    await screen.findByRole("heading", { name: /Connected daemons/ });
    expect(screen.queryByLabelText("One-time consumer key")).toBeNull();
  });

  it("opens a bookmarked daemon detail page", async () => {
    stubApi();
    window.history.replaceState(null, "", "/#daemon=daemon-poc-001");
    render(<App />);
    await screen.findByRole("heading", { name: "Consumer virtual keys" });
    expect(screen.queryByRole("heading", { name: /Connected daemons/ })).toBeNull();
  });
});
