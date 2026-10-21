import type { UsageEvent } from "../api/client";
import { formatClock, formatLatency } from "../lib/format";
import { EmptyState } from "./Card";

interface TelemetryTableProps {
  /** Newest first, as returned by `GET /v1/usage/events`. */
  events: UsageEvent[];
  limit?: number;
}

const HEADERS = [
  "Received",
  "Daemon",
  "Model",
  "Prompt",
  "Completion",
  "Latency",
  "Stream",
  "Result",
] as const;

/**
 * Token counts are rendered as an em dash when the upstream reported no `usage`
 * object, so "unknown" is never confused with "zero tokens".
 */
function TokenCell({
  reported,
  value,
}: {
  reported: boolean;
  value: number;
}) {
  if (!reported) {
    return (
      <td
        className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-500"
        title="The upstream returned no usage object for this request"
      >
        —
      </td>
    );
  }
  return (
    <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
      {value.toLocaleString()}
    </td>
  );
}

export function TelemetryTable({ events, limit = 25 }: TelemetryTableProps) {
  const recent = events.slice(0, limit);

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
            {recent.map((event, index) => (
              <tr
                key={`${event.received_at}-${event.request_id}-${index}`}
                className="hover:bg-slate-800/40"
              >
                <td
                  className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300"
                  title={`request ${event.request_id}`}
                >
                  {formatClock(event.received_at)}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 font-mono text-xs text-slate-200">
                  {event.daemon_id}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 font-mono text-xs text-slate-200">
                  {event.model_name}
                </td>
                <TokenCell
                  reported={event.usage_reported}
                  value={event.prompt_tokens}
                />
                <TokenCell
                  reported={event.usage_reported}
                  value={event.completion_tokens}
                />
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-300">
                  {formatLatency(event.latency_ms)}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2 text-slate-400">
                  {event.streamed ? "stream" : "unary"}
                </td>
                <td className="whitespace-nowrap border-b border-slate-800/70 px-3 py-2">
                  {event.success ? (
                    <span className="text-emerald-400">ok</span>
                  ) : (
                    <span className="text-rose-400">
                      failed
                      {event.status_code === null ? "" : ` (${event.status_code})`}
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <EmptyState visible={recent.length === 0}>No telemetry received yet.</EmptyState>
    </>
  );
}
