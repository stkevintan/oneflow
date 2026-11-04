import { useEffect, useState } from "react";
import { generateVirtualKey, listVirtualKeys, revokeVirtualKey } from "../api/client";
import type { GenerateKeyResponse } from "../api/client";
import type { DashboardSnapshot } from "../api/useDashboard";
import { Card } from "./Card";
import { TelemetryTable } from "./TelemetryTable";

interface Props {
  daemonId: string;
  snapshot: DashboardSnapshot | null;
  ready: boolean;
}

const inputClass = "rounded border border-slate-700 bg-slate-950 px-3 py-2 text-slate-200";
const buttonClass = "rounded bg-sky-700 px-3 py-2 text-white disabled:opacity-50";

export function DaemonDetails({ daemonId, snapshot, ready }: Props) {
  const credential = snapshot?.credentials.find((item) => item.daemon_id === daemonId);
  const connections = snapshot?.daemons.filter((item) => item.daemon_id === daemonId) ?? [];
  const [consumer, setConsumer] = useState("");
  const [duration, setDuration] = useState("");
  const [excludedModels, setExcludedModels] = useState<string[]>([]);
  const [page, setPage] = useState(1);
  const [revision, setRevision] = useState(0);
  const [listing, setListing] = useState<Awaited<ReturnType<typeof listVirtualKeys>> | null>(null);
  const [issued, setIssued] = useState<GenerateKeyResponse | null>(null);
  const [confirmation, setConfirmation] = useState<{ hash: string; alias: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const models = snapshot?.config.models.map((model) => model.model_name) ?? [];
  const allowedModels = models.filter((model) => !excludedModels.includes(model));
  const disabled = !ready || busy;
  const exists = credential !== undefined;

  useEffect(() => {
    if (!ready || !exists) {
      setListing(null);
      setLoading(false);
      return;
    }
    const controller = new AbortController();
    setLoading(true);
    setListError(null);
    void listVirtualKeys(daemonId, page, controller.signal).then((result) => {
      if (controller.signal.aborted) return;
      if (page > Math.max(1, result.total_pages)) {
        setPage(Math.max(1, result.total_pages));
      } else {
        setListing(result);
      }
    }).catch((cause: unknown) => {
      if (!controller.signal.aborted) {
        setListing(null);
        setListError(cause instanceof Error ? cause.message : "Virtual-key listing failed");
      }
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });
    return () => controller.abort();
  }, [daemonId, page, revision, ready, exists]);

  async function createKey() {
    setBusy(true);
    setError(null);
    setNotice(null);
    setIssued(null);
    try {
      const result = await generateVirtualKey({
        daemon_id: daemonId, key_alias: consumer.trim(),
        models: allowedModels, duration: duration.trim() || null,
      });
      setIssued(result);
      setConsumer("");
      setPage(1);
      setRevision((value) => value + 1);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Virtual-key creation failed");
    } finally {
      setBusy(false);
    }
  }

  async function revokeKey(hash: string) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await revokeVirtualKey(hash);
      if (issued?.token === hash) setIssued(null);
      setConfirmation(null);
      setNotice("Consumer key revoked. Disconnected daemons may accept cached permissions until their policy lease expires (up to 300 seconds plus clock skew). In-flight requests may finish.");
      setRevision((value) => value + 1);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Virtual-key revocation failed");
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <a href="#" className="text-sm text-sky-300">Back to dashboard</a>
      <Card title={`Daemon: ${daemonId}`}>
        <p className="text-sm text-slate-300">
          {!ready ? "Control plane unavailable or administrator authentication required."
            : !credential ? "Daemon not found."
              : credential.revoked_at !== null ? "Daemon credential revoked."
                : connections.length > 0 ? "Connected" : "Offline"}
        </p>
        {connections.map((connection) => (
          <p key={connection.connection_id} className="mt-2 text-sm text-slate-400">
            Host: {connection.hostname ?? "Unknown"} | Version: {connection.version ?? "Unknown"}
            {" | "}Loaded models: {connection.loaded_models.join(", ") || "None"}
            {" | "}Config: {connection.config_in_sync ? "in sync" : "pending"}
            {connection.applied_config_version !== null && ` (applied ${connection.applied_config_version})`}
          </p>
        ))}
        <p className="mt-2 text-sm text-slate-400">Consumers use the daemon&apos;s reachable inference URL
          (for example <code>http://&lt;daemon-host&gt;:9000/v1</code>) and a virtual key below.
          The default listener is loopback-only; configure the listener and firewall for remote consumers.</p>
      </Card>
      {credential ? (
        <Card title="Consumer virtual keys" hint="Keys created here authorize this daemon only.">
          <p className="mb-4 text-sm text-slate-400">
            These are consumer API credentials, not upstream provider keys or daemon control tokens.
            Model selections restrict what each consumer may access. Keys created elsewhere without a daemon
            binding are cross-daemon and are not listed here.
          </p>
          <form className="mb-4 flex flex-col gap-3 text-sm" onSubmit={(event) => {
            event.preventDefault();
            void createKey();
          }}>
            <div className="flex flex-wrap items-center gap-3">
              <label htmlFor="consumer-name" className="text-slate-300">Consumer name</label>
              <input id="consumer-name" required maxLength={128} value={consumer}
                onChange={(event) => setConsumer(event.target.value)} disabled={disabled}
                placeholder="my-app" className={inputClass} />
              <label htmlFor="consumer-duration" className="text-slate-300">Expires after</label>
              <input id="consumer-duration" value={duration} pattern="[0-9]+[smhd]"
                onChange={(event) => setDuration(event.target.value)} disabled={disabled}
                placeholder="30d (blank: never)" className={inputClass} />
            </div>
            <fieldset disabled={disabled} className="flex flex-wrap gap-3 text-slate-300">
              <legend className="mb-2">Allowed models</legend>
              {models.map((model) => (
                <label key={model} className="flex items-center gap-2">
                  <input type="checkbox" checked={!excludedModels.includes(model)}
                    onChange={(event) => setExcludedModels((current) => event.target.checked
                      ? current.filter((item) => item !== model) : [...current, model])} />
                  {model}
                </label>
              ))}
            </fieldset>
            {models.length === 0 ? <p className="text-amber-300">Configure at least one proxied model on the dashboard before creating a key.</p> : null}
            <button type="submit" disabled={disabled || credential.revoked_at !== null || allowedModels.length === 0 || !consumer.trim()}
              className={`${buttonClass} self-start`}>Create virtual key</button>
          </form>
          {error ? <p role="alert" className="mb-3 text-sm text-rose-300">{error}</p> : null}
          {notice ? <p role="status" className="mb-3 text-sm text-emerald-300">{notice}</p> : null}
          {issued ? (
            <section className="mb-4 rounded border border-amber-700 p-4 text-sm text-slate-200">
              <p>Save this consumer key now. It is shown only once and cannot be recovered.</p>
              <label htmlFor="issued-consumer-key" className="mt-3 block">One-time consumer key</label>
              <input id="issued-consumer-key" readOnly autoComplete="off" spellCheck={false}
                value={issued.key} onFocus={(event) => event.target.select()}
                className={`${inputClass} my-2 w-full font-mono`} />
              <p>Consumer: {issued.key_alias} | Daemon: {daemonId}</p>
              <button type="button" className="mt-3 text-sky-300" onClick={() => setIssued(null)}>Dismiss key</button>
            </section>
          ) : null}
          {confirmation ? (
            <div role="group" aria-label="Confirm consumer key revocation"
              className="mb-4 rounded border border-amber-800 p-3 text-sm text-amber-200">
              <p>Revoke key for {confirmation.alias}? This cannot be undone.</p>
              <button type="button" disabled={disabled} className={`${buttonClass} mr-3 mt-2`}
                onClick={() => void revokeKey(confirmation.hash)}>Confirm revoke key</button>
              <button type="button" disabled={busy} onClick={() => setConfirmation(null)}>Cancel</button>
            </div>
          ) : null}
          <div className="mb-3 flex items-center gap-3 text-sm text-slate-300">
            <span>{listing?.total_count ?? 0} keys | Page {page} of {Math.max(1, listing?.total_pages ?? 1)}</span>
            <button type="button" disabled={disabled || loading || page <= 1} onClick={() => setPage((value) => value - 1)}>Previous</button>
            <button type="button" disabled={disabled || loading || page >= (listing?.total_pages ?? 1)}
              onClick={() => setPage((value) => value + 1)}>Next</button>
            <button type="button" disabled={disabled || loading} onClick={() => setRevision((value) => value + 1)}>Refresh keys</button>
          </div>
          {listError ? <p role="alert" className="mb-3 text-sm text-rose-300">{listError}</p> : null}
          {loading ? <p role="status" className="text-sm text-slate-400">Loading keys...</p> : null}
          <div className="overflow-x-auto">
            <table className="w-full text-left text-sm text-slate-300">
              <thead><tr>{["Consumer", "Allowed models", "Created", "Expires", "Actions"].map((heading) =>
                <th key={heading} scope="col" className="border-b border-slate-800 p-2">{heading}</th>
              )}</tr></thead>
              <tbody>{listing?.keys.map((key) => (
                <tr key={key.token}>
                  <td className="p-2">{key.key_alias}</td>
                  <td className="p-2">{key.models.join(", ")}</td>
                  <td className="p-2">{key.created_at}</td>
                  <td className="p-2">{key.expires
                    ? `${key.expires}${Date.parse(key.expires) <= Date.now() ? " (Expired)" : ""}` : "Never"}</td>
                  <td className="p-2"><button type="button" disabled={disabled || loading}
                    aria-label={`Revoke key ${key.key_alias}`}
                    onClick={() => setConfirmation({ hash: key.token, alias: key.key_alias })}>Revoke</button></td>
                </tr>
              ))}</tbody>
            </table>
          </div>
          {listing?.keys.length === 0 && !loading ? <p className="mt-3 text-sm text-slate-400">No consumer keys for this daemon.</p> : null}
        </Card>
      ) : null}
      <Card title="Recent daemon usage" hint="Events for this daemon within the dashboard's recent usage window">
        <TelemetryTable events={snapshot?.usage.filter((event) => event.daemon_id === daemonId) ?? []} limit={25} />
      </Card>
    </>
  );
}
