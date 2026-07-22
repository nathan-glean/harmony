import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Frontend unit tests. Default environment is Node (pure logic tests like types.test.ts don't
// touch the DOM). Component tests (*.test.tsx) opt into jsdom with a `// @vitest-environment
// jsdom` docblock at the top of the file. Run via `npm test` (→ `task app:test` → `task ci`).
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
