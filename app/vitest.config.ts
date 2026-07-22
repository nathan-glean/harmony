import { defineConfig } from "vitest/config";

// Unit tests for the frontend's pure logic (data parsing/derivation). Node environment — these
// helpers don't touch the DOM or Tauri IPC. Run via `npm test` (→ `task app:test` → `task ci`).
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
