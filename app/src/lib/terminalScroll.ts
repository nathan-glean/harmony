// Pure scroll-position logic for the live terminal, kept out of the React component so it can be
// unit-tested without a DOM or xterm instance. xterm exposes the viewport as three numbers:
//   - baseY:   the top line of the bottom-most (latest) scroll position
//   - viewportY: the top line currently shown
// The view is "at the bottom" when viewportY has caught up to baseY.

/** A minimal snapshot of xterm's scroll state — the fields we reason about. */
export type ScrollState = {
  /** Top line of the latest (bottom-most) viewport position — `buffer.active.baseY`. */
  baseY: number;
  /** Top line currently displayed — `buffer.active.viewportY`. */
  viewportY: number;
};

/**
 * Is the viewport parked at (or within `tolerance` lines of) the bottom? A small tolerance
 * absorbs sub-line rounding so the terminal still counts as "at bottom" mid-render.
 */
export function isAtBottom({ baseY, viewportY }: ScrollState, tolerance = 0): boolean {
  return baseY - viewportY <= tolerance;
}

/**
 * Should freshly-arrived output pull the view down to the latest line? Only when the user was
 * already at the bottom — otherwise we'd yank the view out from under someone reading history.
 */
export function shouldStickToBottom(state: ScrollState, tolerance = 0): boolean {
  return isAtBottom(state, tolerance);
}

/**
 * Should the "jump to latest" affordance be visible? Exactly when the user has scrolled up far
 * enough that new output would no longer auto-stick — the inverse of {@link isAtBottom}.
 */
export function shouldShowJumpToLatest(state: ScrollState, tolerance = 0): boolean {
  return !isAtBottom(state, tolerance);
}
