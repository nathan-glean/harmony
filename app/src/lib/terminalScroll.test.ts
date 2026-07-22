import { describe, it, expect } from "vitest";
import { isAtBottom, shouldStickToBottom, shouldShowJumpToLatest } from "./terminalScroll";

describe("isAtBottom", () => {
  it("is true when the viewport has caught up to the latest line", () => {
    expect(isAtBottom({ baseY: 500, viewportY: 500 })).toBe(true);
  });

  it("is false when scrolled up into history", () => {
    expect(isAtBottom({ baseY: 500, viewportY: 480 })).toBe(false);
  });

  it("honours a tolerance for sub-line rounding", () => {
    expect(isAtBottom({ baseY: 500, viewportY: 499 }, 1)).toBe(true);
    expect(isAtBottom({ baseY: 500, viewportY: 498 }, 1)).toBe(false);
  });

  it("treats an empty (unscrolled) buffer as at bottom", () => {
    expect(isAtBottom({ baseY: 0, viewportY: 0 })).toBe(true);
  });
});

describe("shouldStickToBottom", () => {
  it("sticks new output only when already at the bottom", () => {
    expect(shouldStickToBottom({ baseY: 500, viewportY: 500 })).toBe(true);
    expect(shouldStickToBottom({ baseY: 500, viewportY: 400 })).toBe(false);
  });
});

describe("shouldShowJumpToLatest", () => {
  it("shows the button exactly when scrolled up (inverse of at-bottom)", () => {
    expect(shouldShowJumpToLatest({ baseY: 500, viewportY: 500 })).toBe(false);
    expect(shouldShowJumpToLatest({ baseY: 500, viewportY: 400 })).toBe(true);
  });
});
