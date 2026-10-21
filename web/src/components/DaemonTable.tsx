import type { Daemon } from "../api/client";
import { formatClock, formatCount, formatRelative } from "../lib/format";
import { EmptyState } from "./Card";

interface DaemonTableProps {
  daemons: Daemon[];
}

const HEADERS = [
  "Daemon",
  "Cluster",
  "Host",
  "Version",
  "Active",
  "Requests",
  "Tokens",
  "Models",
  "Last seen",
] as const;

export function DaemonTable({ daemons }: DaemonTableProps) {
  return (
    <>
      <div className="overflow-x-auto">
        <table className="w-full border-collapse text-sm">
          <thead>
            <tr>
              {HEADERS.map((header) => (
                <th
                  key={header}
                  scope="col"
                  className="border-b border-slate-800 px-3 py-2 text-left text-xs font-medium uppercase tracking-wide text-slate-500"
                >
                  {header}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {daemons.map((daemon) => (
              <tr key={daemon.connection_id} className="hover:bg-slate-800/40">
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 font-mono text-xs text-slate-200">
                  {daemon.daemon_id ?? `(unregistered ${daemon.connection_id.slice(0, 8)})`}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {daemon.cluster_name ?? "—"}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {daemon.hostname ?? "—"}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {daemon.version ?? "—"}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {daemon.active_requests}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {formatCount(daemon.total_requests)}
                  <span className="text-slate-500"> ({formatCount(daemon.failed_requests)} failed)</span>
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {formatCount(daemon.total_prompt_tokens + daemon.total_completion_tokens)}
                </td>
                <td className="border-b border-slate-800/70 px-3 py-2">
                  {daemon.loaded_models.length === 0 ? (
                    <span className="text-slate-500">—</span>
                  ) : (
                    <div className="flex flex-wrap gap-1">
                      {daemon.loaded_models.map((model) => (
                        <span
                          key={model}
                          className="rounded border border-slate-700 bg-slate-800 px-2 py-0.5 font-mono text-[11px] text-slate-300"
                        >
                          {model}
                        </span>
                      ))}
                    </div>
                  )}
                </td>
                <td
                  className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300"
                  title={formatClock(daemon.last_seen)}
                >
                  {formatRelative(daemon.last_seen)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <EmptyState visible={daemons.length === 0}>
        No daemons connected yet. Start one with <code className="text-slate-400">cargo run -p daemon</code>.
      </EmptyState>
    </>
  );
}
