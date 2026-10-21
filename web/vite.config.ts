import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { parse } from "smol-toml";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asRecord(value: unknown, name: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new Error(`configuration section "${name}" must be a table`);
  }
  return value;
}

function asString(value: unknown, name: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`configuration value "${name}" must be a non-empty string`);
  }
  return value;
}

const configPath = resolve(
  process.env.LLMCP_CONFIG ?? fileURLToPath(new URL("../config.toml", import.meta.url)),
);
const config = asRecord(parse(readFileSync(configPath, "utf8")), "root");
const controlPlane = asRecord(config.control_plane, "control_plane");
const web = asRecord(config.web, "web");
const controlPlaneWebSocketUrl = new URL(
  asString(controlPlane.websocket_url, "control_plane.websocket_url"),
);
if (controlPlaneWebSocketUrl.protocol === "ws:") {
  controlPlaneWebSocketUrl.protocol = "http:";
} else if (controlPlaneWebSocketUrl.protocol === "wss:") {
  controlPlaneWebSocketUrl.protocol = "https:";
} else {
  throw new Error('configuration value "control_plane.websocket_url" must use ws:// or wss://');
}
controlPlaneWebSocketUrl.pathname = "/";
controlPlaneWebSocketUrl.search = "";
controlPlaneWebSocketUrl.hash = "";

const webAddress = new URL(`http://${asString(web.addr, "web.addr")}`);
if (!webAddress.port) {
  throw new Error('configuration value "web.addr" must include a port');
}

// The built `dist/` directory is embedded into the control-plane binary by
// rust-embed, so it must stay a plain static bundle (relative asset paths).
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: "/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    host: webAddress.hostname,
    port: Number(webAddress.port),
    strictPort: true,
    proxy: {
      "/api": {
        target: controlPlaneWebSocketUrl.origin,
      },
    },
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
