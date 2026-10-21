interface ImportMetaEnv {
  /**
   * Control-plane origin, for example `https://cp.example.com`. Leave unset to
   * call the same origin, which the dev and preview servers proxy to the
   * control plane.
   */
  readonly VITE_CONTROL_PLANE_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
