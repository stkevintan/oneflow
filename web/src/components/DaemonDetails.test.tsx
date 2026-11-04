import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { generateVirtualKey, listVirtualKeys, revokeVirtualKey } from "../api/client";
import type { VirtualKey } from "../api/client";
import type { DashboardSnapshot } from "../api/useDashboard";
import { DaemonDetails } from "./DaemonDetails";

vi.mock("../api/client", () => ({
  generateVirtualKey: vi.fn(), listVirtualKeys: vi.fn(), revokeVirtualKey: vi.fn(),
}));

const consumerKey: VirtualKey = {
  token: "a".repeat(64), token_id: "b".repeat(32), daemon_id: "server-a",
  key_alias: "external-app", models: ["demo-model"], blocked: false,
  created_at: "2026-10-22T09:00:00Z", expires: null,
};
const snapshot: DashboardSnapshot = {
  status: { version: "0.1.0", uptime_seconds: 10, connected_daemons: 0, config_version: "v1" },
  credentials: [{ daemon_id: "server-a", created_at: 100, revoked_at: null, rotated_at: null }],
  daemons: [], usage: [],
  config: {
    version: "v1",
    models: ["demo-model", "other-model"].map((name) => ({
      model_name: name, provider: "custom_openai", api_base: "https://provider.example/v1", parameters: {},
    })),
  },
};
const emptyList = { keys: [], total_count: 0, current_page: 1, total_pages: 0 };

describe("daemon consumer virtual keys", () => {
  beforeEach(() => {
    vi.mocked(listVirtualKeys).mockResolvedValue(emptyList);
  });
  afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.resetAllMocks(); });

  it("creates a daemon-bound consumer credential and displays it only once", async () => {
    const storage = vi.spyOn(Storage.prototype, "setItem");
    vi.mocked(generateVirtualKey).mockResolvedValue({ ...consumerKey, key: "sk-consumer-secret" });
    render(<DaemonDetails daemonId="server-a" snapshot={snapshot} ready />);
    await screen.findByText("No consumer keys for this daemon.");
    expect(screen.getByText("Offline")).toBeTruthy();
    expect(listVirtualKeys).toHaveBeenCalledWith("server-a", 1, expect.any(AbortSignal));
    fireEvent.change(screen.getByLabelText("Consumer name"), { target: { value: "external-app" } });
    fireEvent.change(screen.getByLabelText("Expires after"), { target: { value: "30d" } });
    fireEvent.click(screen.getByLabelText("other-model"));
    fireEvent.click(screen.getByRole("button", { name: "Create virtual key" }));
    expect((await screen.findByLabelText("One-time consumer key") as HTMLInputElement).value).toBe("sk-consumer-secret");
    expect(generateVirtualKey).toHaveBeenCalledWith({
      daemon_id: "server-a", key_alias: "external-app", models: ["demo-model"], duration: "30d",
    });
    expect(storage).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss key" }));
    expect(screen.queryByLabelText("One-time consumer key")).toBeNull();
  });

  it("lists metadata and revokes only after confirmation using the hash, not plaintext", async () => {
    vi.mocked(listVirtualKeys).mockResolvedValue({ keys: [consumerKey], total_count: 1, current_page: 1, total_pages: 1 });
    vi.mocked(revokeVirtualKey).mockResolvedValue({ deleted_keys: [consumerKey.token] });
    render(<DaemonDetails daemonId="server-a" snapshot={snapshot} ready />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke key external-app" }));
    expect(revokeVirtualKey).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: "Confirm consumer key revocation" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Revoke key external-app" }));
    vi.mocked(listVirtualKeys).mockResolvedValue(emptyList);
    fireEvent.click(screen.getByRole("button", { name: "Confirm revoke key" }));
    await screen.findByText(/Consumer key revoked/);
    expect(revokeVirtualKey).toHaveBeenCalledWith(consumerKey.token);
    await screen.findByText("No consumer keys for this daemon.");
  });

  it("surfaces creation, listing, and revocation errors without showing success", async () => {
    vi.mocked(listVirtualKeys).mockRejectedValueOnce(new Error("listing unavailable"));
    vi.mocked(generateVirtualKey).mockRejectedValue(new Error("storage unavailable"));
    render(<DaemonDetails daemonId="server-a" snapshot={snapshot} ready />);
    await screen.findByText("listing unavailable");
    fireEvent.change(screen.getByLabelText("Consumer name"), { target: { value: "consumer" } });
    fireEvent.click(screen.getByRole("button", { name: "Create virtual key" }));
    await screen.findByText("storage unavailable");
    expect(screen.queryByLabelText("One-time consumer key")).toBeNull();
    vi.mocked(listVirtualKeys).mockResolvedValue({ keys: [consumerKey], total_count: 1, current_page: 1, total_pages: 1 });
    fireEvent.click(screen.getByRole("button", { name: "Refresh keys" }));
    fireEvent.click(await screen.findByRole("button", { name: "Revoke key external-app" }));
    vi.mocked(revokeVirtualKey).mockRejectedValue(new Error("revocation unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "Confirm revoke key" }));
    await screen.findByText("revocation unavailable");
    expect(screen.queryByText(/Consumer key revoked/)).toBeNull();
  });

  it("paginates daemon-filtered metadata", async () => {
    vi.mocked(listVirtualKeys).mockResolvedValue({ keys: [consumerKey], total_count: 11, current_page: 1, total_pages: 2 });
    render(<DaemonDetails daemonId="server-a" snapshot={snapshot} ready />);
    await waitFor(() => expect((screen.getByRole("button", { name: "Next" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    await waitFor(() => expect(listVirtualKeys).toHaveBeenCalledWith("server-a", 2, expect.any(AbortSignal)));
  });

  it("disables creation without models, when unauthenticated, or for revoked daemons", async () => {
    const { rerender } = render(<DaemonDetails daemonId="server-a" snapshot={snapshot} ready={false} />);
    expect(listVirtualKeys).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "Create virtual key" }) as HTMLButtonElement).disabled).toBe(true);
    const noModels = { ...snapshot, config: { version: "empty", models: [] } };
    rerender(<DaemonDetails daemonId="server-a" snapshot={noModels} ready />);
    await screen.findByText(/Configure at least one proxied model/);
    fireEvent.change(screen.getByLabelText("Consumer name"), { target: { value: "consumer" } });
    expect((screen.getByRole("button", { name: "Create virtual key" }) as HTMLButtonElement).disabled).toBe(true);
    rerender(<DaemonDetails daemonId="server-a" snapshot={{
      ...snapshot, credentials: [{ ...snapshot.credentials[0]!, revoked_at: 200 }],
    }} ready />);
    expect((screen.getByRole("button", { name: "Create virtual key" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("reports an unknown daemon rather than managing another daemon's keys", () => {
    render(<DaemonDetails daemonId="missing" snapshot={snapshot} ready />);
    expect(screen.getByText("Daemon not found.")).toBeTruthy();
    expect(listVirtualKeys).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Create virtual key" })).toBeNull();
  });
});
