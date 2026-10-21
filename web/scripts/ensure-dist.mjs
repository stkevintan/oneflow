// `vite build` clears `dist/` (emptyOutDir), which also deletes the marker file
// that keeps the rust-embed source folder present in a fresh checkout. Recreate
// it so a build never leaves the working tree dirty.
import { writeFileSync } from "node:fs";

writeFileSync(new URL("../dist/.gitkeep", import.meta.url), "");
