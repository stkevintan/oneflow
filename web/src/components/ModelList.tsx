import { useState } from "react";

import { putConfig } from "../api/client";
import type { ModelConfig, ModelConfigSet } from "../api/client";
import type { ModelDraft } from "../lib/modelForm";
import {
  credentialSummary,
  draftFromModel,
  emptyDraft,
  modelFromDraft,
  nextVersion,
  unchangedModel,
} from "../lib/modelForm";
import { Card, EmptyState } from "./Card";
import { ConfigEditor } from "./ConfigEditor";
import { ModelForm } from "./ModelForm";

interface ModelListProps {
  config: ModelConfigSet | null;
  onPushed: () => void;
}

/** null: nothing open; -1: adding; otherwise the index being edited. */
type Editing = number | null;

const CELL = "whitespace-nowrap border-b border-slate-800/70 px-3 py-2";
const HEADERS = ["Model", "Provider", "API base", "Upstream model", "Credential", "Actions"];

/**
 * The model table plus add/edit/delete. Every change is pushed as a whole
 * configuration (`PUT /v1/config`) with a new version label, since that is the
 * only write the control plane offers.
 */
export function ModelList({ config, onPushed }: ModelListProps) {
  const [editing, setEditing] = useState<Editing>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [advanced, setAdvanced] = useState(false);

  const models = config?.models ?? [];
  const nextLabel = version ?? nextVersion(config?.version ?? "");

  const push = async (nextModels: ModelConfig[], success: string): Promise<boolean> => {
    if (!config) {
      return false;
    }
    if (nextLabel.trim() === "") {
      setMessage({ ok: false, text: "Enter a version label for the push." });
      return false;
    }
    setBusy(true);
    setMessage(null);
    try {
      await putConfig({ version: nextLabel.trim(), models: nextModels });
      setMessage({ ok: true, text: `${success} Pushed as ${nextLabel.trim()}.` });
      setVersion(null);
      onPushed();
      return true;
    } catch (cause) {
      setMessage({
        ok: false,
        text: `Push failed: ${cause instanceof Error ? cause.message : "unknown error"}`,
      });
      return false;
    } finally {
      setBusy(false);
    }
  };

  const save = async (draft: ModelDraft) => {
    const updated = modelFromDraft(draft);
    const nextModels =
      editing === -1
        ? [...models.map(unchangedModel), updated]
        : models.map((model, index) => (index === editing ? updated : unchangedModel(model)));
    const verb = editing === -1 ? "Added" : "Saved";
    if (await push(nextModels, `${verb} ${updated.model_name}.`)) {
      setEditing(null);
    }
  };

  const remove = async (index: number) => {
    const name = models[index].model_name;
    if (!window.confirm(`Delete ${name}? Daemons stop serving it as soon as they apply the push.`)) {
      return;
    }
    await push(
      models.filter((_, i) => i !== index).map(unchangedModel),
      `Deleted ${name}.`,
    );
  };

  const otherNames = models
    .filter((_, index) => index !== editing)
    .map((model) => model.model_name);

  return (
    <Card
      title="Models"
      badge={
        <span className="rounded-full border border-slate-700 bg-slate-800 px-2 py-0.5 text-xs font-medium normal-case tracking-normal text-slate-400">
          {config?.version ?? "—"}
        </span>
      }
      hint="Each change pushes the whole configuration to every connected daemon"
    >
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
            {models.map((model, index) => (
              <tr key={model.model_name} className="hover:bg-slate-800/40">
                <th scope="row" className={`${CELL} text-left font-mono text-xs text-slate-100`}>
                  {model.model_name}
                </th>
                <td className={`${CELL} text-slate-300`}>{model.provider}</td>
                <td className={`${CELL} font-mono text-xs text-slate-400`}>{model.api_base}</td>
                <td className={`${CELL} font-mono text-xs text-slate-400`}>
                  {model.parameters?.upstream_model ?? "—"}
                </td>
                <td className={`${CELL} text-slate-300`}>{credentialSummary(model)}</td>
                <td className={`${CELL} flex gap-3`}>
                  <button
                    type="button"
                    className="text-sky-300 hover:underline disabled:opacity-50"
                    aria-label={`Edit ${model.model_name}`}
                    disabled={busy || editing !== null}
                    onClick={() => {
                      setMessage(null);
                      setEditing(index);
                    }}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    className="text-rose-300 hover:underline disabled:opacity-50"
                    aria-label={`Delete ${model.model_name}`}
                    disabled={busy || editing !== null}
                    onClick={() => void remove(index)}
                  >
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <EmptyState visible={config !== null && models.length === 0}>
        No models configured. Daemons reject every request until one is added.
      </EmptyState>

      <div className="mt-4 flex flex-wrap items-center gap-3 text-sm">
        <button
          type="button"
          disabled={!config || busy || editing !== null}
          onClick={() => {
            setMessage(null);
            setEditing(-1);
          }}
          className="rounded-md border border-slate-700 bg-slate-800 px-3.5 py-1.5 text-slate-100 hover:border-sky-500 disabled:cursor-not-allowed disabled:opacity-60"
        >
          Add model
        </button>
        <label htmlFor="next-config-version" className="text-slate-400">
          Next version
        </label>
        <input
          id="next-config-version"
          className="w-40 rounded-md border border-slate-700 bg-slate-950/80 px-2 py-1 font-mono text-xs text-slate-100 focus:border-sky-500 focus:outline-none"
          value={nextLabel}
          onChange={(event) => setVersion(event.target.value)}
        />
        <span
          role="status"
          aria-live="polite"
          className={message?.ok === false ? "text-rose-400" : "text-emerald-400"}
        >
          {message?.text ?? ""}
        </span>
      </div>

      {editing !== null ? (
        <ModelForm
          key={editing}
          initial={editing === -1 ? emptyDraft() : draftFromModel(models[editing])}
          otherNames={otherNames}
          busy={busy}
          onSave={(draft) => void save(draft)}
          onCancel={() => setEditing(null)}
        />
      ) : null}

      <div className="mt-5 border-t border-slate-800 pt-3">
        <button
          type="button"
          aria-expanded={advanced}
          aria-controls="raw-config-editor"
          className="text-xs text-slate-400 hover:text-slate-200"
          onClick={() => setAdvanced((open) => !open)}
        >
          {advanced ? "▾" : "▸"} Edit raw JSON
        </button>
        {advanced ? (
          <div id="raw-config-editor">
            <ConfigEditor config={config} onPushed={onPushed} embedded />
          </div>
        ) : null}
      </div>
    </Card>
  );
}
