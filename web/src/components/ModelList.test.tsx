import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ModelConfigSet } from "../api/client";
import { nextVersion } from "../lib/modelForm";
import { ModelList } from "./ModelList";

const CONFIG: ModelConfigSet = {
  version: "prod-v3",
  models: [
    {
      model_name: "keyed",
      provider: "custom_openai",
      api_base: "https://keyed.example/v1",
      api_key_configured: true,
      parameters: { upstream_model: "keyed-up", temperature: "0.2" },
    },
    {
      model_name: "plain",
      provider: "custom_openai",
      api_base: "https://plain.example/v1",
      api_key_configured: false,
      parameters: { api_key_env: "PLAIN_KEY" },
    },
  ],
};

function setup() {
  const fetchMock = vi.fn(async (_input: string, init?: RequestInit) => ({
    ok: true,
    status: 200,
    json: async () => JSON.parse(String(init?.body)),
  }));
  vi.stubGlobal("fetch", fetchMock);
  const onPushed = vi.fn();
  render(<ModelList config={CONFIG} onPushed={onPushed} />);
  const sent = () => {
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    return call ? (JSON.parse(String(call[1]?.body)) as ModelConfigSet) : undefined;
  };
  return { fetchMock, onPushed, sent };
}

function form(name: RegExp | string) {
  return within(screen.getByRole("form", { name }));
}

describe("model list", () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("lists models with how each authenticates", () => {
    setup();
    expect(screen.getByRole("rowheader", { name: "keyed" })).toBeTruthy();
    expect(screen.getByText("stored key")).toBeTruthy();
    expect(screen.getByText("env PLAIN_KEY")).toBeTruthy();
    expect((screen.getByLabelText("Next version") as HTMLInputElement).value).toBe("prod-v4");
  });

  it("edits a model and keeps its stored key by omitting api_key", async () => {
    const { sent, onPushed } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Edit keyed" }));
    const edit = form("Edit model keyed");
    fireEvent.change(edit.getByLabelText("API base URL"), {
      target: { value: "https://moved.example/v1" },
    });
    fireEvent.click(edit.getByRole("button", { name: "Save and push" }));

    await screen.findByText("Saved keyed. Pushed as prod-v4.");
    expect(onPushed).toHaveBeenCalled();
    expect(sent()).toEqual({
      version: "prod-v4",
      models: [
        {
          model_name: "keyed",
          provider: "custom_openai",
          api_base: "https://moved.example/v1",
          parameters: { upstream_model: "keyed-up", temperature: "0.2" },
        },
        {
          model_name: "plain",
          provider: "custom_openai",
          api_base: "https://plain.example/v1",
          parameters: { api_key_env: "PLAIN_KEY" },
        },
      ],
    });
    expect(screen.queryByRole("form")).toBeNull();
  });

  it("replaces or removes a stored key", async () => {
    const { sent, fetchMock } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Edit keyed" }));
    let edit = form("Edit model keyed");
    fireEvent.click(edit.getByLabelText("Replace"));
    // An empty replacement is rejected rather than silently removing the key.
    fireEvent.click(edit.getByRole("button", { name: "Save and push" }));
    expect(await edit.findByText(/Enter the new API key/)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();

    fireEvent.change(edit.getByLabelText("New API key"), { target: { value: " sk-new " } });
    fireEvent.click(edit.getByRole("button", { name: "Save and push" }));
    await screen.findByText(/Saved keyed/);
    expect(sent()?.models[0]?.parameters?.api_key).toBe("sk-new");

    fetchMock.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Edit keyed" }));
    edit = form("Edit model keyed");
    fireEvent.click(edit.getByLabelText("Remove"));
    fireEvent.click(edit.getByRole("button", { name: "Save and push" }));
    await screen.findByText(/Saved keyed/);
    expect(sent()?.models[0]?.parameters?.api_key).toBe("");
  });

  it("validates a new model before adding it", async () => {
    const { sent, fetchMock } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Add model" }));
    const add = form("Add model");
    fireEvent.change(add.getByLabelText("Model name"), { target: { value: "plain" } });
    fireEvent.change(add.getByLabelText("API base URL"), { target: { value: "not a url" } });
    fireEvent.click(add.getByRole("button", { name: "Save and push" }));

    expect(add.getByText('Another model is already named "plain".')).toBeTruthy();
    expect(add.getByText(/must be an absolute URL/)).toBeTruthy();
    expect(add.getByLabelText("API base URL").getAttribute("aria-invalid")).toBe("true");
    expect(fetchMock).not.toHaveBeenCalled();

    // The daemon appends /chat/completions, which a fragment would swallow.
    fireEvent.change(add.getByLabelText("API base URL"), {
      target: { value: "https://fresh.example/v1#eu" },
    });
    fireEvent.click(add.getByRole("button", { name: "Save and push" }));
    expect(add.getByText(/#fragment/)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();

    fireEvent.change(add.getByLabelText("Model name"), { target: { value: "fresh" } });
    fireEvent.change(add.getByLabelText("API base URL"), {
      target: { value: "https://fresh.example/v1" },
    });
    fireEvent.change(add.getByLabelText(/API key environment variable/), {
      target: { value: "FRESH_KEY" },
    });
    fireEvent.click(add.getByRole("button", { name: "Add parameter" }));
    fireEvent.change(add.getByLabelText("Parameter 1 name"), { target: { value: "region" } });
    fireEvent.change(add.getByLabelText("Parameter 1 value"), { target: { value: "eu" } });
    fireEvent.change(screen.getByLabelText("Next version"), { target: { value: "release-7" } });
    fireEvent.click(add.getByRole("button", { name: "Save and push" }));

    await screen.findByText("Added fresh. Pushed as release-7.");
    const body = sent();
    expect(body?.version).toBe("release-7");
    expect(body?.models.map((model) => model.model_name)).toEqual(["keyed", "plain", "fresh"]);
    expect(body?.models[2]).toEqual({
      model_name: "fresh",
      provider: "custom_openai",
      api_base: "https://fresh.example/v1",
      parameters: { region: "eu", api_key_env: "FRESH_KEY" },
    });
  });

  it("does not let a rename silently drop the stored key", async () => {
    const { fetchMock } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Edit keyed" }));
    const edit = form("Edit model keyed");
    fireEvent.change(edit.getByLabelText("Model name"), { target: { value: "renamed" } });
    expect(edit.getByText(/Renaming drops the stored key/)).toBeTruthy();
    fireEvent.click(edit.getByRole("button", { name: "Save and push" }));
    expect(edit.getByText(/Enter it again, or remove it/)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("deletes a model after confirmation", async () => {
    const { sent, fetchMock } = setup();
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false);
    fireEvent.click(screen.getByRole("button", { name: "Delete plain" }));
    expect(confirm).toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();

    confirm.mockReturnValueOnce(true);
    fireEvent.click(screen.getByRole("button", { name: "Delete plain" }));
    await screen.findByText("Deleted plain. Pushed as prod-v4.");
    expect(sent()?.models.map((model) => model.model_name)).toEqual(["keyed"]);
  });

  it("shows the control plane's error when a push is rejected", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: false,
        status: 400,
        json: async () => ({ error: "api_base must not be empty" }),
      })),
    );
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<ModelList config={CONFIG} onPushed={() => undefined} />);
    fireEvent.click(screen.getByRole("button", { name: "Delete plain" }));
    expect(await screen.findByText(/Push failed: api_base must not be empty/)).toBeTruthy();
  });
});

describe("nextVersion", () => {
  it("bumps a trailing number or appends one", () => {
    expect(nextVersion("prod-v3")).toBe("prod-v4");
    expect(nextVersion("2026.11.9")).toBe("2026.11.10");
    expect(nextVersion("poc")).toBe("poc-2");
    expect(nextVersion("")).toBe("v1");
  });
});
