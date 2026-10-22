import { useCallback, useEffect, useRef, useState } from "react";

import { ApiError, getConfig, getStatus, listDaemons, listDaemonCredentials, listUsageEvents } from "./client";
import type { Daemon, DaemonCredential, ModelConfigSet, StatusResponse, UsageEvent } from "./client";

export type ConnectionState = "connecting" | "live" | "offline";

/** Number of usage events the dashboard shows. */
export const RECENT_USAGE_LIMIT = 25;

export interface DashboardSnapshot {
  status: StatusResponse;
  daemons: Daemon[];
  credentials: DaemonCredential[];
  credentialError: string | null;
  /** Newest first. */
  usage: UsageEvent[];
  config: ModelConfigSet;
}

export interface DashboardState {
  snapshot: DashboardSnapshot | null;
  error: string | null;
  connection: ConnectionState;
  refresh: () => Promise<void>;
}

async function fetchSnapshot(signal: AbortSignal): Promise<DashboardSnapshot> {
  const [status, daemons, usage, config, credentials] = await Promise.all([
    getStatus(signal),
    listDaemons(signal),
    listUsageEvents({ limit: RECENT_USAGE_LIMIT }, signal),
    getConfig(signal),
    listDaemonCredentials(signal).then(
      (value) => ({ items: value.items, error: null }),
      (cause: unknown) => {
        if (cause instanceof ApiError && cause.status === 503) {
          return { items: [], error: cause.message };
        }
        throw cause;
      },
    ),
  ]);
  return { status, daemons: daemons.items, usage: usage.items, config,
    credentials: credentials.items, credentialError: credentials.error };
}

/**
 * Polls the `/v1` resources the dashboard renders and exposes the latest
 * snapshot. Polling is paused while the tab is hidden to avoid pointless
 * traffic.
 */
export function useDashboard(pollIntervalMs = 3000): DashboardState {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connection, setConnection] = useState<ConnectionState>("connecting");
  const controllerRef = useRef<AbortController | null>(null);

  const refresh = useCallback(async () => {
    controllerRef.current?.abort();
    const controller = new AbortController();
    controllerRef.current = controller;

    try {
      const next = await fetchSnapshot(controller.signal);
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
  }, []);

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
