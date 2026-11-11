import { useRef, useState } from "react";
import { StatusDot } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { insights, setInsightState } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { ago, cx, fmtInt, shortId } from "@/lib/format";
import type { Insight, InsightState, Severity } from "@/lib/types";

export const SEVERITY: Record<Severity, { dot: "err" | "warn" | "idle"; label: string; cls: string }> = {
  high: { dot: "err", label: "High", cls: "text-err" },
  medium: { dot: "warn", label: "Medium", cls: "text-warn" },
  low: { dot: "idle", label: "Low", cls: "text-fg-3" },
};

const DETECTOR: Record<string, string> = {
  repeated_read: "Repeated reads",
  test_rerun_loop: "Full test reruns",
  tool_failure_hotspot: "Failing tool",
  slow_tool: "Slower tool",
  cache_miss: "Cache misses",
  cost_outlier: "Cost outlier",
  permission_wait: "Waiting on a person",
};

const TABS: Array<[InsightState, string]> = [
  ["open", "Open"],
  ["dismissed", "Dismissed"],
  ["resolved", "Resolved"],
];

const ACTION = "rounded-md border border-line-2 bg-bg-2 px-2.5 py-1 text-[12px] text-fg-2 transition-colors hover:bg-bg-3 hover:text-fg disabled:opacity-50";

/** How many findings a tab asks for, then after "Show more" (the hosts' most). */
const LIMITS = [200, 500];

export function Findings() {
  const [state, setState] = useState<InsightState>("open");
  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-[18px] font-semibold tracking-tight text-fg">Findings</h1>
          <p className="mt-0.5 text-[12.5px] text-fg-3">What the detectors see in recent events, most severe first. A finding closes when it stops showing up. One you resolve or dismiss stays closed unless it gets worse.</p>
        </div>
        <div className="flex rounded-md border border-line bg-bg-1 p-0.5" role="group" aria-label="Finding state">
          {TABS.map(([value, label]) => (
            <button key={value} type="button" aria-pressed={state === value} onClick={() => setState(value)} className={cx("rounded px-2.5 py-1 text-[12px] transition-colors", state === value ? "bg-bg-3 text-fg" : "text-fg-3 hover:text-fg")}>
              {label}
            </button>
          ))}
        </div>
      </div>
      {/* One list per tab, so a tab never shows another's findings while it loads. */}
      <List key={state} state={state} />
    </div>
  );
}

function List({ state }: { state: InsightState }) {
  const [version, setVersion] = useState(0);
  // What the last action did, for screen readers; the list keeps focus
  // when the finding acted on moves to another tab.
  const [done, setDone] = useState("");
  const list = useRef<HTMLDivElement>(null);
  const changed = (what: string) => {
    setDone(what);
    setVersion((n) => n + 1);
    list.current?.focus();
  };
  const [limit, setLimit] = useState(LIMITS[0]);
  const st = useAsync((s) => insights(state, s, limit), [version, limit], 30_000);
  const more = LIMITS.find((l) => l > limit);
  return (
    <div ref={list} tabIndex={-1} className="space-y-5 outline-none" aria-label="Findings">
      <span role="status" className="sr-only">{done}</span>
      {st.status === "loading" && <div className="label py-20 text-center">loading…</div>}
      {st.status === "error" && <Empty title="Couldn't load findings" body={st.error} />}
      {st.status === "ok" && st.data.length === 0 && (
        <Empty
          title={state === "open" ? "No open findings" : `No ${state} findings`}
          body="The detectors look at new events every few minutes. A finding names the trajectories and events that show it."
        />
      )}
      {st.status === "ok" && st.data.map((f) => <Finding key={f.id} f={f} onChanged={changed} />)}
      {st.status === "ok" && st.data.length >= limit && (
        <p className="mono text-center text-[11px] text-fg-4">
          The first {fmtInt(limit)}, most severe first.{" "}
          {more && <button type="button" className="text-fg-2 underline-offset-2 hover:text-fg hover:underline" onClick={() => setLimit(more)}>Show up to {fmtInt(more)}</button>}
        </p>
      )}
    </div>
  );
}

const DONE: Record<InsightState, string> = { open: "Reopened", dismissed: "Dismissed", resolved: "Marked resolved" };

function Finding({ f, onChanged }: { f: Insight; onChanged: (what: string) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const sev = SEVERITY[f.severity];
  const act = (next: InsightState) => {
    setBusy(true);
    setError(null);
    setInsightState(f.id, next)
      .then(() => onChanged(`${DONE[next]}: ${f.title}`))
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <article className="rounded-lg border border-line bg-bg-1 px-4 py-3.5">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className={cx("mono inline-flex items-center gap-1.5 text-[11.5px]", sev.cls)}><StatusDot status={sev.dot} />{sev.label}</span>
        <span className="label text-[10px]">{DETECTOR[f.detector] ?? f.detector}</span>
        <span className="mono ml-auto text-[11px] text-fg-4" title={`Still seen ${ago(f.last_seen_at)}`}>found {ago(f.first_seen_at)}</span>
      </div>
      <h2 className="mt-2 text-[14px] font-medium text-fg">{f.title}</h2>
      <p className="mt-1 text-[13px] leading-[1.55] text-fg-2">{f.body}</p>
      <p className="mt-2 text-[13px] leading-[1.55] text-fg-2"><span className="label mr-2 text-[10px]">Recommendation</span>{f.recommendation}</p>
      {f.evidence.length > 0 && (
        <ul className="mt-3 space-y-1">
          {f.evidence.map((e, i) => (
            <li key={i} className="mono flex flex-wrap items-baseline gap-x-2 text-[11.5px]">
              <a href={`#/t/${encodeURIComponent(e.trajectory_id)}`} title={e.trajectory_id} className="text-model hover:underline">{shortId(e.trajectory_id)}</a>
              <span className="text-fg-3">{e.note}</span>
            </li>
          ))}
        </ul>
      )}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        {f.state === "open" ? (
          <>
            <button type="button" className={ACTION} disabled={busy} onClick={() => act("resolved")}>Mark resolved</button>
            <button type="button" className={ACTION} disabled={busy} onClick={() => act("dismissed")}>Dismiss</button>
          </>
        ) : (
          <button type="button" className={ACTION} disabled={busy} onClick={() => act("open")}>Reopen</button>
        )}
        {error != null && <span role="alert" className="text-[12px] text-err">{error || "That didn't work."}</span>}
      </div>
    </article>
  );
}
