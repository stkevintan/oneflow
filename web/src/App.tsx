import { NO_USAGE_FILTER, RECENT_USAGE_LIMIT, useDashboard } from "./api/useDashboard";
import type { UsageFilter } from "./api/useDashboard";
import { Card } from "./components/Card";
import { DaemonTable } from "./components/DaemonTable";
import { DaemonCredentials } from "./components/DaemonCredentials";
import { DaemonDetails } from "./components/DaemonDetails";
import { ConfigHistory } from "./components/ConfigHistory";
import { ModelList } from "./components/ModelList";
import { TelemetryTable } from "./components/TelemetryTable";
import { Topbar } from "./components/Topbar";

export default function App() {
  const [usageFilter, setUsageFilter] = useState<UsageFilter>(NO_USAGE_FILTER);
  const { snapshot, error, connection, refresh } = useDashboard(3000, usageFilter);
  const filtered = usageFilter.daemon_id !== "" || usageFilter.model_name !== "";
  // Offer every daemon and model the dashboard knows, plus the current choice.
  const daemonOptions = [...new Set([
    ...(snapshot?.credentials ?? []).map((credential) => credential.daemon_id),
    ...(snapshot?.daemons ?? []).flatMap((daemon) => (daemon.daemon_id ? [daemon.daemon_id] : [])),
    ...(usageFilter.daemon_id ? [usageFilter.daemon_id] : []),
  ])].sort();
  const modelOptions = [...new Set([
    ...(snapshot?.config.models ?? []).map((model) => model.model_name),
    ...(usageFilter.model_name ? [usageFilter.model_name] : []),
  ])].sort();
  const [token, setToken] = useState("");
  const [authRevision, setAuthRevision] = useState(0);
  const [daemonId, setDaemonId] = useState(() => new URLSearchParams(window.location.hash.slice(1)).get("daemon"));

  useEffect(() => {
    const onHashChange = () => setDaemonId(new URLSearchParams(window.location.hash.slice(1)).get("daemon"));
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  return (
    <div className="min-h-screen bg-slate-950">
      <Topbar server={snapshot?.status ?? null} connection={connection} onRefresh={refresh} />

      <main className="mx-auto flex max-w-[1400px] flex-col gap-5 px-7 py-6">
        <form
          className="flex flex-wrap items-center gap-3 text-sm text-slate-300"
          onSubmit={(event) => {
            event.preventDefault();
            setAdminToken(token);
            setToken("");
            setAuthRevision((revision) => revision + 1);
            void refresh();
          }}
        >
          <label htmlFor="admin-token">Administrator token</label>
          <input
            id="admin-token"
            type="password"
            autoComplete="off"
            value={token}
            onChange={(event) => setToken(event.target.value)}
            className="rounded border border-slate-700 bg-slate-900 px-3 py-2"
          />
          <button type="submit" className="rounded bg-slate-800 px-3 py-2">
            Apply token
          </button>
          <span>Required for management. Kept in memory until page reload.</span>
        </form>
        {error ? (
          <p
            role="alert"
            className="rounded-lg border border-rose-900/60 bg-rose-950/40 px-4 py-3 text-sm text-rose-200"
          >
            Cannot reach the control plane: {error}
          </p>
        ) : null}

        {daemonId ? (
          <DaemonDetails
            key={`${authRevision}:${daemonId}`}
            daemonId={daemonId}
            snapshot={snapshot}
            ready={connection === "live"}
          />
        ) : (
          <>
            <DaemonCredentials
              key={authRevision}
              credentials={snapshot?.credentials ?? []}
              ready={connection === "live"}
              onChanged={refresh}
            />

            <Card
              title="Connected daemons"
              badge={
                <span className="rounded-full border border-slate-700 bg-slate-800 px-2 py-0.5 text-xs font-medium normal-case tracking-normal text-slate-400">
                  {snapshot?.daemons.length ?? 0}
                </span>
              }
            >
              <DaemonTable daemons={snapshot?.daemons ?? []} />
            </Card>

            <Card
              title="Recent usage"
              hint={`Latest ${RECENT_USAGE_LIMIT} events${filtered ? " matching the filters" : " across all daemons"}`}
            >
              <div className="mb-3 flex flex-wrap items-center gap-3 text-sm text-slate-300">
                <label htmlFor="usage-daemon">Daemon</label>
                <select
                  id="usage-daemon"
                  value={usageFilter.daemon_id}
                  onChange={(event) => setUsageFilter((current) => ({ ...current, daemon_id: event.target.value }))}
                  className="rounded border border-slate-700 bg-slate-900 px-2 py-1"
                >
                  <option value="">All daemons</option>
                  {daemonOptions.map((id) => <option key={id} value={id}>{id}</option>)}
                </select>
                <label htmlFor="usage-model">Model</label>
                <select
                  id="usage-model"
                  value={usageFilter.model_name}
                  onChange={(event) => setUsageFilter((current) => ({ ...current, model_name: event.target.value }))}
                  className="rounded border border-slate-700 bg-slate-900 px-2 py-1"
                >
                  <option value="">All models</option>
                  {modelOptions.map((name) => <option key={name} value={name}>{name}</option>)}
                </select>
                {filtered ? (
                  <button
                    type="button"
                    className="text-xs text-sky-300 hover:underline"
                    onClick={() => setUsageFilter(NO_USAGE_FILTER)}
                  >
                    Clear filters
                  </button>
                ) : null}
              </div>
              <TelemetryTable events={snapshot?.usage ?? []} limit={RECENT_USAGE_LIMIT} filtered={filtered} />
            </Card>

            <ModelList config={snapshot?.config ?? null} onPushed={refresh} />
            <ConfigHistory config={snapshot?.config ?? null} onRolledBack={refresh} />
          </>
        )}
      </main>
    </div>
  );
}
import { useEffect, useState } from "react";
import { setAdminToken } from "./api/client";
