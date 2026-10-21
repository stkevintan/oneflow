import { useEffect, useMemo, useState } from "react";

import { pushConfig } from "../api/client";
import type { ConfigUpdate } from "../api/client";
import { Card } from "./Card";

interface ConfigEditorProps {
  /** Latest configuration reported by the control plane. */
  config: ConfigUpdate | null;
  onPushed: () => void;
}

type MessageState = "idle" | "ok" | "error";

function isConfigUpdate(value: unknown): value is ConfigUpdate {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Partial<ConfigUpdate>;
  return typeof candidate.version === "string" && Array.isArray(candidate.models);
}

export function ConfigEditor({ config, onPushed }: ConfigEditorProps) {
  const [draft, setDraft] = useState("");
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [messageState, setMessageState] = useState<MessageState>("idle");

  const serialized = useMemo(() => (config ? JSON.stringify(config, null, 2) : ""), [config]);

  // Seed the editor from the server until the operator starts editing, so
  // background polling never clobbers in-progress changes.
  useEffect(() => {
    if (!dirty) {
      setDraft(serialized);
    }
  }, [dirty, serialized]);

  const handlePush = async () => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(draft);
    } catch (cause) {
      setMessageState("error");
      setMessage(`Invalid JSON: ${cause instanceof Error ? cause.message : "parse error"}`);
      return;
    }

    if (!isConfigUpdate(parsed)) {
      setMessageState("error");
      setMessage('Expected an object with a "version" string and a "models" array.');
      return;
    }

    setBusy(true);
    setMessageState("idle");
    setMessage("Pushing configuration…");

    try {
      await pushConfig(parsed);
      setDirty(false);
      setMessageState("ok");
      setMessage("Configuration pushed to all connected daemons.");
      onPushed();
    } catch (cause) {
      setMessageState("error");
      setMessage(`Push failed: ${cause instanceof Error ? cause.message : "unknown error"}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card
      title="Model configuration"
      badge={
        <span className="rounded-full border border-slate-700 bg-slate-800 px-2 py-0.5 text-xs font-medium normal-case tracking-normal text-slate-400">
          {config?.version ?? "—"}
        </span>
      }
      hint="Pushing replaces the configuration on every connected daemon"
    >
      <textarea
        aria-label="Model configuration JSON"
        spellCheck={false}
        rows={18}
        value={draft}
        onChange={(event) => {
          setDraft(event.target.value);
          setDirty(true);
          setMessageState("idle");
          setMessage("Unsaved local changes.");
        }}
        className="min-h-[260px] w-full resize-y rounded-lg border border-slate-800 bg-slate-950/80 p-3 font-mono text-xs leading-relaxed text-slate-100 focus:border-sky-500 focus:outline-none"
      />
      <div className="mt-4 flex flex-wrap items-center gap-3">
        <button
          type="button"
          onClick={handlePush}
          disabled={busy}
          className="rounded-md bg-sky-500 px-3.5 py-1.5 text-sm font-semibold text-slate-950 transition hover:bg-sky-400 disabled:cursor-not-allowed disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-400"
        >
          {busy ? "Pushing…" : "Push configuration"}
        </button>
        <button
          type="button"
          onClick={() => {
            setDraft(serialized);
            setDirty(false);
            setMessageState("ok");
            setMessage("Reloaded the configuration from the server.");
          }}
          className="rounded-md border border-slate-700 bg-slate-800 px-3.5 py-1.5 text-sm text-slate-100 transition hover:border-sky-500 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-sky-500"
        >
          Reload from server
        </button>
        <span
          role="status"
          aria-live="polite"
          className={
            messageState === "ok"
              ? "text-sm text-emerald-400"
              : messageState === "error"
                ? "text-sm text-rose-400"
                : "text-sm text-slate-400"
          }
        >
          {message}
        </span>
      </div>
    </Card>
  );
}
