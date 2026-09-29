import { useMemo, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { Glyph, StatusDot, kindColor } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { trajectory } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { cx, fmtClock, fmtDur, fmtInt, fmtTokens, tilde } from "@/lib/format";
import { breakdown, buildRows, fmtK, type Row } from "@/lib/spans";
import { attr, type Event, type TrajectoryDetail } from "@/lib/types";
import { useReducedMotion } from "@/lib/hooks";

const CONTENT_KEYS = new Set<string>([attr.PROMPT_TEXT, attr.TOOL_INPUT, attr.TOOL_OUTPUT, attr.SHELL_COMMAND, attr.SEARCH_PATTERN, attr.ERROR_MESSAGE, attr.NOTIFICATION_MESSAGE]);

function Minimap({ rows, total, running }: { rows: Row[]; total: number; running: boolean }) {
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
        <span className="text-fg-3"><span className="text-tool">■</span> tool <span className="ml-2 text-model">●</span> model <span className="ml-2 text-agent">◆</span> subagent <span className="ml-2 text-err">■</span> failed</span>
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
    <div className="grid gap-4 border-b border-line px-4 py-3 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
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
    </div>
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
            <span className="hidden lg:inline">in <span className="text-fg-3">{fmtK(r.tokens.in)}</span></span>
            <span className="hidden lg:inline">cache-w <span className="text-fg-3">{fmtK(r.tokens.cacheWrite)}</span></span>
            <span className="hidden md:inline">cache-r <span className="text-fg-3">{fmtK(r.tokens.cacheRead)}</span></span>
            <span>out <span className="text-fg-2">{fmtK(r.tokens.out)}</span></span>
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

function Header({ d }: { d: TrajectoryDetail }) {
  const s = STATUS[d.status] ?? { dot: "idle" as const, label: d.status, cls: "text-fg-3" };
  const stats: Array<[string, string]> = [
    ["duration", fmtDur(d.duration_ms)],
    ["turns", String(d.user_turns)],
    ["model calls", String(d.model_calls)],
    ["tool calls", d.tool_errors ? `${d.tool_calls} · ${d.tool_errors} failed` : String(d.tool_calls)],
    ["in", fmtTokens(d.input_tokens)],
    ["cache write", fmtTokens(d.cache_creation_tokens)],
    ["cache read", fmtTokens(d.cache_read_tokens)],
    ["out", fmtTokens(d.output_tokens)],
    ["events", String(d.event_count)],
  ];
  return (
    <div className="mb-4">
      <a href="#/" className="mono text-[11px] text-fg-3 hover:text-fg">← trajectories</a>
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
      <dl className="mono mt-3 grid grid-cols-3 gap-px overflow-hidden rounded-lg border border-line bg-line sm:grid-cols-5 lg:grid-cols-9">
        {stats.map(([k, v]) => (
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
  const [filter, setFilter] = useState<"all" | "tool" | "model">("all");
  const data = st.status === "ok" ? st.data.data : null;
  const rows = useMemo(() => (data ? buildRows(data) : []), [data]);
  if (st.status === "loading") return <div className="label py-20 text-center">loading…</div>;
  if (st.status === "error" || !data) return <Empty title="Couldn't load this trajectory" body={st.status === "error" ? st.error : ""} />;
  const total = Math.max(1, data.duration_ms);
  const maxMs = Math.max(1000, ...rows.map((r) => r.durationMs ?? 0));
  const shown = rows.filter((r) => filter === "all" || r.kind === filter || r.kind === "user" || r.kind === "complete" || r.kind === "error");
  return (
    <div>
      <Header d={data} />
      <div className="overflow-hidden rounded-lg border border-line bg-bg-1">
        <Minimap rows={rows} total={total} running={data.status === "running"} />
        <Breakdown rows={rows} total={total} />
        <div className="flex items-center justify-between border-b border-line px-3 py-1.5">
          <div className="flex gap-1">
            {(["all", "tool", "model"] as const).map((f) => (
              <button key={f} onClick={() => setFilter(f)} aria-pressed={filter === f} className={cx("whitespace-nowrap rounded-[4px] px-2 py-1 text-[11.5px] transition-colors", filter === f ? "bg-bg-3 text-fg" : "text-fg-3 hover:text-fg-2")}>{f === "all" ? "All events" : f === "tool" ? "Tools" : "Model calls"}</button>
            ))}
          </div>
          <div className="mono hidden text-[10.5px] text-fg-4 sm:block">{shown.length} rows · select a row to inspect</div>
        </div>
        <ol className="px-1 py-1">
          {shown.map((r) => (
            <li key={r.id}>
              <EventRow r={r} maxMs={maxMs} open={open === r.id} onToggle={() => setOpen((o) => (o === r.id ? null : r.id))} />
              <AnimatePresence initial={false}>{open === r.id && (reduced ? <div><Inspector row={r} /></div> : <Inspector row={r} />)}</AnimatePresence>
            </li>
          ))}
        </ol>
        {data.status === "running" && (
          <div className="mono flex items-center gap-2 border-t border-line px-4 py-2 text-[11px] text-fg-3"><StatusDot status="running" /> live · refreshing every 3s</div>
        )}
      </div>
      <p className="mono mt-3 text-[10.5px] text-fg-4">Sizes in tool output are bytes as reported by the daemon. Content fields may be absent when excluded by redaction policy.</p>
    </div>
  );
}
