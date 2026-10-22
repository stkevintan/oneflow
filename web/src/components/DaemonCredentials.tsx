import { useState } from "react";
import {
  createDaemon, rotateDaemonCredential, revokeDaemonCredential,
} from "../api/client";
import type { DaemonCredential, IssuedDaemonCredential } from "../api/client";
import { formatClock } from "../lib/format";
import { Card } from "./Card";

interface Props {
  credentials: DaemonCredential[];
  ready: boolean;
  onChanged: () => Promise<void>;
}

export function DaemonCredentials({ credentials, ready, onChanged }: Props) {
  const [daemonId, setDaemonId] = useState("");
  const [issued, setIssued] = useState<IssuedDaemonCredential | null>(null);
  const [confirmation, setConfirmation] = useState<{ id: string; action: "rotate" | "revoke" } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const disabled = busy || !ready;

  function downloadConfig() {
    if (!issued) return;
    setError(null);
    try {
      const blob = new Blob([issued.config_toml], { type: "application/toml;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = "config.toml";
      document.body.appendChild(link);
      try {
        link.click();
      } finally {
        link.remove();
        window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
    } catch (cause) {
      setError(`Configuration download failed: ${cause instanceof Error ? cause.message : "unknown error"}`);
    }
  }

  async function perform(action: "create" | "rotate" | "revoke", id: string) {
    setBusy(true);
    setError(null);
    setNotice(null);
    setIssued(null);
    try {
      if (action === "revoke") {
        await revokeDaemonCredential(id);
        setNotice(`Credential revoked for ${id}. Existing control sessions close within 5 seconds; cached inference permissions expire with their policy lease.`);
      } else {
        const result = await (action === "create" ? createDaemon(id) : rotateDaemonCredential(id));
        setIssued(result);
        if (action === "create") setDaemonId("");
      }
      setConfirmation(null);
      await onChanged();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Daemon credential operation failed");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Daemon credentials" hint="Create a daemon and download its one-time configuration.">
      <form className="mb-4 flex flex-wrap items-center gap-3 text-sm" onSubmit={(event) => {
        event.preventDefault();
        void perform("create", daemonId);
      }}>
        <label htmlFor="new-daemon-id" className="text-slate-300">New daemon ID</label>
        <input id="new-daemon-id" required maxLength={128} pattern="[A-Za-z0-9._\-]+"
          value={daemonId} onChange={(event) => setDaemonId(event.target.value)} disabled={disabled}
          autoComplete="off" placeholder="server-a"
          className="rounded border border-slate-700 bg-slate-900 px-3 py-2 text-slate-200" />
        <button type="submit" disabled={disabled}
          className="rounded bg-sky-700 px-3 py-2 text-white disabled:opacity-50">Create daemon</button>
      </form>
      {error ? <p role="alert" className="mb-3 text-sm text-rose-300">{error}</p> : null}
      {notice ? <p role="status" className="mb-3 text-sm text-emerald-300">{notice}</p> : null}
      {issued ? (
        <section className="mb-4 rounded border border-amber-700 bg-slate-900 p-4 text-sm text-slate-200">
          <p className="mb-2">Download the configuration now for <strong>{issued.credential.daemon_id}</strong>. It contains a secret token and cannot be retrieved again.</p>
          <button type="button" onClick={downloadConfig}
            className="mb-3 rounded bg-sky-700 px-3 py-2 text-white">Download config.toml</button>
          <label htmlFor="issued-daemon-token">One-time daemon token</label>
          <input id="issued-daemon-token" readOnly autoComplete="off" spellCheck={false}
            value={issued.token} onFocus={(event) => event.target.select()}
            className="my-2 block w-full rounded border border-slate-700 bg-slate-950 p-2 font-mono" />
          <p>The file includes the control-plane URL, daemon ID, and token. Transfer it securely to the daemon host, restrict access with <code>chmod 600 config.toml</code>, then run <code>oneflowd -c config.toml</code>.</p>
          <p className="mt-2">An existing <code>ONEFLOW_DAEMON_TOKEN</code> overrides the downloaded token; unset it when using this file.</p>
          <button type="button" onClick={() => setIssued(null)}
            className="mt-3 rounded bg-slate-700 px-3 py-2">Dismiss token</button>
        </section>
      ) : null}
      {confirmation ? (
        <div role="group" aria-label="Confirm credential change" className="mb-4 rounded border border-amber-800 p-3 text-sm text-amber-200">
          <p>{confirmation.action === "rotate" ? "Rotate" : "Revoke"} credential for {confirmation.id}?
            {" "}The current token will stop working and its control sessions will close.
            Cached inference permissions can remain valid for up to 300 seconds plus clock skew.
            {confirmation.action === "rotate" ? " Deploy the new token and restart the daemon." : ""}</p>
          <button type="button" disabled={disabled}
            onClick={() => void perform(confirmation.action, confirmation.id)}
            className="mr-3 mt-2 rounded bg-amber-900 px-3 py-2">Confirm {confirmation.action}</button>
          <button type="button" disabled={busy} onClick={() => setConfirmation(null)}>Cancel</button>
        </div>
      ) : null}
      <div className="overflow-x-auto">
        <table className="w-full text-left text-sm text-slate-300">
          <thead><tr>{["Daemon ID", "Credential", "Created", "Last rotated", "Actions"].map((heading) =>
            <th key={heading} scope="col" className="border-b border-slate-800 p-2">{heading}</th>
          )}</tr></thead>
          <tbody>{credentials.map((credential) => (
            <tr key={credential.daemon_id}>
              <td className="p-2 font-mono">{credential.daemon_id}</td>
              <td className="p-2">{credential.revoked_at === null ? "Active" : "Revoked"}</td>
              <td className="p-2">{formatClock(credential.created_at)}</td>
              <td className="p-2">{credential.rotated_at === null ? "Never" : formatClock(credential.rotated_at)}</td>
              <td className="flex gap-3 p-2">
                <button type="button" disabled={disabled} aria-label={`Rotate ${credential.daemon_id}`}
                  onClick={() => setConfirmation({ id: credential.daemon_id, action: "rotate" })}>
                  {credential.revoked_at === null ? "Rotate" : "Reactivate"}
                </button>
                <button type="button" disabled={disabled || credential.revoked_at !== null}
                  aria-label={`Revoke ${credential.daemon_id}`}
                  onClick={() => setConfirmation({ id: credential.daemon_id, action: "revoke" })}>Revoke</button>
              </td>
            </tr>
          ))}</tbody>
        </table>
      </div>
      {ready && credentials.length === 0 ? <p className="mt-3 text-sm text-slate-400">No daemon credentials provisioned yet.</p> : null}
    </Card>
  );
}
