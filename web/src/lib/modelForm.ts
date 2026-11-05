import type { ModelConfig } from "../api/client";

/** Parameters the form edits with dedicated fields rather than as free-form rows. */
export const RESERVED_PARAMETERS = ["upstream_model", "api_key", "api_key_env"] as const;

/**
 * What to do with a literal upstream `api_key` on push. The control plane never
 * returns the stored key, so "keep" is expressed by omitting it (see
 * `PUT /v1/config` in contract/openapi.yaml).
 */
export type ApiKeyAction = "keep" | "replace" | "remove";

export interface ParameterRow {
  key: string;
  value: string;
}

export interface ModelDraft {
  model_name: string;
  provider: string;
  api_base: string;
  upstream_model: string;
  api_key_env: string;
  /** Whether the control plane reports a stored literal key for `originalName`. */
  api_key_configured: boolean;
  api_key_action: ApiKeyAction;
  /** New literal key; only sent when `api_key_action` is "replace". */
  api_key: string;
  extra: ParameterRow[];
  /** Name the model had on the server, or null for a new model. */
  originalName: string | null;
}

export const DEFAULT_PROVIDER = "custom_openai";

export function emptyDraft(): ModelDraft {
  return {
    model_name: "",
    provider: DEFAULT_PROVIDER,
    api_base: "",
    upstream_model: "",
    api_key_env: "",
    api_key_configured: false,
    api_key_action: "keep",
    api_key: "",
    extra: [],
    originalName: null,
  };
}

export function draftFromModel(model: ModelConfig): ModelDraft {
  const parameters = model.parameters ?? {};
  return {
    model_name: model.model_name,
    provider: model.provider,
    api_base: model.api_base,
    upstream_model: parameters.upstream_model ?? "",
    api_key_env: parameters.api_key_env ?? "",
    api_key_configured: model.api_key_configured === true,
    api_key_action: "keep",
    api_key: "",
    extra: Object.entries(parameters)
      .filter(([key]) => !(RESERVED_PARAMETERS as readonly string[]).includes(key))
      .map(([key, value]) => ({ key, value })),
    originalName: model.model_name,
  };
}

/**
 * The stored key belongs to `originalName`; the control plane matches keys by
 * `model_name`, so a renamed model would silently lose it.
 */
export function renameDropsStoredKey(draft: ModelDraft): boolean {
  return (
    draft.api_key_configured &&
    draft.api_key_action === "keep" &&
    draft.originalName !== null &&
    draft.originalName !== draft.model_name.trim()
  );
}

/** Validation errors keyed by field, empty when the draft can be saved. */
export function validateDraft(draft: ModelDraft, otherNames: string[]): Record<string, string> {
  const errors: Record<string, string> = {};
  const name = draft.model_name.trim();
  if (name === "") {
    errors.model_name = "Model name is required.";
  } else if (otherNames.includes(name)) {
    errors.model_name = `Another model is already named "${name}".`;
  }
  if (draft.provider.trim() === "") {
    errors.provider = "Provider is required.";
  }
  const base = draft.api_base.trim();
  if (base === "") {
    errors.api_base = "API base URL is required.";
  } else {
    try {
      const url = new URL(base);
      if (url.protocol !== "http:" && url.protocol !== "https:") {
        errors.api_base = "API base URL must use http or https.";
      }
    } catch {
      errors.api_base = "API base URL must be an absolute URL, for example https://api.example.com/v1.";
    }
  }
  if (draft.api_key_action === "replace" && draft.api_key.trim() === "") {
    errors.api_key = "Enter the new API key, or choose to keep or remove the stored one.";
  }
  const seen = new Set<string>();
  draft.extra.forEach((row, index) => {
    const key = row.key.trim();
    if (key === "" && row.value === "") {
      return;
    }
    if (key === "") {
      errors[`extra.${index}`] = "Parameter name is required.";
    } else if ((RESERVED_PARAMETERS as readonly string[]).includes(key)) {
      errors[`extra.${index}`] = `"${key}" has its own field above.`;
    } else if (seen.has(key)) {
      errors[`extra.${index}`] = `"${key}" is listed twice.`;
    }
    seen.add(key);
  });
  if (renameDropsStoredKey(draft)) {
    errors.api_key =
      "Renaming drops the stored API key. Enter it again, or remove it to use api_key_env.";
  }
  return errors;
}

/** The `ModelConfig` sent to `PUT /v1/config` for a validated draft. */
export function modelFromDraft(draft: ModelDraft): ModelConfig {
  const parameters: Record<string, string> = {};
  for (const row of draft.extra) {
    if (row.key.trim() !== "") {
      parameters[row.key.trim()] = row.value;
    }
  }
  if (draft.upstream_model.trim() !== "") {
    parameters.upstream_model = draft.upstream_model.trim();
  }
  if (draft.api_key_env.trim() !== "") {
    parameters.api_key_env = draft.api_key_env.trim();
  }
  if (draft.api_key_action === "replace") {
    parameters.api_key = draft.api_key.trim();
  } else if (draft.api_key_action === "remove" && draft.api_key_configured) {
    parameters.api_key = "";
  }
  return {
    model_name: draft.model_name.trim(),
    provider: draft.provider.trim(),
    api_base: draft.api_base.trim(),
    parameters,
  };
}

/**
 * A model as sent back unchanged: `api_key_configured` is response-only and the
 * stored key is kept by omitting `api_key`.
 */
export function unchangedModel(model: ModelConfig): ModelConfig {
  return {
    model_name: model.model_name,
    provider: model.provider,
    api_base: model.api_base,
    parameters: { ...model.parameters },
  };
}

/** How a model authenticates upstream, for the model list. */
export function credentialSummary(model: ModelConfig): string {
  const env = model.parameters?.api_key_env;
  if (env) {
    return `env ${env}`;
  }
  if (model.api_key_configured) {
    return "stored key";
  }
  return "none";
}

/** Suggested label for the next push: bumps a trailing number, else appends "-2". */
export function nextVersion(version: string): string {
  const match = /^(.*?)(\d+)$/.exec(version);
  if (match) {
    return `${match[1]}${Number(match[2]) + 1}`;
  }
  return version === "" ? "v1" : `${version}-2`;
}
