import { useCallback, useEffect, useState } from "react";

import { listConfigHistory, rollbackConfig } from "../api/client";
import type { ConfigHistoryEntry, ModelConfig, ModelConfigSet } from "../api/client";
import { formatClock } from "../lib/format";
import { nextVersion } from "../lib/modelForm";
import { Card, EmptyState } from "./Card";

interface ConfigHistoryProps {
  /** Current configuration; history reloads whenever its version changes. */
  config: ModelConfigSet | null;
  onRolledBack: () => void;
}

const PAGE_SIZE = 20;
const CELL = "whitespace-nowrap border-b border-slate-800/70 px-3 py-2";
const HEADERS = ["Version", "Stored", "Source", "Models", "Changes vs current", "Actions"];

/** Models a rollback would leave without any upstream credential. */
export function modelsWithoutCredential(entry: ConfigHistoryEntry): string[] {
  return entry.config.models
    .filter((model) => !model.parameters?.api_key_env && model.api_key_configured !== true)
    .map((model) => model.model_name);
}

function comparable(model: ModelConfig): string {
  const { api_key_configured: _ignored, ...rest } = model;
  const parameters = Object.fromEntries(
    Object.entries(rest.parameters ?? {}).sort(([a], [b]) => a.localeCompare(b)),
  );
  return JSON.stringify({ ...rest, parameters });
}

/** Short description of how `entry` differs from the current configuration. */
export function describeChanges(entry: ConfigHistoryEntry, current: ModelConfigSet | null): string {
  if (!current) {
    return "—";
  }
  const now = new Map(current.models.map((model) => [model.model_name, comparable(model)]));
  const then = new Map(entry.config.models.map((model) => [model.model_name, comparable(model)]));
  const parts: string[] = [];
  const added = [...then.keys()].filter((name) => !now.has(name));
  const removed = [...now.keys()].filter((name) => !then.has(name));
  const changed = [...then.keys()].filter((name) => now.has(name) && now.get(name) !== then.get(name));
  if (added.length) parts.push(`+${added.join(", ")}`);
  if (removed.length) parts.push(`−${removed.join(", ")}`);
  if (changed.length) parts.push(`~${changed.join(", ")}`);
  return parts.length ? parts.join("; ") : "same models";
}

function sourceLabel(entry: ConfigHistoryEntry): string {
  if (entry.source === "rollback" && entry.rolled_back_from !== null) {
    return `rollback of #${entry.rolled_back_from}`;
  }
  return entry.source;
}

/**
 * Stored configurations, newest first, each with a rollback. Loaded on demand
 * rather than polled: it reloads when the current version changes.
 */
export function ConfigHistory({ config, onRolledBack }: ConfigHistoryProps) {
  const [entries, setEntries] = useState<ConfigHistoryEntry[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [busyId, setBusyId] = useState<number | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const version = config?.version;

  const load = useCallback(async (after: string | null, signal?: AbortSignal) => {
    try {
      const page = await listConfigHistory(
        { limit: PAGE_SIZE, cursor: after ?? undefined },
        signal,
      );
      setEntries((current) => (after ? [...current, ...page.items] : page.items));
      setCursor(page.next_cursor);
      setLoadError("");
    } catch (cause) {
      if (signal?.aborted) return;
      setLoadError(cause instanceof Error ? cause.message : "unknown error");
    } finally {
      if (!signal?.aborted) setLoaded(true);
    }
  }, []);

  useEffect(() => {
    if (version === undefined) return;
    const controller = new AbortController();
    void load(null, controller.signal);
    return () => controller.abort();
  }, [load, version]);

  const rollback = async (entry: ConfigHistoryEntry) => {
    const label = nextVersion(config?.version ?? entry.version);
    const missing = modelsWithoutCredential(entry);
    const warning = missing.length
      ? `\n\nNo stored key or api_key_env for: ${missing.join(", ")}. They will reach daemons without a credential.`
      : "";
    if (
      !window.confirm(
        `Roll back to ${entry.version} (#${entry.id}) as ${label}? Every connected daemon gets it.${warning}`,
      )
    ) {
      return;
    }
    setBusyId(entry.id);
    setMessage(null);
    try {
      const stored = await rollbackConfig(entry.id, label);
      setMessage({ ok: true, text: `Rolled back to ${entry.version}, pushed as ${stored.version}.` });
      onRolledBack();
    } catch (cause) {
      setMessage({
        ok: false,
        text: `Rollback failed: ${cause instanceof Error ? cause.message : "unknown error"}`,
      });
    } finally {
      setBusyId(null);
    }
  };

  return (
    <Card title="Configuration history" hint="Latest 100 kept; keys are never stored in history">
      {loadError ? (
        <p role="alert" className="mb-3 text-sm text-rose-400">
          Could not load history: {loadError}
        </p>
      ) : null}
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
            {entries.map((entry, index) => {
              const isCurrent = index === 0 && entry.version === config?.version;
              return (
                <tr key={entry.id} className="hover:bg-slate-800/40">
                  <th scope="row" className={`${CELL} text-left font-mono text-xs text-slate-100`}>
                    {entry.version}
                    <span className="ml-2 text-slate-500">#{entry.id}</span>
                  </th>
                  <td className={`${CELL} text-slate-400`}>{formatClock(entry.created_at)}</td>
                  <td className={`${CELL} text-slate-400`}>{sourceLabel(entry)}</td>
                  <td className={`${CELL} font-mono text-xs text-slate-300`}>
                    {entry.config.models.map((model) => model.model_name).join(", ")}
                  </td>
                  <td className={`${CELL} font-mono text-xs text-slate-400`}>
                    {isCurrent ? "current" : describeChanges(entry, config)}
                  </td>
                  <td className={CELL}>
                    {isCurrent ? null : (
                      <button
                        type="button"
                        aria-label={`Roll back to ${entry.version} (#${entry.id})`}
                        disabled={busyId !== null || !config}
                        onClick={() => void rollback(entry)}
                        className="text-sky-300 hover:underline disabled:opacity-50"
                      >
                        {busyId === entry.id ? "Rolling back…" : "Roll back"}
                      </button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <EmptyState visible={loaded && !loadError && entries.length === 0}>
        No configuration history yet.
      </EmptyState>
      <div className="mt-3 flex flex-wrap items-center gap-3 text-sm">
        {cursor ? (
          <button
            type="button"
            className="rounded-md border border-slate-700 bg-slate-800 px-3 py-1 text-slate-100 hover:border-sky-500"
            onClick={() => void load(cursor)}
          >
            Load older
          </button>
        ) : null}
        <span
          role="status"
          aria-live="polite"
          className={message?.ok === false ? "text-rose-400" : "text-emerald-400"}
        >
          {message?.text ?? ""}
        </span>
      </div>
    </Card>
  );
}
