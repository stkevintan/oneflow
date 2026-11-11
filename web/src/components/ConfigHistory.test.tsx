import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ConfigHistoryEntry, ConfigHistoryPage, ModelConfigSet } from "../api/client";
import { ConfigHistory, describeChanges, modelsWithoutCredential } from "./ConfigHistory";

const model = (name: string, parameters: Record<string, string> = {}, keyed = false) => ({
  model_name: name,
  provider: "custom_openai",
  api_base: "https://example.test/v1",
  api_key_configured: keyed,
  parameters,
});

const CURRENT: ModelConfigSet = {
  version: "prod-v3",
  models: [model("a", { api_key_env: "A_KEY" }), model("b", { upstream_model: "b2" }, true)],
};

const entry = (id: number, version: string, models: ConfigHistoryEntry["config"]["models"]) => ({
  id,
  version,
  created_at: 1_700_000_000 + id,
  source: id === 1 ? "initial" : "put",
  rolled_back_from: null,
  config: { version, models },
});

const PAGE_1: ConfigHistoryPage = {
  items: [
    entry(3, "prod-v3", CURRENT.models),
    entry(2, "prod-v2", [model("a", { api_key_env: "A_KEY" }), model("old")]),
  ],
  next_cursor: "2",
};
const PAGE_2: ConfigHistoryPage = {
  items: [entry(1, "prod-v1", [model("b", {}, true)])],
  next_cursor: null,
};

function setup() {
  const fetchMock = vi.fn(async (input: string, init?: RequestInit) => {
    const url = new URL(input, "http://local");
    let payload: unknown;
    if (url.pathname === "/v1/config/history") {
      payload = url.searchParams.get("cursor") === "2" ? PAGE_2 : PAGE_1;
    } else if (url.pathname.endsWith("/rollback")) {
      payload = { ...PAGE_1.items[1].config, version: JSON.parse(String(init?.body)).version };
    }
    return { ok: true, status: 200, json: async () => payload };
  });
  vi.stubGlobal("fetch", fetchMock);
  const onRolledBack = vi.fn();
  render(<ConfigHistory config={CURRENT} onRolledBack={onRolledBack} />);
  return { fetchMock, onRolledBack };
}

describe("configuration history", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("lists entries with their difference from the current configuration", async () => {
    const { fetchMock } = setup();
    expect(await screen.findByRole("rowheader", { name: /prod-v2/ })).toBeTruthy();
    expect(screen.getByText("current")).toBeTruthy();
    expect(screen.getByText("+old; −b")).toBeTruthy();
    // The current entry has no rollback button.
    expect(screen.queryByRole("button", { name: /Roll back to prod-v3/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Load older" }));
    expect(await screen.findByRole("rowheader", { name: /prod-v1/ })).toBeTruthy();
    expect(screen.getByText("−a; ~b")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Load older" })).toBeNull();
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual([
      "/v1/config/history?limit=20",
      "/v1/config/history?limit=20&cursor=2",
    ]);
  });

  it("rolls back under the next version after confirming, warning about missing keys", async () => {
    const { fetchMock, onRolledBack } = setup();
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false);
    fireEvent.click(await screen.findByRole("button", { name: "Roll back to prod-v2 (#2)" }));
    expect(confirm.mock.calls[0][0]).toContain("as prod-v4");
    expect(confirm.mock.calls[0][0]).toContain("No stored key or api_key_env for: old");
    expect(fetchMock).toHaveBeenCalledTimes(1);

    confirm.mockReturnValueOnce(true);
    fireEvent.click(screen.getByRole("button", { name: "Roll back to prod-v2 (#2)" }));
    await screen.findByText("Rolled back to prod-v2, pushed as prod-v4.");
    const call = fetchMock.mock.calls.find(([url]) => String(url).endsWith("/rollback"));
    expect(call?.[0]).toBe("/v1/config/history/2/rollback");
    expect(call?.[1]?.method).toBe("POST");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ version: "prod-v4" });
    await waitFor(() => expect(onRolledBack).toHaveBeenCalled());
  });

  it("shows a load error", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: false,
        status: 503,
        json: async () => ({ error: "durable configuration is unavailable" }),
      })),
    );
    render(<ConfigHistory config={CURRENT} onRolledBack={() => undefined} />);
    expect(
      (await screen.findByRole("alert")).textContent,
    ).toContain("durable configuration is unavailable");
  });
});

describe("history helpers", () => {
  it("flags models without a credential and ignores parameter order", () => {
    expect(modelsWithoutCredential(PAGE_1.items[1])).toEqual(["old"]);
    const reordered = entry(9, "x", [
      model("a", { api_key_env: "A_KEY" }),
      model("b", { upstream_model: "b2" }, false),
    ]);
    expect(describeChanges(reordered, CURRENT)).toBe("same models");
    expect(describeChanges(reordered, null)).toBe("—");
  });
});
