import { useCallback, useEffect, useRef, useState } from "react";

import { fetchStatus } from "./client";
import type { StatusResponse } from "./client";

export type ConnectionState = "connecting" | "live" | "offline";

export interface StatusState {
  status: StatusResponse | null;
  error: string | null;
  connection: ConnectionState;
  refresh: () => Promise<void>;
}

/**
 * Polls `GET /api/status` and exposes the latest snapshot. Polling is paused
 * while the tab is hidden to avoid pointless traffic.
 */
export function useStatus(pollIntervalMs = 3000): StatusState {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connection, setConnection] = useState<ConnectionState>("connecting");
  const controllerRef = useRef<AbortController | null>(null);

  const refresh = useCallback(async () => {
    controllerRef.current?.abort();
    const controller = new AbortController();
    controllerRef.current = controller;

    try {
      const snapshot = await fetchStatus(controller.signal);
      setStatus(snapshot);
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

  return { status, error, connection, refresh };
}
