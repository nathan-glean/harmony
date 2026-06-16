/** Ghost/shimmer placeholder shown while a panel loads for the first time. */
export function Skeleton({ lines = 3 }: { lines?: number }) {
  return (
    <div className="skeleton" aria-busy="true" aria-label="Loading…">
      {Array.from({ length: lines }).map((_, i) => (
        <div
          key={i}
          className="skeleton-line"
          // Vary the last line's width so it reads like real text.
          style={i === lines - 1 ? { width: "60%" } : undefined}
        />
      ))}
    </div>
  );
}
