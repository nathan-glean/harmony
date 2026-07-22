import { describe, it, expect } from "vitest";
// `@types/node` isn't a dependency of this frontend package, but Vitest runs this file in a
// `node` environment where the builtin exists at runtime. Suppress the missing-types error
// rather than pulling in a new dependency just to read one file.
// @ts-ignore -- no @types/node; builtin resolved at runtime in the node test env
import { readFileSync } from "node:fs";

// Regression guard: the shared content region must keep a window-level bottom gap so content
// never sits flush against the window's bottom edge. Reads the CSS from disk (node env, no DOM).
// Node's readFileSync accepts a file: URL directly, so no path conversion is needed.
const css: string = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

describe("styles.css", () => {
  it("gives the .main content region a bottom padding", () => {
    // Match the exact `.main` rule, not `.main`-prefixed classes like `.main-foo`.
    const match = css.match(/\.main\s*\{([^}]*)\}/);
    expect(match).not.toBeNull();
    expect(match![1]).toContain("padding-bottom: 16px");
  });
});
