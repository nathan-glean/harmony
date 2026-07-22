import { describe, it, expect } from "vitest";
import {
  parseActivity,
  parseProofArtifacts,
  COLUMNS,
  COLUMN_LABELS,
} from "./types";

describe("parseActivity", () => {
  it("parses a valid activity JSON", () => {
    const a = parseActivity('{"category":"working","label":"Implementing…","detail":null}');
    expect(a).toEqual({ category: "working", label: "Implementing…", detail: null });
  });

  it("returns null for empty or unparseable input", () => {
    expect(parseActivity("")).toBeNull();
    expect(parseActivity("not json")).toBeNull();
  });
});

describe("parseProofArtifacts", () => {
  it("parses a JSON array of artifacts", () => {
    const arts = parseProofArtifacts(
      '[{"kind":"image","path":"/x/a.png","caption":"a","url":""},{"kind":"video","path":"/x/d.mp4","caption":"demo","url":"https://h/d.mp4"}]',
    );
    expect(arts).toHaveLength(2);
    expect(arts[0].kind).toBe("image");
    expect(arts[1].url).toBe("https://h/d.mp4");
  });

  it("returns [] for empty, unparseable, or non-array input", () => {
    expect(parseProofArtifacts("")).toEqual([]);
    expect(parseProofArtifacts("{oops")).toEqual([]);
    expect(parseProofArtifacts('{"not":"an array"}')).toEqual([]);
  });
});

describe("board columns", () => {
  it("has a label for every column, in lifecycle order", () => {
    expect(COLUMNS[0]).toBe("todo");
    expect(COLUMNS[COLUMNS.length - 1]).toBe("done");
    for (const c of COLUMNS) {
      expect(COLUMN_LABELS[c]).toBeTruthy();
    }
  });
});
