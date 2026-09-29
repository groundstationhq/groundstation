export type Order = "oldest" | "newest";

/** Two-state sort control. Reads "newest first" / "oldest first"; click flips it. */
export function SortToggle({ order, onChange, className }: { order: Order; onChange: (o: Order) => void; className?: string }) {
  const next: Order = order === "oldest" ? "newest" : "oldest";
  return (
    <button
      type="button"
      onClick={() => onChange(next)}
      aria-label={`Sorted ${order} first. Switch to ${next} first.`}
      className={`mono flex items-center gap-1.5 rounded-[4px] border border-line px-2 py-1 text-[11px] text-fg-2 transition-colors hover:border-line-3 hover:text-fg ${className ?? ""}`}
    >
      <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" aria-hidden>
        {order === "oldest" ? <path d="M5 1.5v7M2.5 6l2.5 2.5L7.5 6" /> : <path d="M5 8.5v-7M2.5 4l2.5-2.5L7.5 4" />}
      </svg>
      {order === "oldest" ? "oldest first" : "newest first"}
    </button>
  );
}
