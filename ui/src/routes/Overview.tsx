import { useMemo, useState } from "react";
import { SortToggle, type Order } from "@/components/ui/SortToggle";
import { Glyph, StatusDot } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { Bars, HBars, Line, Stacked } from "@/components/charts";
import { ApiError, insights, toolStats, trajectories } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { useHost } from "@/lib/host";
import { ago, cx, fmtDur, fmtInt, fmtPct, fmtTokens, fmtUsd, percentile, tilde } from "@/lib/format";
import { cacheHit, promptTokens, totalTokens, type ToolStats, type TrajectorySummary } from "@/lib/types";
import { ENDED, fmtHit, hitCls, newestFirst, statusOf, STOPPED } from "@/routes/Trajectories";
import { SEVERITY } from "@/routes/Findings";

/* ---------- live ---------- */

function Stat({ k, v, sub, cls }: { k: string; v: string; sub?: string; cls?: string }) {
  return (
    <div className="bg-bg-1 px-4 py-3">
      <div className="label whitespace-nowrap text-[10px]">{k}</div>
      <div className={cx("mono mt-1 whitespace-nowrap text-[17px] text-fg", cls)}>{v}</div>
      <div className="mono h-4 truncate text-[10.5px] text-fg-4">{sub ?? ""}</div>
    </div>
  );
}

function Strip({ rows, latest }: { rows: TrajectorySummary[]; latest: boolean }) {
  const running = rows.filter((r) => r.status === "running");
  const done = rows.filter((r) => ENDED.includes(r.status));
  const ok = done.filter((r) => r.status === "completed").length;
  const sum = (f: (r: TrajectorySummary) => number) => rows.reduce((a, r) => a + f(r), 0);
  const prompt = sum((r) => promptTokens(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens));
  const hit = cacheHit(sum((r) => r.input_tokens), sum((r) => r.cache_creation_tokens), sum((r) => r.cache_read_tokens));
  const errors = sum((r) => r.tool_errors), tools = sum((r) => r.tool_calls);
  return (
    <div className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line sm:grid-cols-4 lg:grid-cols-7">
      <Stat k={latest ? "Latest trajectories" : "Trajectories"} v={fmtInt(rows.length)} sub={`${fmtInt(running.length)} running`} />
      <Stat k="Success" v={done.length ? fmtPct(ok / done.length) : "—"} sub={done.length ? `${fmtInt(ok)} / ${fmtInt(done.length)} finished` : "nothing finished yet"} />
      <Stat k="Prompt → out" v={`${fmtTokens(prompt)} → ${fmtTokens(sum((r) => r.output_tokens))}`} sub={`${fmtTokens(sum((r) => r.input_tokens))} in · uncached`} />
      <Stat k="Cache hit" v={fmtHit(hit)} sub={`${fmtTokens(sum((r) => r.cache_read_tokens))} read from cache`} cls={hitCls(hit)} />
      <Stat k="Model calls" v={fmtInt(sum((r) => r.model_calls))} sub={`${fmtInt(sum((r) => r.user_turns))} turns`} />
      <Stat k="Tool calls" v={fmtInt(tools)} sub={tools ? `${fmtPct(errors / tools)} failed` : ""} />
      <Stat k="Tool errors" v={fmtInt(errors)} cls={errors ? "text-err" : undefined} />
    </div>
  );
}

function Card({ title, right, children, preview, className }: { title: string; right?: React.ReactNode; children: React.ReactNode; preview?: boolean; className?: string }) {
  return (
    <section className={cx("overflow-hidden rounded-lg border border-line bg-bg-1", className)} aria-label={title}>
      <div className="flex h-9 items-center justify-between gap-3 border-b border-line bg-bg-2/70 px-3">
        <div className="flex shrink-0 items-center gap-2">
          <span className="label whitespace-nowrap text-[10px] text-fg-2">{title}</span>
          {preview && <span className="mono whitespace-nowrap rounded-full border border-agent/40 bg-agent/10 px-1.5 py-px text-[9.5px] text-agent" title="Mocked. This layer of the product is not built yet.">preview · v2</span>}
        </div>
        <div className="mono min-w-0 truncate text-[10.5px] text-fg-4">{right}</div>
      </div>
      {children}
    </section>
  );
}

const byStart = (order: Order) => (a: TrajectorySummary, b: TrajectorySummary) => (order === "newest" ? newestFirst(a, b) : newestFirst(b, a));

/** The newest live agents, whatever order the Latest card is in. */
function Active({ rows }: { rows: TrajectorySummary[] }) {
  const all = rows.filter((r) => r.status === "running" || STOPPED.includes(r.status)).sort(newestFirst);
  const live = all.slice(0, 6);
  return (
    <Card title="Active now" right={all.length > live.length ? `${fmtInt(all.length)} agents · newest ${live.length}` : `${live.length} agent${live.length === 1 ? "" : "s"}`}>
      {live.length === 0 ? (
        <div className="px-4 py-8 text-center text-[12.5px] text-fg-3">No agent is running. Start one and it appears here within seconds.</div>
      ) : (
        <ul>
          {live.map((r) => (
            <li key={r.id}>
              <a href={`#/t/${encodeURIComponent(r.id)}`} className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-3 border-b border-line px-4 py-2.5 last:border-0 hover:bg-bg-2">
                <div className="min-w-0">
                  <div className="truncate text-[12.5px] text-fg">{r.title ?? <span className="text-fg-3">(no prompt captured)</span>}</div>
                  <div className="mono mt-0.5 flex items-center gap-1.5 truncate text-[10.5px] text-fg-4"><Glyph kind="agent" size={6} />{r.agent} · {tilde(r.repository ?? r.cwd ?? "")}</div>
                </div>
                <div className="mono text-right text-[11.5px]">
                  <div className={r.status === "running" ? "text-model" : "text-fg-2"}>{fmtDur(r.duration_ms)}</div>
                  <div className="text-[10.5px] text-fg-4">{fmtTokens(promptTokens(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens))} → {fmtTokens(r.output_tokens)}</div>
                </div>
                <span className={cx("mono inline-flex w-[76px] items-center justify-end gap-1.5 text-[11px]", statusOf(r.status).cls)}><StatusDot status={statusOf(r.status).dot} />{statusOf(r.status).label}</span>
              </a>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function Recent({ rows, order, onOrder }: { rows: TrajectorySummary[]; order: Order; onOrder: (o: Order) => void }) {
  const recent = [...rows].sort(byStart(order)).slice(0, 6);
  return (
    <Card title={order === "newest" ? "Latest trajectories" : "Oldest trajectories"} right={<span className="flex items-center gap-3"><SortToggle order={order} onChange={onOrder} /><a href="#/trajectories" className="text-fg-3 hover:text-fg">all trajectories →</a></span>}>
      <table className="w-full">
        <tbody>
          {recent.map((r) => {
            const st = statusOf(r.status);
            return (
              <tr key={r.id} className="cursor-pointer border-b border-line last:border-0 hover:bg-bg-2" onClick={() => (location.hash = `#/t/${encodeURIComponent(r.id)}`)}>
                <td className="w-full max-w-0 px-4 py-2 text-[12.5px] text-fg"><a href={`#/t/${encodeURIComponent(r.id)}`} className="block truncate" onClick={(e) => e.stopPropagation()}>{r.title ?? <span className="text-fg-3">(no prompt captured)</span>}</a></td>
                <td className="mono hidden whitespace-nowrap px-3 py-2 text-[11px] text-fg-4 md:table-cell">{r.agent}</td>
                <td className="mono whitespace-nowrap px-3 py-2 text-right text-[11.5px] text-fg-2">{fmtDur(r.duration_ms)}</td>
                <td className="mono hidden whitespace-nowrap px-3 py-2 text-right text-[11.5px] text-fg-2 sm:table-cell">{fmtInt(r.tool_calls)} tools</td>
                <td className={cx("mono whitespace-nowrap px-4 py-2 text-right text-[11px]", hitCls(cacheHit(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens)))}>{fmtHit(cacheHit(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens))}</td>
                <td className="whitespace-nowrap px-4 py-2 text-right"><span className={cx("mono inline-flex items-center gap-1.5 text-[11px]", st.cls)}><StatusDot status={st.dot} />{st.label}</span></td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </Card>
  );
}

/* ---------- preview (mocked) ---------- */

type Sev = "warn" | "err" | "info";
const FINDINGS: Array<{ sev: Sev; title: string; subject: string; body: string; when: string; id: string }> = [
  { sev: "err", title: "Potential tool loop", subject: "read_file(src/refunds.rs) × 31 in 4m 02s", body: "File changed once; 27 reads returned identical content, adding 68k tokens to context.", when: "12m ago", id: "det_2a94" },
  { sev: "warn", title: "Slow trajectory", subject: "cargo test × 18 · 14m 42s cumulative", body: "61% of a 24m 32s trajectory. Typical for this repository is 6m 18s.", when: "41m ago", id: "det_2a91" },
  { sev: "warn", title: "Cache hit degradation", subject: "hit rate 99% → 34% after prompt change", body: "System prompt edited mid-trajectory invalidated the prefix; 64k tokens re-sent per call for 6 calls.", when: "1h ago", id: "det_2b02" },
  { sev: "warn", title: "Cost regression", subject: "review-bot v1.42 · $0.41 → $0.87 per task", body: "First seen 2h after deploy across 1,184 trajectories. Success rate unchanged.", when: "3h ago", id: "det_2b10" },
  { sev: "info", title: "Context growth", subject: "24k → 181k tokens over 12 calls", body: "Largest contributor: tool output (96k). Truncating test output would cut context by half.", when: "5h ago", id: "det_2b17" },
];
const sevMark: Record<Sev, [string, string]> = { err: ["▲", "text-err"], warn: ["◆", "text-warn"], info: ["●", "text-model"] };

/** The host's open findings, most severe first (hosted only: `insights` in `Health.features`). */
function LiveFindings() {
  const st = useAsync((s) => insights("open", s, 5), [], 30_000);
  return (
    <Card title="Findings" right={<a href="#/findings" className="text-fg-3 hover:text-fg">all findings →</a>}>
      {st.status === "loading" && <Note>loading…</Note>}
      {st.status === "error" && <Note>couldn't load findings</Note>}
      {st.status === "ok" && st.data.length === 0 && <Note>no open findings</Note>}
      {st.status === "ok" && st.data.length > 0 && (
        <ul>
          {st.data.map((f) => {
            const sev = SEVERITY[f.severity];
            const trajectory = f.evidence[0]?.trajectory_id;
            return (
              <li key={f.id} className="grid grid-cols-[68px_minmax(0,1fr)_auto] gap-x-3 border-b border-line px-4 py-2.5 last:border-0">
                <span className={cx("mono inline-flex items-center gap-1.5 pt-px text-[11px]", sev.cls)}><StatusDot status={sev.dot} />{sev.label}</span>
                <div className="min-w-0">
                  <a href={trajectory ? `#/t/${encodeURIComponent(trajectory)}` : "#/findings"} className="text-[12.5px] text-fg hover:underline">{f.title}</a>
                  <div className="mt-0.5 text-[11.5px] leading-[1.45] text-fg-3">{f.body}</div>
                </div>
                <span className="mono whitespace-nowrap text-[10.5px] text-fg-4" title={`Still seen ${ago(f.last_seen_at)}`}>found {ago(f.first_seen_at)}</span>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

function Findings() {
  return (
    <Card title="Findings" preview right="last 24h">
      <ul>
        {FINDINGS.map((f) => {
          const [glyph, cls] = sevMark[f.sev];
          return (
            <li key={f.id} className="grid grid-cols-[16px_minmax(0,1fr)_auto] gap-x-3 border-b border-line px-4 py-2.5 last:border-0">
              <span className={cx("mono pt-px text-[11px]", cls)} aria-hidden>{glyph}</span>
              <div className="min-w-0">
                <div className="flex flex-wrap items-baseline gap-x-2"><span className="whitespace-nowrap text-[12.5px] text-fg">{f.title}</span><span className="mono truncate text-[11px] text-fg-3">{f.subject}</span></div>
                <div className="mt-0.5 text-[11.5px] leading-[1.45] text-fg-3">{f.body}</div>
              </div>
              <span className="mono text-[10.5px] text-fg-4">{f.when}</span>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}

/* ---------- live charts ---------- */

/** The last `n` local calendar days, oldest first. */
function lastDays(n: number): Array<{ start: number; end: number; label: string }> {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  return Array.from({ length: n }, (_, k) => {
    const i = n - 1 - k;
    const d = new Date(today);
    d.setDate(today.getDate() - i);
    const e = new Date(d);
    e.setDate(d.getDate() + 1);
    return { start: d.getTime(), end: e.getTime(), label: i === 0 ? "today" : i === 1 ? "yesterday" : d.toLocaleDateString([], { weekday: "short" }) };
  });
}

const startedIn = (r: TrajectorySummary, d: { start: number; end: number }) => {
  const t = new Date(r.started_at).getTime();
  return t >= d.start && t < d.end;
};

function Note({ children }: { children: React.ReactNode }) {
  return <div className="mono px-4 py-7 text-center text-[11px] text-fg-4">{children}</div>;
}

function DurationChart({ rows }: { rows: TrajectorySummary[] }) {
  const durs = rows.filter((r) => ENDED.includes(r.status) && r.duration_ms > 0).map((r) => r.duration_ms).sort((a, b) => a - b);
  if (durs.length < 2) return <Card title="Trajectory duration" right="finished trajectories"><Note>needs at least two finished trajectories</Note></Card>;
  const p50 = percentile(durs, 0.5), p95 = percentile(durs, 0.95);
  const BINS = 14;
  const cap = Math.max(60_000, p95 * 1.25); // the last bin collects everything past this
  const width = cap / (BINS - 1);
  const counts = new Array<number>(BINS).fill(0);
  for (const d of durs) counts[Math.min(BINS - 1, Math.floor(d / width))]++;
  const runs = (n: number) => `${fmtInt(n)} trajector${n === 1 ? "y" : "ies"}`;
  const titles = counts.map((n, i) => (i === BINS - 1 ? `${runs(n)} · ${fmtDur(cap)}+` : `${runs(n)} · ${fmtDur(i * width)} – ${fmtDur((i + 1) * width)}`));
  return (
    <Card title="Trajectory duration" right={`p50 ${fmtDur(p50)} · p95 ${fmtDur(p95)}`}>
      <div className="px-4 py-3">
        <Bars values={counts} titles={titles} labels={[`${fmtInt(durs.length)} finished`, `p50 ${fmtDur(p50)}`, `${fmtDur(cap)}+`]} highlight={(i) => i === BINS - 1} ariaLabel={`Distribution of ${fmtInt(durs.length)} finished trajectory durations; median ${fmtDur(p50)}, 95th percentile ${fmtDur(p95)}`} />
      </div>
    </Card>
  );
}

/** Days that began before `loadedFrom` (the oldest start among the
 * trajectories loaded, when not all were) are incomplete. */
const notLoaded = (d: { start: number; end: number }, loadedFrom: number | null) => loadedFrom != null && d.start < loadedFrom;

function CacheTrend({ rows, loadedFrom }: { rows: TrajectorySummary[]; loadedFrom: number | null }) {
  const days = lastDays(14);
  const pts = days.map((d) => {
    if (loadedFrom != null && d.end <= loadedFrom) return null;
    const inDay = rows.filter((r) => startedIn(r, d));
    if (inDay.length === 0) return null;
    const sum = (f: (r: TrajectorySummary) => number) => inDay.reduce((a, r) => a + f(r), 0);
    return cacheHit(sum((r) => r.input_tokens), sum((r) => r.cache_creation_tokens), sum((r) => r.cache_read_tokens));
  });
  const valid = pts.filter((v): v is number => v != null);
  if (valid.length === 0) return <Card title="Cache hit rate" right="14 days"><Note>no model calls in the trajectories loaded</Note></Card>;
  const dips = pts.flatMap((v, i) => (v != null && v < 0.5 ? [i] : []));
  const latest = valid[valid.length - 1];
  const lowest = Math.min(...valid);
  return (
    <Card title="Cache hit rate" right={<>14 days · latest <span className={hitCls(latest)}>{fmtHit(latest)}</span></>}>
      <div className="px-4 py-3">
        <Line values={pts} min={0} max={1} marks={dips} titles={days.map((d, i) => (notLoaded(d, loadedFrom) ? `${d.label}: not all loaded` : pts[i] == null ? `${d.label}: no trajectories` : `${d.label}: ${fmtHit(pts[i])}`))} ariaLabel={`Daily cache hit rate over 14 days, latest ${fmtHit(latest)}, lowest ${fmtHit(lowest)}`} />
        <div className="mono mt-1 flex justify-between text-[10px] text-fg-4">
          <span>{days[0].label}</span>
          {dips.length > 0 ? <span className="text-warn">{dips.length} day{dips.length > 1 ? "s" : ""} under 50%</span> : <span>lowest {fmtHit(lowest)}</span>}
          <span>today</span>
        </div>
      </div>
    </Card>
  );
}

function ToolLatencyChart({ stats, error }: { stats: ToolStats | null; error?: { message: string; status: number | null } }) {
  const host = useHost();
  const days = stats?.days ?? 14;
  const tools = (stats?.tools ?? []).slice(0, 6);
  if (tools.length === 0) {
    // Only a gsd older than the endpoint answers 404 for it.
    const outdated = error && !host.hosted && error.status === 404;
    return (
      <Card title="Tool latency" right={`p50 / p95 · ${days}d`}>
        <Note>{outdated ? <>this gsd has no <span className="text-fg-3">/v1/stats/tools</span>; run <span className="text-fg-3">groundstation update</span></> : error ? `couldn't load tool latency: ${error.message}` : stats ? `no tool calls in the last ${days} days` : "loading…"}</Note>
      </Card>
    );
  }
  const slowest = [...tools].sort((a, b) => b.p95_ms - a.p95_ms)[0];
  return (
    <Card title="Tool latency" right={`p50 / p95 · ${days}d`}>
      <div className="px-4 py-3">
        <HBars log rows={tools.map((t) => ({ k: t.failed > 0 ? `${t.category} ✕${fmtInt(t.failed)}` : t.category, p50: t.p50_ms, p95: t.p95_ms, fmt: fmtDur }))} ariaLabel={`Tool latency by category over ${days} days; ${slowest.category} slowest at ${fmtDur(slowest.p95_ms)} p95`} />
      </div>
    </Card>
  );
}

function CostChart({ rows, loadedFrom }: { rows: TrajectorySummary[]; loadedFrom: number | null }) {
  const days = lastDays(14);
  const perDay = days.map((d) => rows.filter((r) => startedIn(r, d)));
  const cost = perDay.map((rs) => rs.reduce((a, r) => a + r.cost_usd, 0));
  const tokens = perDay.map((rs) => rs.reduce((a, r) => a + totalTokens(r), 0));
  const hasCost = cost.some((v) => v > 0);
  if (!tokens.some((v) => v > 0)) return <Card title="Cost per day" right="14 days"><Note>no trajectories in the last 14 days</Note></Card>;

  // The day an agent first ran a new version, after its first day in the window.
  const firstDay = new Map<string, number>();
  perDay.forEach((rs, i) => rs.forEach((r) => {
    for (const key of [r.agent, `${r.agent}@${r.agent_version ?? ""}`]) if (!firstDay.has(key)) firstDay.set(key, i);
  }));
  const shipped: Array<{ i: number; label: string }> = [];
  for (const [key, i] of firstDay) {
    const at = key.indexOf("@");
    if (at < 0 || key.slice(at + 1) === "") continue;
    const [agent, version] = [key.slice(0, at), key.slice(at + 1)];
    // Shipped: newer than every version of this agent seen before it. An
    // older one appearing later is another machine (or a teammate) behind.
    const earlier = [...firstDay].filter(([k, j]) => k.startsWith(`${agent}@`) && k !== key && j < i).map(([k]) => k.slice(at + 1));
    if (i > (firstDay.get(agent) ?? 0) && earlier.every((v) => newerVersion(version, v))) shipped.push({ i, label: `${agent} ${version}` });
  }
  const shippedOn = new Set(shipped.map((s) => s.i));
  const values = hasCost ? cost : tokens;
  const fmt = hasCost ? (v: number) => fmtUsd(v) : (v: number) => `${fmtTokens(v)} tokens`;
  const titles = days.map((d, i) => `${d.label}: ${notLoaded(d, loadedFrom) ? "not all loaded" : fmt(values[i])}${shipped.filter((s) => s.i === i).map((s) => ` · ${s.label} shipped`).join("")}`);
  const middle = shipped.length === 1 ? `${shipped[0].label} shipped` : shipped.length > 1 ? `${shipped.length} version changes` : "";
  return (
    <Card title={hasCost ? "Cost per day" : "Tokens per day"} right={`${fmt(values[values.length - 1])} today`}>
      <div className="px-4 py-3">
        <Bars tone="neutral" values={values} titles={titles} labels={[days[0].label, middle, "today"]} highlight={(i) => shippedOn.has(i)} ariaLabel={`${hasCost ? "Cost" : "Tokens"} per day over 14 days, ${fmt(values[values.length - 1])} today${middle ? `, ${middle}` : ""}`} />
        <p className="mt-2 text-[11px] leading-[1.45] text-fg-3">{hasCost ? "Dollars as reported by the agent (OpenCode, pi). Claude Code and Codex report tokens only, so their runs don't add to this." : "No agent here reports cost; Claude Code and Codex report tokens only."}</p>
      </div>
    </Card>
  );
}

function Charts({ rows, loadedFrom }: { rows: TrajectorySummary[]; loadedFrom: number | null }) {
  const stats = useAsync((s) => toolStats(14, s), [], 30_000);
  return (
    <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      <DurationChart rows={rows} />
      <CacheTrend rows={rows} loadedFrom={loadedFrom} />
      <ToolLatencyChart stats={stats.status === "ok" ? stats.data : null} error={stats.status === "error" ? { message: stats.error, status: stats.cause instanceof ApiError ? stats.cause.status : null } : undefined} />
      <CostChart rows={rows} loadedFrom={loadedFrom} />
    </div>
  );
}

function Versions() {
  const rows: Array<[string, string, string, string, boolean]> = [
    ["prompt tokens / task", "46k", "83k", "+81%", true],
    ["runtime", "3m 41s", "5m 25s", "+47%", true],
    ["tool calls", "27", "44", "+63%", true],
    ["cache hit", "97%", "71%", "−26pt", true],
    ["cost / task", "$0.41", "$0.87", "+112%", true],
    ["success rate", "92.1%", "92.4%", "+0.3pt", false],
  ];
  return (
    <Card title="Agent versions" preview right="review-bot · v41 → v42 · 7d · n=2,417">
      <table className="w-full">
        <thead><tr className="border-b border-line">{["metric", "v41", "v42", "Δ"].map((h, i) => <th key={h} className={cx("label px-4 py-1.5 text-left text-[10px] font-normal", i > 0 && "text-right")}>{h}</th>)}</tr></thead>
        <tbody>
          {rows.map(([k, a, b, d, bad]) => (
            <tr key={k} className="border-b border-line last:border-0">
              <td className="px-4 py-1.5 text-[12px] text-fg-2">{k}</td>
              <td className="mono px-4 py-1.5 text-right text-[11.5px] text-fg-3">{a}</td>
              <td className="mono px-4 py-1.5 text-right text-[11.5px] text-fg">{b}</td>
              <td className={cx("mono px-4 py-1.5 text-right text-[11.5px]", bad ? "text-warn" : "text-fg-3")}>{d}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="border-t border-line px-4 py-2 text-[11.5px] text-fg-3">Success barely moved. Cost and runtime doubled: v42 searches the repository 2.4× more before editing.</div>
    </Card>
  );
}

function Where({ rows, latest }: { rows: TrajectorySummary[]; latest: boolean }) {
  const sum = (f: (r: TrajectorySummary) => number) => rows.reduce((a, r) => a + f(r), 0);
  return (
    <Card title="Where the tokens went" right={latest ? `latest ${fmtInt(rows.length)} trajectories · live` : "all trajectories · live"}>
      <div className="px-4 py-3">
        <Stacked ariaLabel="Token composition across all trajectories" parts={[
          { k: "cache read", v: sum((r) => r.cache_read_tokens), c: "var(--color-model)" },
          { k: "cache write", v: sum((r) => r.cache_creation_tokens), c: "var(--color-tool)" },
          { k: "in · uncached", v: sum((r) => r.input_tokens), c: "var(--color-fg-3)" },
          { k: "out", v: sum((r) => r.output_tokens), c: "var(--color-ok)" },
        ]} />
        <p className="mt-2.5 text-[11.5px] leading-[1.45] text-fg-3">Cache reads are billed at roughly a tenth of uncached input. A shrinking teal share means the prompt prefix is being invalidated.</p>
      </div>
    </Card>
  );
}

/** How many trajectories the Overview loads. */
const LIMIT = 1000;

/** Whether version `a` is newer than `b`, comparing their numbers in order. */
function newerVersion(a: string, b: string): boolean {
  const parts = (v: string) => v.split(/[^0-9]+/).filter(Boolean).map(Number);
  const [x, y] = [parts(a), parts(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) > (y[i] ?? 0);
  }
  return false;
}

export function Overview() {
  const host = useHost();
  const st = useAsync((s) => trajectories(LIMIT, s), [], 5_000);
  const rows = useMemo(() => (st.status === "ok" ? st.data : []), [st]);
  // A full answer may leave older trajectories out: the hosts send the most recently active.
  const latest = rows.length >= LIMIT;
  const loadedFrom = latest ? Math.min(...rows.map((r) => Date.parse(r.started_at))) : null;
  const [order, setOrder] = useState<Order>("newest");
  if (st.status === "loading") return <div className="label py-20 text-center">loading…</div>;
  if (st.status === "error") return <Empty title={host.hosted ? "Couldn't load trajectories" : "Couldn't reach the daemon"} body={st.error} />;
  return (
    <div className="space-y-4">
      <div className="flex items-end justify-between">
        <div>
          <h1 className="text-[18px] font-semibold tracking-tight text-fg">Overview</h1>
          <p className="mt-0.5 text-[12.5px] text-fg-3">What every agent {host.hosted ? "uploading here" : "on this machine"} is doing, and what it's costing.</p>
        </div>
        <div className="mono text-[11px] text-fg-4">live · updates every 5s · <span className="text-agent">preview</span> = mocked until v2</div>
      </div>
      <Strip rows={rows} latest={latest} />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)]">
        <div className="space-y-4">
          <Active rows={rows} />
          <Where rows={rows} latest={latest} />
        </div>
        {host.features.includes("insights") ? <LiveFindings /> : <Findings />}
      </div>
      {latest && <p className="mono text-[11px] text-fg-4">These figures cover the {fmtInt(LIMIT)} most recently active trajectories; days before {new Date(loadedFrom ?? 0).toLocaleDateString()} aren't all loaded.</p>}
      <Charts rows={rows} loadedFrom={loadedFrom} />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)]">
        <Recent rows={rows} order={order} onOrder={setOrder} />
        <Versions />
      </div>
    </div>
  );
}
