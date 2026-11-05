import { useCallback, useEffect, useRef, useState } from "react";

import { getConfig, getStatus, listDaemons, listDaemonCredentials, listUsageEvents } from "./client";
import type { Daemon, DaemonCredential, ModelConfigSet, StatusResponse, UsageEvent } from "./client";

export type ConnectionState = "connecting" | "live" | "offline";

/** Number of usage events the dashboard shows. */
export const RECENT_USAGE_LIMIT = 25;

export interface DashboardSnapshot {
  status: StatusResponse;
  daemons: Daemon[];
  credentials: DaemonCredential[];
  /** Newest first. */
  usage: UsageEvent[];
  config: ModelConfigSet;
}

/** Narrows the usage events the dashboard shows; empty strings mean "all". */
export interface UsageFilter {
  daemon_id: string;
  model_name: string;
}

export const NO_USAGE_FILTER: UsageFilter = { daemon_id: "", model_name: "" };

export interface DashboardState {
  snapshot: DashboardSnapshot | null;
  error: string | null;
  connection: ConnectionState;
  refresh: () => Promise<void>;
}

async function fetchSnapshot(
  signal: AbortSignal,
  filter: UsageFilter,
): Promise<DashboardSnapshot> {
  const [status, daemons, usage, config, credentials] = await Promise.all([
    getStatus(signal),
    listDaemons(signal),
    listUsageEvents({ limit: RECENT_USAGE_LIMIT, ...filter }, signal),
    getConfig(signal),
    listDaemonCredentials(signal),
  ]);
  return { status, daemons: daemons.items, usage: usage.items, config,
    credentials: credentials.items };
}

/**
 * Polls the `/v1` resources the dashboard renders and exposes the latest
 * snapshot. Polling is paused while the tab is hidden to avoid pointless
 * traffic.
 */
export function useDashboard(
  pollIntervalMs = 3000,
  usageFilter: UsageFilter = NO_USAGE_FILTER,
): DashboardState {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connection, setConnection] = useState<ConnectionState>("connecting");
  const controllerRef = useRef<AbortController | null>(null);
  // Depend on the values, not the object, so callers may pass a fresh literal.
  const { daemon_id, model_name } = usageFilter;

  const refresh = useCallback(async () => {
    controllerRef.current?.abort();
    const controller = new AbortController();
    controllerRef.current = controller;

    try {
      const next = await fetchSnapshot(controller.signal, { daemon_id, model_name });
      setSnapshot(next);
      setError(null);
      setConnection("live");
    } catch (cause) {
      if (controller.signal.aborted) {
        return;
      }
      setError(cause instanceof Error ? cause.message : "unknown error");
      setConnection("offline");
    }
  }, [daemon_id, model_name]);

  useEffect(() => {
    let timer: number | undefined;

    const start = () => {
      void refresh();
      timer = window.setInterval(() => void refresh(), pollIntervalMs);
    };
    const stop = () => {
      if (timer !== undefined) {
        window.clearInterval(timer);
        timer = undefined;
      }
    };
    const handleVisibility = () => {
      stop();
      if (!document.hidden) {
        start();
      }
    };

    start();
    document.addEventListener("visibilitychange", handleVisibility);

    return () => {
      stop();
      document.removeEventListener("visibilitychange", handleVisibility);
      controllerRef.current?.abort();
    };
  }, [pollIntervalMs, refresh]);

  return { snapshot, error, connection, refresh };
}
