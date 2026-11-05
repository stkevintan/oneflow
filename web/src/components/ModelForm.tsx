import { useId, useState } from "react";

import type { ApiKeyAction, ModelDraft } from "../lib/modelForm";
import { renameDropsStoredKey, validateDraft } from "../lib/modelForm";

interface ModelFormProps {
  initial: ModelDraft;
  /** Names of the other models, to reject duplicates. */
  otherNames: string[];
  busy: boolean;
  onSave: (draft: ModelDraft) => void;
  onCancel: () => void;
}

const INPUT =
  "w-full rounded-md border border-slate-700 bg-slate-950/80 px-3 py-1.5 text-sm text-slate-100 focus:border-sky-500 focus:outline-none aria-[invalid=true]:border-rose-500";
const LABEL = "mb-1 block text-xs font-medium text-slate-300";
const HELP = "mt-1 text-xs text-slate-500";
const ERROR = "mt-1 text-xs text-rose-400";

function Field({
  id,
  label,
  help,
  error,
  children,
}: {
  id: string;
  label: string;
  help?: string;
  error?: string;
  children: (describedBy: string | undefined) => React.ReactNode;
}) {
  const describedBy = [help ? `${id}-help` : null, error ? `${id}-error` : null]
    .filter(Boolean)
    .join(" ");
  return (
    <div>
      <label htmlFor={id} className={LABEL}>
        {label}
      </label>
      {children(describedBy || undefined)}
      {help ? (
        <p id={`${id}-help`} className={HELP}>
          {help}
        </p>
      ) : null}
      {error ? (
        <p id={`${id}-error`} className={ERROR}>
          {error}
        </p>
      ) : null}
    </div>
  );
}

/** Add/edit form for one model. Validation runs on save; nothing is pushed here. */
export function ModelForm({ initial, otherNames, busy, onSave, onCancel }: ModelFormProps) {
  const id = useId();
  const [draft, setDraft] = useState<ModelDraft>(initial);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [submitted, setSubmitted] = useState(false);

  const update = (patch: Partial<ModelDraft>) => {
    const next = { ...draft, ...patch };
    setDraft(next);
    if (submitted) {
      setErrors(validateDraft(next, otherNames));
    }
  };

  const keyActions: { value: ApiKeyAction; label: string }[] = initial.api_key_configured
    ? [
        { value: "keep", label: "Keep stored key" },
        { value: "replace", label: "Replace" },
        { value: "remove", label: "Remove" },
      ]
    : [
        { value: "keep", label: "No literal key" },
        { value: "replace", label: "Set a key" },
      ];

  return (
    <form
      aria-label={initial.originalName ? `Edit model ${initial.originalName}` : "Add model"}
      noValidate
      className="mt-4 rounded-lg border border-slate-700 bg-slate-950/40 p-4"
      onSubmit={(event) => {
        event.preventDefault();
        setSubmitted(true);
        const found = validateDraft(draft, otherNames);
        setErrors(found);
        if (Object.keys(found).length === 0) {
          onSave(draft);
        }
      }}
    >
      <h3 className="mb-3 text-sm font-semibold text-slate-200">
        {initial.originalName ? `Edit ${initial.originalName}` : "Add model"}
      </h3>
      <div className="grid gap-4 md:grid-cols-2">
        <Field
          id={`${id}-name`}
          label="Model name"
          help="The id clients send to the daemon."
          error={errors.model_name}
        >
          {(describedBy) => (
            <input
              id={`${id}-name`}
              className={INPUT}
              value={draft.model_name}
              aria-invalid={errors.model_name ? true : undefined}
              aria-describedby={describedBy}
              onChange={(event) => update({ model_name: event.target.value })}
            />
          )}
        </Field>
        <Field id={`${id}-provider`} label="Provider" error={errors.provider}>
          {(describedBy) => (
            <input
              id={`${id}-provider`}
              className={INPUT}
              value={draft.provider}
              aria-invalid={errors.provider ? true : undefined}
              aria-describedby={describedBy}
              onChange={(event) => update({ provider: event.target.value })}
            />
          )}
        </Field>
        <Field
          id={`${id}-base`}
          label="API base URL"
          help="OpenAI-compatible base, for example https://api.deepseek.com/v1."
          error={errors.api_base}
        >
          {(describedBy) => (
            <input
              id={`${id}-base`}
              type="url"
              className={INPUT}
              value={draft.api_base}
              aria-invalid={errors.api_base ? true : undefined}
              aria-describedby={describedBy}
              onChange={(event) => update({ api_base: event.target.value })}
            />
          )}
        </Field>
        <Field
          id={`${id}-upstream`}
          label="Upstream model (optional)"
          help="Rewrites the model id sent upstream. Empty sends the model name."
        >
          {(describedBy) => (
            <input
              id={`${id}-upstream`}
              className={INPUT}
              value={draft.upstream_model}
              aria-describedby={describedBy}
              onChange={(event) => update({ upstream_model: event.target.value })}
            />
          )}
        </Field>
        <Field
          id={`${id}-env`}
          label="API key environment variable (recommended)"
          help="Read on the daemon host, so the key never leaves it. Takes precedence over a stored key."
        >
          {(describedBy) => (
            <input
              id={`${id}-env`}
              className={`${INPUT} font-mono`}
              placeholder="DEEPSEEK_API_KEY"
              value={draft.api_key_env}
              aria-describedby={describedBy}
              onChange={(event) => update({ api_key_env: event.target.value })}
            />
          )}
        </Field>
        <fieldset aria-describedby={`${id}-key-help${errors.api_key ? ` ${id}-key-error` : ""}`}>
          <legend className={LABEL}>
            Stored API key{" "}
            <span className="font-normal text-slate-500">
              ({initial.api_key_configured ? "one is stored" : "none stored"})
            </span>
          </legend>
          <div className="flex flex-wrap gap-3 text-sm text-slate-300">
            {keyActions.map((action) => (
              <label key={action.value} className="flex items-center gap-1.5">
                <input
                  type="radio"
                  name={`${id}-key-action`}
                  value={action.value}
                  checked={draft.api_key_action === action.value}
                  onChange={() => update({ api_key_action: action.value })}
                />
                {action.label}
              </label>
            ))}
          </div>
          {draft.api_key_action === "replace" ? (
            <input
              aria-label="New API key"
              type="password"
              autoComplete="off"
              className={`${INPUT} mt-2`}
              value={draft.api_key}
              aria-invalid={errors.api_key ? true : undefined}
              onChange={(event) => update({ api_key: event.target.value })}
            />
          ) : null}
          <p id={`${id}-key-help`} className={HELP}>
            Write-only: the control plane never shows a stored key, and sends it to daemons over
            the control channel.
          </p>
          {renameDropsStoredKey(draft) && !errors.api_key ? (
            <p className="mt-1 text-xs text-amber-300">
              Renaming drops the stored key: keys are matched by model name.
            </p>
          ) : null}
          {errors.api_key ? (
            <p id={`${id}-key-error`} className={ERROR}>
              {errors.api_key}
            </p>
          ) : null}
        </fieldset>
      </div>

      <fieldset className="mt-4">
        <legend className={LABEL}>Other parameters</legend>
        {draft.extra.length === 0 ? (
          <p className="text-xs text-slate-500">None.</p>
        ) : null}
        <ul className="flex flex-col gap-2">
          {draft.extra.map((row, index) => (
            <li key={index}>
              <div className="flex gap-2">
                <input
                  aria-label={`Parameter ${index + 1} name`}
                  className={`${INPUT} font-mono`}
                  value={row.key}
                  aria-invalid={errors[`extra.${index}`] ? true : undefined}
                  onChange={(event) =>
                    update({
                      extra: draft.extra.map((item, i) =>
                        i === index ? { ...item, key: event.target.value } : item,
                      ),
                    })
                  }
                />
                <input
                  aria-label={`Parameter ${index + 1} value`}
                  className={INPUT}
                  value={row.value}
                  onChange={(event) =>
                    update({
                      extra: draft.extra.map((item, i) =>
                        i === index ? { ...item, value: event.target.value } : item,
                      ),
                    })
                  }
                />
                <button
                  type="button"
                  aria-label={`Remove parameter ${index + 1}`}
                  className="rounded-md border border-slate-700 px-2 text-sm text-slate-300 hover:border-rose-500"
                  onClick={() => update({ extra: draft.extra.filter((_, i) => i !== index) })}
                >
                  ×
                </button>
              </div>
              {errors[`extra.${index}`] ? (
                <p className={ERROR}>{errors[`extra.${index}`]}</p>
              ) : null}
            </li>
          ))}
        </ul>
        <button
          type="button"
          className="mt-2 text-xs text-sky-300 hover:underline"
          onClick={() => update({ extra: [...draft.extra, { key: "", value: "" }] })}
        >
          Add parameter
        </button>
      </fieldset>

      {submitted && Object.keys(errors).length > 0 ? (
        <p role="alert" className="mt-3 text-sm text-rose-400">
          Fix the highlighted fields before saving.
        </p>
      ) : null}
      <div className="mt-4 flex gap-3">
        <button
          type="submit"
          disabled={busy}
          className="rounded-md bg-sky-500 px-3.5 py-1.5 text-sm font-semibold text-slate-950 transition hover:bg-sky-400 disabled:cursor-not-allowed disabled:opacity-60"
        >
          {busy ? "Saving…" : "Save and push"}
        </button>
        <button
          type="button"
          onClick={onCancel}
          className="rounded-md border border-slate-700 bg-slate-800 px-3.5 py-1.5 text-sm text-slate-100 hover:border-sky-500"
        >
          Cancel
        </button>
      </div>
    </form>
  );
}
