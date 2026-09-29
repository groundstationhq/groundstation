import { useMemo, useState, type ReactNode } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Glyph, StatusDot, kindColor } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { trajectory } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { cx, fmtClock, fmtDur, fmtInt, fmtTokens, tilde } from "@/lib/format";
import { breakdown, buildRows, fmtK, type Row } from "@/lib/spans";
import { attr, cacheHit, promptTokens, type Event, type TrajectoryDetail } from "@/lib/types";
import { fmtHit, hitCls } from "@/routes/Trajectories";
import { useReducedMotion } from "@/lib/hooks";
import { SortToggle, type Order } from "@/components/ui/SortToggle";

const CONTENT_KEYS = new Set<string>([attr.PROMPT_TEXT, attr.TOOL_INPUT, attr.TOOL_OUTPUT, attr.SHELL_COMMAND, attr.SEARCH_PATTERN, attr.ERROR_MESSAGE, attr.NOTIFICATION_MESSAGE]);

export type Filter = "all" | "user" | "tool" | "model" | "agent" | "failed";

/**
 * What the event list can be narrowed to. Kind filters keep user turns and the
 * end of the run as context; "failed" shows only what went wrong.
 */
const FILTERS: Array<{ id: Filter; label: string; match: (r: Row) => boolean; context: boolean }> = [
  { id: "all", label: "All events", match: () => true, context: false },
  { id: "user", label: "Turns", match: (r) => r.eventKind === "turn.user", context: false },
  { id: "tool", label: "Tools", match: (r) => r.kind === "tool", context: true },
  { id: "model", label: "Model calls", match: (r) => r.kind === "model", context: true },
  { id: "agent", label: "Subagents", match: (r) => r.kind === "agent", context: true },
  { id: "failed", label: "Failed", match: (r) => r.failed || r.kind === "error", context: false },
];

const isContext = (r: Row) => r.kind === "user" || r.kind === "complete" || r.kind === "error";

export function applyFilter(rows: Row[], filter: Filter): Row[] {
  const f = FILTERS.find((x) => x.id === filter) ?? FILTERS[0];
  return rows.filter((r) => f.match(r) || (f.context && isContext(r)));
}

export function filterCounts(rows: Row[]): Record<Filter, number> {
  const counts = { all: rows.length, user: 0, tool: 0, model: 0, agent: 0, failed: 0 };
  for (const f of FILTERS) if (f.id !== "all") counts[f.id] = rows.filter(f.match).length;
  return counts;
}

interface Filtering { filter: Filter; onFilter: (f: Filter) => void }

/** A number or legend entry that narrows the event list when clicked. */
function FilterLink({ to, filtering, className, children, title }: { to: Filter; filtering: Filtering; className?: string; children: ReactNode; title?: string }) {
  const active = filtering.filter === to;
  return (
    <button type="button" onClick={() => filtering.onFilter(active ? "all" : to)} aria-pressed={active} title={title ?? (active ? "Show all events" : `Show only ${FILTERS.find((f) => f.id === to)?.label.toLowerCase()}`)} className={cx("rounded-[3px] underline-offset-2 transition-colors hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-model", active && "text-model", className)}>
      {children}
    </button>
  );
}

function Minimap({ rows, total, running, filtering }: { rows: Row[]; total: number; running: boolean; filtering: Filtering }) {
  const spans = rows.filter((r) => r.durationMs != null && r.depth === 0 && (r.kind === "tool" || r.kind === "model" || r.kind === "agent"));
  return (
    <div className="border-b border-line px-4 py-3">
      <div className="relative h-6 w-full overflow-hidden rounded-[3px] bg-bg-3" role="img" aria-label="Timeline of the whole trajectory">
        {spans.map((r) => {
          const left = (r.t0 / total) * 100;
          const w = Math.max(0.3, ((r.durationMs ?? 0) / total) * 100);
          return <div key={r.id} className="absolute top-1 bottom-1 rounded-[2px]" title={`${r.name} ${r.detail} · ${fmtDur(r.durationMs ?? 0)}`} style={{ left: `${left}%`, width: `${w}%`, background: r.failed ? "var(--color-err)" : kindColor[r.kind], opacity: r.kind === "agent" ? 0.55 : 1 }} />;
        })}
        {running && <div className="absolute inset-y-0 right-0 w-px bg-fg" />}
      </div>
      <div className="mono mt-1.5 flex justify-between text-[10px] text-fg-4">
        <span>00:00</span>
        <span className="flex gap-2 text-fg-3">
          <FilterLink to="tool" filtering={filtering}><span className="text-tool">■</span> tool</FilterLink>
          <FilterLink to="model" filtering={filtering}><span className="text-model">●</span> model</FilterLink>
          <FilterLink to="agent" filtering={filtering}><span className="text-agent">◆</span> subagent</FilterLink>
          <FilterLink to="failed" filtering={filtering}><span className="text-err">■</span> failed</FilterLink>
        </span>
        <span>{fmtClock(total / 1000)}</span>
      </div>
    </div>
  );
}

function Breakdown({ rows, total }: { rows: Row[]; total: number }) {
  const b = useMemo(() => breakdown(rows), [rows]);
  const modelKnown = rows.some((r) => r.kind === "model" && r.durationMs != null);
  const other = Math.max(0, total - b.tool - b.model - b.sub);
  const parts = [
    { k: "tools", v: b.tool, c: "var(--color-tool)" },
    { k: "model", v: b.model, c: "var(--color-model)" },
    { k: "subagents", v: b.sub, c: "var(--color-agent)" },
    { k: modelKnown ? "idle / other" : "unattributed", v: other, c: "var(--color-bg-4)" },
  ].filter((p) => p.v > 0);
  return (
    <div className="grid gap-x-6 gap-y-4 border-b border-line px-4 py-3 md:grid-cols-2">
      <div>
        <div className="label text-[10px]">Where the time went</div>
        <div className="mt-2 flex h-2.5 w-full gap-[2px] overflow-hidden rounded-[3px]" role="img" aria-label={parts.map((p) => `${p.k} ${Math.round((p.v / total) * 100)}%`).join(", ")}>
          {parts.map((p) => <div key={p.k} className="h-full rounded-[2px]" style={{ width: `${(p.v / total) * 100}%`, background: p.c }} />)}
        </div>
        <ul className="mono mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[10.5px] text-fg-3">
          {parts.map((p) => (
            <li key={p.k} className="flex items-center gap-1.5"><span className="inline-block h-1.5 w-1.5 rounded-[1px]" style={{ background: p.c }} aria-hidden />{p.k} <span className="text-fg-2">{Math.round((p.v / total) * 100)}%</span></li>
          ))}
        </ul>
        {!modelKnown && <p className="mono mt-1.5 text-[10.5px] text-fg-4">This adapter reports model calls without latency, so model time is included in unattributed.</p>}
      </div>
      <div>
        <div className="label text-[10px]">Longest tool calls</div>
        <ol className="mono mt-2 space-y-1 text-[11.5px]">
          {b.top.slice(0, 4).map((t) => (
            <li key={t.key} className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-3">
              <span className="truncate text-fg-2">{t.key}</span>
              <span className="text-fg-4">×{t.n}{t.failed > 0 && <span className="text-err"> · {t.failed} failed</span>}</span>
              <span className={cx("w-16 text-right", t.ms / total > 0.3 ? "text-tool" : "text-fg")}>{fmtDur(t.ms)}</span>
            </li>
          ))}
          {b.top.length === 0 && <li className="text-fg-4">no tool calls</li>}
        </ol>
      </div>
      <div>
        <div className="label text-[10px]">Cache hit rate per model call</div>
        <div className="mt-2"><CacheChart rows={rows} /></div>
      </div>
      <div>
        <div className="label text-[10px]">Output tokens per second per model call</div>
        <div className="mt-2"><SpeedChart rows={rows} /></div>
      </div>
    </div>
  );
}

interface CallPoint { id: string; t0: number; v: number | null; tip: ReactNode }

/** A line over model calls in order, with a 0–max y axis and a hover readout for the nearest call. */
function CallChart({ pts, max, fmtTick, flag, ariaLabel, footer }: { pts: CallPoint[]; max: number; fmtTick: (v: number) => string; flag?: (v: number) => boolean; ariaLabel: string; footer: ReactNode }) {
  const [hover, setHover] = useState<number | null>(null);
  const W = 100, H = 30;
  const x = (i: number) => (i / (pts.length - 1)) * W;
  const y = (v: number) => H - 2 - Math.min(1, v / max) * (H - 4);
  const d = pts.map((p, i) => (p.v == null ? "" : `${i === 0 || pts[i - 1].v == null ? "M" : "L"}${x(i).toFixed(2)} ${y(p.v).toFixed(2)}`)).join(" ");
  const flagged = flag ? pts.map((p, i) => ({ ...p, i })).filter((p) => p.v != null && flag(p.v)) : [];
  const h = hover != null ? pts[hover] : null;
  const hx = hover != null ? (hover / (pts.length - 1)) * 100 : 0;
  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const f = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    setHover(Math.round(f * (pts.length - 1)));
  };
  return (
    <div>
      <div className="flex gap-1.5">
        <div className="mono relative h-[52px] w-7 shrink-0 text-right text-[9.5px] leading-none text-fg-4" aria-hidden>
          {[max, max / 2, 0].map((g) => <span key={g} className="absolute right-0 -translate-y-1/2" style={{ top: `${(y(g) / H) * 100}%` }}>{fmtTick(g)}</span>)}
        </div>
        <div className="relative h-[52px] min-w-0 flex-1" onPointerMove={onMove} onPointerLeave={() => setHover(null)}>
          <svg viewBox={`0 0 ${W} ${H}`} className="h-full w-full" preserveAspectRatio="none" role="img" aria-label={ariaLabel}>
            {[0, max / 2, max].map((g) => <line key={g} x1="0" x2={W} y1={y(g)} y2={y(g)} stroke="var(--color-line)" strokeWidth="0.3" vectorEffect="non-scaling-stroke" />)}
            <path d={d} fill="none" stroke="var(--color-model)" strokeWidth="1.2" vectorEffect="non-scaling-stroke" />
            {flagged.map((p) => <circle key={p.id} cx={x(p.i)} cy={y(p.v as number)} r="1.6" fill="var(--color-warn)" />)}
          </svg>
          {h && (
            <>
              <div className="pointer-events-none absolute inset-y-0 w-px bg-fg-4/50" style={{ left: `${hx}%` }} />
              {h.v != null && <div className="pointer-events-none absolute size-[7px] -translate-x-1/2 -translate-y-1/2 rounded-full border border-bg bg-model" style={{ left: `${hx}%`, top: `${(y(h.v) / H) * 100}%` }} />}
              <div className={cx("mono pointer-events-none absolute bottom-full z-10 mb-1.5 whitespace-nowrap rounded-[4px] border border-line bg-bg-2 px-2 py-1 text-[10.5px] text-fg-3 shadow-sm", hx > 60 ? "-translate-x-full" : "")} style={{ left: `${hx}%` }}>
                {h.tip} · call {hover! + 1} at {fmtClock(h.t0 / 1000)}
              </div>
            </>
          )}
        </div>
      </div>
      <div className="mono mt-1 flex flex-wrap justify-between gap-x-3 pl-[34px] text-[10.5px] text-fg-4">{footer}</div>
    </div>
  );
}

/** Cache hit rate per model call, in order. A dip means the prompt prefix changed and had to be re-sent. */
function CacheChart({ rows }: { rows: Row[] }) {
  const calls = rows.filter((r) => r.kind === "model" && r.tokens).map((r) => ({ ...r, hit: cacheHit(r.tokens!.in, r.tokens!.cacheWrite, r.tokens!.cacheRead) }));
  const valid = calls.filter((c) => c.hit != null);
  if (valid.length < 2) return <div className="mono text-[10.5px] text-fg-4">not enough model calls</div>;
  const pts: CallPoint[] = calls.map((c) => ({
    id: c.id,
    t0: c.t0,
    v: c.hit,
    tip: <><span className={hitCls(c.hit)}>{fmtHit(c.hit)}</span> · read {fmtK(c.tokens!.cacheRead)} · wrote {fmtK(c.tokens!.cacheWrite)}</>,
  }));
  const dips = calls.filter((c) => c.hit != null && c.hit < 0.5);
  const worst = [...dips].sort((a, b) => (a.hit ?? 1) - (b.hit ?? 1))[0];
  const min = Math.min(...valid.map((c) => c.hit as number));
  const last = valid[valid.length - 1].hit as number;
  return (
    <CallChart
      pts={pts}
      max={1}
      fmtTick={(v) => `${Math.round(v * 100)}%`}
      flag={(v) => v < 0.5}
      ariaLabel={`Cache hit rate across ${pts.length} model calls, lowest ${fmtHit(min)}, latest ${fmtHit(last)}`}
      footer={<>
        <span>latest <span className={hitCls(last)}>{fmtHit(last)}</span> · lowest <span className={hitCls(min)}>{fmtHit(min)}</span></span>
        {worst ? <span className="text-warn">{dips.length} call{dips.length > 1 ? "s" : ""} under 50% · worst at {fmtClock(worst.t0 / 1000)} re-sent {fmtK(worst.tokens!.cacheWrite)}</span> : <span>no calls under 50%</span>}
      </>}
    />
  );
}

/** Smallest 1/2/5×10ⁿ at or above v, so the axis ends on a round number. */
function niceCeil(v: number): number {
  const e = 10 ** Math.floor(Math.log10(v));
  return [1, 2, 5, 10].map((m) => m * e).find((n) => n >= v) ?? 10 * e;
}

const fmtTps = (v: number) => (v >= 10 ? `${Math.round(v)}` : v.toFixed(1));

/**
 * Output tokens per second per model call: output tokens over the whole call duration. Time to first
 * token is included, so calls with short outputs (a single tool call) read slower than the model streams.
 */
function SpeedChart({ rows }: { rows: Row[] }) {
  const calls = rows.filter((r) => r.kind === "model" && r.tokens).map((r) => ({ ...r, tps: r.durationMs && r.tokens!.out > 0 ? r.tokens!.out / (r.durationMs / 1000) : null }));
  const valid = calls.filter((c) => c.tps != null).map((c) => c.tps as number);
  if (valid.length < 2) {
    const noLatency = calls.length >= 2 && calls.every((c) => c.durationMs == null);
    return <div className="mono text-[10.5px] text-fg-4">{noLatency ? "this adapter reports model calls without latency" : "not enough model calls"}</div>;
  }
  const pts: CallPoint[] = calls.map((c) => ({
    id: c.id,
    t0: c.t0,
    v: c.tps,
    tip: c.tps == null ? <span>—</span> : <><span className="text-fg">{fmtTps(c.tps)} tok/s</span> · {fmtK(c.tokens!.out)} out in {fmtDur(c.durationMs!)}</>,
  }));
  const sorted = [...valid].sort((a, b) => a - b);
  const median = sorted[Math.floor(sorted.length / 2)];
  const totalOut = calls.reduce((n, c) => n + (c.tps != null ? c.tokens!.out : 0), 0);
  const totalSec = calls.reduce((n, c) => n + (c.tps != null ? c.durationMs! / 1000 : 0), 0);
  return (
    <CallChart
      pts={pts}
      max={niceCeil(sorted[sorted.length - 1])}
      fmtTick={fmtTps}
      ariaLabel={`Output tokens per second across ${pts.length} model calls, median ${fmtTps(median)}, range ${fmtTps(sorted[0])} to ${fmtTps(sorted[sorted.length - 1])}`}
      footer={<>
        <span>median <span className="text-fg">{fmtTps(median)}</span> · range {fmtTps(sorted[0])}–{fmtTps(sorted[sorted.length - 1])} tok/s</span>
        <span>overall {fmtTps(totalOut / totalSec)} tok/s over {fmtK(totalOut)} out</span>
      </>}
    />
  );
}

function Value({ k, v }: { k: string; v: unknown }) {
  const [open, setOpen] = useState(false);
  const text = typeof v === "string" ? v : JSON.stringify(v, null, 2);
  const long = CONTENT_KEYS.has(k) || text.length > 120;
  if (!long) return <span className="mono break-all text-fg">{text}</span>;
  const shown = open ? text : text.slice(0, 160) + (text.length > 160 ? "…" : "");
  return (
    <span className="mono block whitespace-pre-wrap break-all text-fg">
      {shown}
      {text.length > 160 && (
        <button className="ml-2 text-[10.5px] text-model hover:underline" onClick={() => setOpen((o) => !o)}>{open ? "less" : `more (${fmtInt(text.length)} chars)`}</button>
      )}
    </span>
  );
}

function Inspector({ row }: { row: Row }) {
  const events: Array<[string, Event]> = [["opened", row.open]];
  if (row.close) events.push(["closed", row.close]);
  const merged = new Map<string, unknown>();
  for (const [, e] of events) for (const [k, v] of Object.entries(e.attributes)) merged.set(k, v);
  const keys = [...merged.keys()].sort();
  return (
    <motion.div initial={{ height: 0, opacity: 0 }} animate={{ height: "auto", opacity: 1 }} exit={{ height: 0, opacity: 0 }} transition={{ duration: 0.22, ease: [0.16, 1, 0.3, 1] }} className="overflow-hidden">
      <div className="mx-2 mb-2 rounded-[4px] border border-line bg-bg px-3 py-2.5 lg:ml-[86px]">
        <div className="mono mb-2 flex flex-wrap gap-x-4 gap-y-1 text-[10.5px] text-fg-4">
          {events.map(([label, e]) => (
            <span key={e.id}>{label} <span className="text-fg-3">{e.kind}</span> · {new Date(e.timestamp).toLocaleTimeString([], { hour12: false })}.{String(new Date(e.timestamp).getMilliseconds()).padStart(3, "0")}</span>
          ))}
          {row.open.span_id && <span>span <span className="text-fg-3">{row.open.span_id}</span></span>}
          <span>agent <span className="text-fg-3">{row.open.agent.name}{row.open.agent.version ? ` ${row.open.agent.version}` : ""}</span></span>
        </div>
        <dl className="grid gap-x-6 gap-y-1.5 text-[11.5px] sm:grid-cols-[220px_minmax(0,1fr)]">
          {keys.map((k) => (
            <div key={k} className="contents">
              <dt className="mono truncate text-fg-3">{k}</dt>
              <dd className="min-w-0"><Value k={k} v={merged.get(k)} /></dd>
            </div>
          ))}
          {keys.length === 0 && <dd className="mono text-fg-4">no attributes</dd>}
        </dl>
      </div>
    </motion.div>
  );
}

function EventRow({ r, maxMs, open, onToggle }: { r: Row; maxMs: number; open: boolean; onToggle: () => void }) {
  const isLong = (r.durationMs ?? 0) >= 5000 && r.kind === "tool";
  const barW = Math.max(3, (Math.min(r.durationMs ?? 0, maxMs) / maxMs) * 120);
  const glyph = r.failed && !r.running ? "error" : r.kind;
  return (
    <button type="button" aria-expanded={open} onClick={onToggle} className={cx("group grid w-full grid-cols-[46px_14px_minmax(0,1fr)_auto] items-center gap-x-2.5 rounded-[4px] px-2 py-[5px] text-left text-[12.5px] transition-colors hover:bg-bg-2 sm:grid-cols-[46px_14px_130px_minmax(0,1fr)_auto]", open && "bg-bg-2", r.running && "bg-bg-2/60")}>
      <span className="mono text-[11px] text-fg-4">{fmtClock(r.t0 / 1000)}</span>
      <span className="flex justify-center" style={{ paddingLeft: r.depth * 10 }}><Glyph kind={glyph} size={9} pulse={r.running && r.kind === "model"} /></span>
      <span className={cx("mono truncate", r.kind === "user" ? "text-fg-2" : "text-fg")} style={{ paddingLeft: r.depth * 10 }}>{r.name}</span>
      <span className={cx("mono hidden truncate text-fg-2 sm:block", r.kind === "user" && "italic text-fg")}>{r.detail}</span>
      <span className="mono flex items-center justify-end gap-2 text-[11px] text-fg-3">
        {r.kind === "model" && r.tokens ? (
          <span className="flex items-center gap-2.5 text-[10.5px] text-fg-4">
            <span className="hidden md:inline">prompt <span className="text-fg-2">{fmtK(promptTokens(r.tokens.in, r.tokens.cacheWrite, r.tokens.cacheRead))}</span></span>
            <span className="hidden xl:inline">in·uncached <span className="text-fg-3">{fmtK(r.tokens.in)}</span></span>
            <span className="hidden xl:inline">cache-w <span className="text-fg-3">{fmtK(r.tokens.cacheWrite)}</span></span>
            <span className="hidden lg:inline">cache-r <span className="text-fg-3">{fmtK(r.tokens.cacheRead)}</span></span>
            <span>out <span className="text-fg-2">{fmtK(r.tokens.out)}</span></span>
            <span className={cx("w-9 text-right", hitCls(cacheHit(r.tokens.in, r.tokens.cacheWrite, r.tokens.cacheRead)))}>{fmtHit(cacheHit(r.tokens.in, r.tokens.cacheWrite, r.tokens.cacheRead))}</span>
          </span>
        ) : (
          r.dims.map((d, i) => <span key={i} className={cx("hidden md:inline", d.startsWith("exit") && !d.endsWith(" 0") && "text-err")}>{d}</span>)
        )}
        {isLong && (
          <span className="hidden h-1.5 overflow-hidden rounded-full bg-bg-4 sm:inline-block" style={{ width: barW }} aria-hidden>
            <span className={cx("block h-full rounded-full", r.failed ? "bg-err" : "bg-tool", r.running && "animate-pulse-dot")} style={{ width: "100%" }} />
          </span>
        )}
        <span className={cx("w-14 text-right", r.running ? "text-model" : isLong ? "text-fg" : "text-fg-2")}>{r.durationMs == null ? "" : r.running ? `${fmtDur(r.durationMs)} …` : fmtDur(r.durationMs)}</span>
      </span>
    </button>
  );
}

const STATUS: Record<string, { dot: "running" | "ok" | "err" | "idle"; label: string; cls: string }> = {
  running: { dot: "running", label: "Running", cls: "text-model" },
  idle: { dot: "idle", label: "Idle", cls: "text-fg-3" },
  waiting: { dot: "idle", label: "Waiting", cls: "text-fg-3" },
  completed: { dot: "ok", label: "Complete", cls: "text-fg-2" },
  failed: { dot: "err", label: "Failed", cls: "text-err" },
  cancelled: { dot: "idle", label: "Cancelled", cls: "text-fg-3" },
};

interface Stat { k: string; v: ReactNode }

function Header({ d, counts, filtering }: { d: TrajectoryDetail; counts: Record<Filter, number>; filtering: Filtering }) {
  const s = STATUS[d.status] ?? { dot: "idle" as const, label: d.status, cls: "text-fg-3" };
  const link = (to: Filter, text: string) => <FilterLink to={to} filtering={filtering}>{text}</FilterLink>;
  const stats: Stat[] = [
    { k: "duration", v: fmtDur(d.duration_ms) },
    { k: "turns", v: link("user", String(d.user_turns)) },
    { k: "model calls", v: link("model", String(d.model_calls)) },
    { k: "tool calls", v: <>{link("tool", String(d.tool_calls))}{d.tool_errors > 0 && <> · <FilterLink to="failed" filtering={filtering} className="text-err">{d.tool_errors} failed</FilterLink></>}</> },
    ...(counts.agent > 0 ? [{ k: "subagents", v: link("agent", String(counts.agent)) }] : []),
    { k: "prompt", v: fmtTokens(promptTokens(d.input_tokens, d.cache_creation_tokens, d.cache_read_tokens)) },
    { k: "in · uncached", v: fmtTokens(d.input_tokens) },
    { k: "cache write", v: fmtTokens(d.cache_creation_tokens) },
    { k: "cache read", v: fmtTokens(d.cache_read_tokens) },
    { k: "out", v: fmtTokens(d.output_tokens) },
    { k: "cache hit", v: fmtHit(cacheHit(d.input_tokens, d.cache_creation_tokens, d.cache_read_tokens)) },
    { k: "events", v: link("all", String(d.event_count)) },
  ];
  return (
    <div className="mb-4">
      <a href="#/trajectories" className="mono text-[11px] text-fg-3 hover:text-fg">← trajectories</a>
      <div className="mt-2 flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h1 className="text-[17px] font-semibold tracking-tight text-fg">{d.title ?? <span className="text-fg-3">(no prompt captured)</span>}</h1>
          <div className="mono mt-1 flex flex-wrap items-center gap-x-2 text-[11.5px] text-fg-3">
            <span>{d.id}</span><span className="text-fg-4">/</span>
            <span>{d.agent}{d.agent_version ? ` ${d.agent_version}` : ""}</span><span className="text-fg-4">/</span>
            <span>{(d.repository ?? d.cwd) ? tilde((d.repository ?? d.cwd) as string) : "—"}{d.branch ? ` @ ${d.branch}` : ""}</span>
            {d.host && <><span className="text-fg-4">/</span><span>{d.host}</span></>}
          </div>
        </div>
        <span className={cx("mono inline-flex items-center gap-1.5 text-[12px]", s.cls)}><StatusDot status={s.dot} />{s.label}</span>
      </div>
      <dl className={cx("mono mt-3 grid grid-cols-3 gap-px overflow-hidden rounded-lg border border-line bg-line sm:grid-cols-4", stats.length > 11 ? "lg:grid-cols-12" : "lg:grid-cols-11")}>
        {stats.map(({ k, v }) => (
          <div key={k} className="bg-bg-1 px-3 py-2"><dt className="label text-[10px]">{k}</dt><dd className="mt-0.5 text-[13px] text-fg">{v}</dd></div>
        ))}
      </dl>
    </div>
  );
}

export function Trajectory({ id }: { id: string }) {
  const reduced = useReducedMotion();
  const st = useAsync((s) => trajectory(id, s), [id], 3_000);
  const [open, setOpen] = useState<string | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [order, setOrder] = useState<Order>("newest");
  const data = st.status === "ok" ? st.data.data : null;
  const rows = useMemo(() => (data ? buildRows(data) : []), [data]);
  const counts = useMemo(() => filterCounts(rows), [rows]);
  // Header tiles and the minimap sit above the list; bring the list into view when they narrow it.
  const filtering: Filtering = {
    filter,
    onFilter: (f) => {
      setFilter(f);
      if (f !== "all") document.getElementById("events")?.scrollIntoView({ block: "start", behavior: reduced ? "auto" : "smooth" });
    },
  };
  if (st.status === "loading") return <div className="label py-20 text-center">loading…</div>;
  if (st.status === "error" || !data) return <Empty title="Couldn't load this trajectory" body={st.status === "error" ? st.error : ""} />;
  const total = Math.max(1, data.duration_ms);
  const maxMs = Math.max(1000, ...rows.map((r) => r.durationMs ?? 0));
  const filtered = applyFilter(rows, filter);
  const shown = order === "newest" ? [...filtered].reverse() : filtered;
  return (
    <div>
      <Header d={data} counts={counts} filtering={filtering} />
      <div className="overflow-hidden rounded-lg border border-line bg-bg-1">
        <Minimap rows={rows} total={total} running={data.status === "running"} filtering={filtering} />
        <Breakdown rows={rows} total={total} />
        <div id="events" className="flex flex-wrap items-center justify-between gap-y-1 border-b border-line px-3 py-1.5 scroll-mt-2">
          <div className="flex flex-wrap gap-1" role="group" aria-label="Filter events">
            {FILTERS.filter((f) => f.id === "all" || counts[f.id] > 0).map((f) => (
              <button key={f.id} type="button" onClick={() => setFilter(f.id)} aria-pressed={filter === f.id} className={cx("whitespace-nowrap rounded-[4px] px-2 py-1 text-[11.5px] transition-colors", filter === f.id ? "bg-bg-3 text-fg" : "text-fg-3 hover:text-fg-2", f.id === "failed" && filter !== "failed" && "text-err/80 hover:text-err")}>
                {f.label}{f.id !== "all" && <span className={cx("mono ml-1.5 text-[10.5px]", filter === f.id ? "text-fg-3" : "text-fg-4")}>{counts[f.id]}</span>}
              </button>
            ))}
          </div>
          <div className="flex items-center gap-3">
            <div className="mono hidden text-[10.5px] text-fg-4 sm:block">{filter === "all" ? `${shown.length} rows` : `${shown.length} of ${rows.length} rows`} · select a row to inspect</div>
            <SortToggle order={order} onChange={setOrder} />
          </div>
        </div>
        <ol className="px-1 py-1">
          {shown.map((r) => (
            <li key={r.id}>
              <EventRow r={r} maxMs={maxMs} open={open === r.id} onToggle={() => setOpen((o) => (o === r.id ? null : r.id))} />
              <AnimatePresence initial={false}>{open === r.id && (reduced ? <div><Inspector row={r} /></div> : <Inspector row={r} />)}</AnimatePresence>
            </li>
          ))}
          {shown.length === 0 && <li className="mono px-3 py-6 text-center text-[11.5px] text-fg-4">nothing matches this filter</li>}
        </ol>
        {data.status === "running" && (
          <div className="mono flex items-center gap-2 border-t border-line px-4 py-2 text-[11px] text-fg-3"><StatusDot status="running" /> live · refreshing every 3s</div>
        )}
      </div>
      <p className="mono mt-3 text-[10.5px] text-fg-4">Sizes in tool output are bytes as reported by the daemon. Content fields may be absent when excluded by redaction policy.</p>
    </div>
  );
}
