import type { ConnectionState } from "../api/useStatus";
import type { ServerInfo } from "../api/client";
import { formatUptime } from "../lib/format";

interface TopbarProps {
  server: ServerInfo | null;
  connection: ConnectionState;
  onRefresh: () => void;
}

const CONNECTION_STYLES: Record<ConnectionState, { dot: string; label: string; text: string }> = {
  connecting: { dot: "bg-amber-400", label: "connecting", text: "text-amber-300" },
  live: { dot: "bg-emerald-400", label: "live", text: "text-emerald-300" },
  offline: { dot: "bg-rose-500", label: "offline", text: "text-rose-300" },
};

export function Topbar({ server, connection, onRefresh }: TopbarProps) {
  const styles = CONNECTION_STYLES[connection];

  const summary = server
    ? `v${server.version} · ${server.connected_daemons} daemon(s) · config ${server.config_version} · uptime ${formatUptime(server.uptime_seconds)}`
    : "Waiting for the control plane…";

  return (
    <header className="flex flex-wrap items-center justify-between gap-x-6 gap-y-3 border-b border-slate-800 bg-slate-900 px-7 py-4">
      <div>
        <h1 className="text-lg font-semibold tracking-tight text-slate-100">LLM Control Plane</h1>
        <p className="mt-0.5 text-sm text-slate-400">{summary}</p>
      </div>
      <div className="flex items-center gap-3">
        <span className={`inline-flex items-center gap-2 text-sm ${styles.text}`}>
          <span className={`h-2.5 w-2.5 rounded-full ${styles.dot}`} aria-hidden="true" />
          {styles.label}
        </span>
        <button
          type="button"
          onClick={onRefresh}
          className="rounded-md border border-slate-700 bg-slate-800 px-3.5 py-1.5 text-sm text-slate-100 transition hover:border-sky-500 hover:bg-slate-800/80 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-500"
        >
          Refresh
        </button>
      </div>
    </header>
  );
}
