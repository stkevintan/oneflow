import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createDaemon, rotateDaemonCredential, revokeDaemonCredential } from "../api/client";
import type { DaemonCredential } from "../api/client";
import { DaemonCredentials } from "./DaemonCredentials";

vi.mock("../api/client", () => ({
  createDaemon: vi.fn(), rotateDaemonCredential: vi.fn(), revokeDaemonCredential: vi.fn(),
}));

const credential: DaemonCredential = {
  daemon_id: "server-a", created_at: 1_700_000_000, rotated_at: null, revoked_at: null,
};

describe("daemon credential management", () => {
  afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); vi.resetAllMocks(); });

  it("creates a daemon and shows the token once without browser storage", async () => {
    const changed = vi.fn(async () => {});
    const localStorageSpy = vi.spyOn(Storage.prototype, "setItem");
    const configToml = 'cp = "wss://control.example/control"\ndaemon_id = "server-a"\ncontrol_token = "ofd_one-time-secret"\n';
    vi.mocked(createDaemon).mockResolvedValue({ credential, token: "ofd_one-time-secret", config_toml: configToml });
    const objectUrl = vi.fn((_blob: Blob) => "blob:test-config");
    vi.stubGlobal("URL", { createObjectURL: objectUrl, revokeObjectURL: vi.fn() });
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      expect(this.download).toBe("config.toml");
      expect(this.href).toBe("blob:test-config");
    });
    render(<DaemonCredentials credentials={[]} ready onChanged={changed} />);
    fireEvent.change(screen.getByLabelText("New daemon ID"), { target: { value: "server-a" } });
    fireEvent.click(screen.getByRole("button", { name: "Create daemon" }));
    const token = await screen.findByLabelText("One-time daemon token") as HTMLInputElement;
    expect(createDaemon).toHaveBeenCalledWith("server-a");
    expect(token.value).toBe("ofd_one-time-secret");
    expect(screen.getByText(/cannot be retrieved again/)).toBeTruthy();
    await waitFor(() => expect(changed).toHaveBeenCalledOnce());
    expect(localStorageSpy).not.toHaveBeenCalled();
    vi.useFakeTimers();
    fireEvent.click(screen.getByRole("button", { name: "Download config.toml" }));
    expect(click).toHaveBeenCalledOnce();
    const blob = objectUrl.mock.calls[0]?.[0] as Blob | undefined;
    expect(blob?.type).toBe("application/toml;charset=utf-8");
    expect(blob?.size).toBe(configToml.length);
    vi.advanceTimersByTime(1000);
    vi.useRealTimers();
    const contents = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(reader.error);
      reader.readAsText(blob!);
    });
    expect(contents).toBe(configToml);
    click.mockRestore();
    vi.unstubAllGlobals();
    localStorageSpy.mockRestore();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss token" }));
    expect(screen.queryByLabelText("One-time daemon token")).toBeNull();
  });

  it("requires confirmation to rotate and displays the replacement token", async () => {
    vi.mocked(rotateDaemonCredential).mockResolvedValue({
      credential: { ...credential, rotated_at: 1_700_000_100 }, token: "ofd_replacement", config_toml: 'control_token = "ofd_replacement"\n',
    });
    render(<DaemonCredentials credentials={[credential]} ready onChanged={async () => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Rotate server-a" }));
    expect(rotateDaemonCredential).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Rotate server-a" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm rotate" }));
    expect((await screen.findByLabelText("One-time daemon token") as HTMLInputElement).value).toBe("ofd_replacement");
    expect(rotateDaemonCredential).toHaveBeenCalledWith("server-a");
  });

  it("revokes only after confirmation and explains bounded offline permissions", async () => {
    vi.mocked(revokeDaemonCredential).mockResolvedValue({ ...credential, revoked_at: 1_700_000_200 });
    const changed = vi.fn(async () => {});
    render(<DaemonCredentials credentials={[credential]} ready onChanged={changed} />);
    fireEvent.click(screen.getByRole("button", { name: "Revoke server-a" }));
    expect(revokeDaemonCredential).not.toHaveBeenCalled();
    expect(screen.getByText(/300 seconds plus clock skew/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Confirm revoke" }));
    expect((await screen.findByRole("status")).textContent).toContain("Credential revoked for server-a");
    expect(revokeDaemonCredential).toHaveBeenCalledWith("server-a");
    await waitFor(() => expect(changed).toHaveBeenCalledOnce());
  });

  it("shows server errors without success messages", async () => {
    vi.mocked(createDaemon).mockRejectedValue(new Error("daemon_id already exists"));
    const changed = vi.fn(async () => {});
    render(<DaemonCredentials credentials={[]} ready onChanged={changed} />);
    fireEvent.change(screen.getByLabelText("New daemon ID"), { target: { value: "server-a" } });
    fireEvent.click(screen.getByRole("button", { name: "Create daemon" }));
    expect((await screen.findByRole("alert")).textContent).toContain("daemon_id already exists");
    expect(screen.queryByLabelText("One-time daemon token")).toBeNull();
    expect(changed).not.toHaveBeenCalled();
  });

  it("disables provisioning while disconnected and shows revoked daemons", () => {
    render(<DaemonCredentials credentials={[{ ...credential, revoked_at: 1_700_000_200 }]}
      ready={false} onChanged={async () => {}} />);
    expect((screen.getByRole("button", { name: "Create daemon" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Revoke server-a" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Reactivate")).toBeTruthy();
    expect(screen.getByText("Revoked")).toBeTruthy();
  });
});
