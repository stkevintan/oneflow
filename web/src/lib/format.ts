export function formatUptime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) {
    return "0s";
  }
  if (seconds < 60) {
    return `${Math.floor(seconds)}s`;
  }
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)}m`;
  }
  if (seconds < 86400) {
    return `${Math.floor(seconds / 3600)}h`;
  }
  return `${Math.floor(seconds / 86400)}d`;
}

export function formatClock(seconds: number): string {
  if (!seconds) {
    return "—";
  }
  return new Date(seconds * 1000).toLocaleTimeString();
}

export function formatRelative(seconds: number): string {
  if (!seconds) {
    return "—";
  }
  const delta = Math.max(0, Math.floor(Date.now() / 1000) - seconds);
  if (delta < 60) {
    return `${delta}s ago`;
  }
  if (delta < 3600) {
    return `${Math.floor(delta / 60)}m ago`;
  }
  return `${Math.floor(delta / 3600)}h ago`;
}

export function formatCount(value: number): string {
  return value.toLocaleString();
}

export function formatLatency(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) {
    return "—";
  }
  if (milliseconds < 1000) {
    return `${Math.round(milliseconds)} ms`;
  }
  return `${(milliseconds / 1000).toFixed(milliseconds < 10_000 ? 2 : 1)} s`;
}
