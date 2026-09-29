import { useMemo } from "react";
import { Glyph, StatusDot } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { Bars, HBars, Line, Stacked } from "@/components/charts";
import { trajectories } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { cx, fmtDur, fmtInt, fmtTokens, tilde } from "@/lib/format";
import { cacheHit, promptTokens, type TrajectorySummary } from "@/lib/types";
import { fmtHit, hitCls } from "@/routes/Trajectories";

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

function Strip({ rows }: { rows: TrajectorySummary[] }) {
  const running = rows.filter((r) => r.status === "running");
  const done = rows.filter((r) => ["completed", "failed", "cancelled"].includes(r.status));
  const ok = done.filter((r) => r.status === "completed").length;
  const sum = (f: (r: TrajectorySummary) => number) => rows.reduce((a, r) => a + f(r), 0);
  const prompt = sum((r) => promptTokens(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens));
  const hit = cacheHit(sum((r) => r.input_tokens), sum((r) => r.cache_creation_tokens), sum((r) => r.cache_read_tokens));
  const errors = sum((r) => r.tool_errors), tools = sum((r) => r.tool_calls);
  return (
    <div className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line sm:grid-cols-4 lg:grid-cols-7">
      <Stat k="Trajectories" v={fmtInt(rows.length)} sub={`${running.length} running`} />
      <Stat k="Success" v={done.length ? `${((ok / done.length) * 100).toFixed(1)}%` : "—"} sub={done.length ? `${ok} / ${done.length} finished` : "nothing finished yet"} />
      <Stat k="Prompt → out" v={`${fmtTokens(prompt)} → ${fmtTokens(sum((r) => r.output_tokens))}`} sub={`${fmtTokens(sum((r) => r.input_tokens))} in · uncached`} />
      <Stat k="Cache hit" v={fmtHit(hit)} sub={`${fmtTokens(sum((r) => r.cache_read_tokens))} read from cache`} cls={hitCls(hit)} />
      <Stat k="Model calls" v={fmtInt(sum((r) => r.model_calls))} sub={`${fmtInt(sum((r) => r.user_turns))} turns`} />
      <Stat k="Tool calls" v={fmtInt(tools)} sub={tools ? `${((errors / tools) * 100).toFixed(1)}% failed` : ""} />
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

function Active({ rows }: { rows: TrajectorySummary[] }) {
  const live = rows.filter((r) => r.status === "running" || r.status === "idle" || r.status === "waiting").slice(0, 6);
  return (
    <Card title="Active now" right={`${live.length} agent${live.length === 1 ? "" : "s"}`}>
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
                <span className={cx("mono inline-flex w-[76px] items-center justify-end gap-1.5 text-[11px]", r.status === "running" ? "text-model" : "text-fg-3")}><StatusDot status={r.status === "running" ? "running" : "idle"} />{r.status === "running" ? "Running" : "Idle"}</span>
              </a>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function Recent({ rows }: { rows: TrajectorySummary[] }) {
  const recent = [...rows].sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 6);
  const s: Record<string, { dot: "running" | "ok" | "err" | "idle"; cls: string }> = { running: { dot: "running", cls: "text-model" }, completed: { dot: "ok", cls: "text-fg-2" }, failed: { dot: "err", cls: "text-err" } };
  return (
    <Card title="Recent trajectories" right={<a href="#/trajectories" className="text-fg-3 hover:text-fg">all trajectories →</a>}>
      <table className="w-full">
        <tbody>
          {recent.map((r) => {
            const st = s[r.status] ?? { dot: "idle" as const, cls: "text-fg-3" };
            return (
              <tr key={r.id} className="cursor-pointer border-b border-line last:border-0 hover:bg-bg-2" onClick={() => (location.hash = `#/t/${encodeURIComponent(r.id)}`)}>
                <td className="w-full max-w-0 px-4 py-2 text-[12.5px] text-fg"><span className="block truncate">{r.title ?? <span className="text-fg-3">(no prompt captured)</span>}</span></td>
                <td className="mono hidden whitespace-nowrap px-3 py-2 text-[11px] text-fg-4 md:table-cell">{r.agent}</td>
                <td className="mono whitespace-nowrap px-3 py-2 text-right text-[11.5px] text-fg-2">{fmtDur(r.duration_ms)}</td>
                <td className="mono hidden whitespace-nowrap px-3 py-2 text-right text-[11.5px] text-fg-2 sm:table-cell">{r.tool_calls} tools</td>
                <td className={cx("mono whitespace-nowrap px-4 py-2 text-right text-[11px]", hitCls(cacheHit(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens)))}>{fmtHit(cacheHit(r.input_tokens, r.cache_creation_tokens, r.cache_read_tokens))}</td>
                <td className="px-4 py-2 text-right"><span className={cx("inline-flex items-center", st.cls)}><StatusDot status={st.dot} /></span></td>
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
  { sev: "warn", title: "Slow trajectory", subject: "cargo test × 18 · 14m 42s cumulative", body: "61% of a 24m 32s run. Typical for this repository is 6m 18s.", when: "41m ago", id: "det_2a91" },
  { sev: "warn", title: "Cache hit degradation", subject: "hit rate 99% → 34% after prompt change", body: "System prompt edited mid-session invalidated the prefix; 64k tokens re-sent per call for 6 calls.", when: "1h ago", id: "det_2b02" },
  { sev: "warn", title: "Cost regression", subject: "review-bot v1.42 · $0.41 → $0.87 per task", body: "First seen 2h after deploy across 1,184 trajectories. Success rate unchanged.", when: "3h ago", id: "det_2b10" },
  { sev: "info", title: "Context growth", subject: "24k → 181k tokens over 12 calls", body: "Largest contributor: tool output (96k). Truncating test output would cut context by half.", when: "5h ago", id: "det_2b17" },
];
const sevMark: Record<Sev, [string, string]> = { err: ["▲", "text-err"], warn: ["◆", "text-warn"], info: ["●", "text-model"] };

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

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function Charts() {
  const hitTrend = [0.97, 0.98, 0.98, 0.97, 0.99, 0.98, 0.62, 0.71, 0.96, 0.98, 0.99, 0.98, 0.99, 0.99];
  return (
    <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
      <Card title="Trajectory duration" preview right="p50 3m 41s · p95 14m 22s">
        <div className="px-4 py-3">
          <Bars values={[4, 18, 41, 62, 48, 31, 19, 12, 8, 5, 3, 2, 1, 1]} labels={["0m", "p50 3m 41s", "28m+"]} highlight={(i) => i >= 11} ariaLabel="Distribution of trajectory durations, most between two and six minutes, a tail past 20 minutes" />
        </div>
      </Card>
      <Card title="Cache hit rate" preview right="14 days">
        <div className="px-4 py-3">
          <Line values={hitTrend} min={0.5} max={1} marks={[6, 7]} ariaLabel="Daily cache hit rate, steady near 98% with a two-day dip to 62% after a prompt change" />
          <div className="mono mt-1 flex justify-between text-[10px] text-fg-4"><span>{DAYS[0]}</span><span className="text-warn">dip: prompt change</span><span>today</span></div>
        </div>
      </Card>
      <Card title="Tool latency" preview right="p50 / p95">
        <div className="px-4 py-3">
          <HBars log rows={[
            { k: "shell", p50: 2410, p95: 47193, fmt: fmtDur },
            { k: "browser", p50: 312, p95: 2140, fmt: fmtDur },
            { k: "http", p50: 188, p95: 1204, fmt: fmtDur },
            { k: "grep", p50: 31, p95: 240, fmt: fmtDur },
            { k: "read_file", p50: 14, p95: 96, fmt: fmtDur },
          ]} ariaLabel="Tool latency by tool; shell dominates at 47 seconds p95" />
        </div>
      </Card>
      <Card title="Cost per day" preview right="est. · $129 today">
        <div className="px-4 py-3">
          <Bars tone="neutral" values={[61, 58, 66, 71, 64, 88, 102, 97, 91, 106, 118, 112, 121, 129]} labels={["14d ago", "agent v42 shipped", "today"]} highlight={(i) => i === 6} ariaLabel="Estimated cost per day over 14 days, rising from $61 to $129 after agent v42 shipped" />
        </div>
      </Card>
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

function Where({ rows }: { rows: TrajectorySummary[] }) {
  // Real where we can: token composition across everything the daemon has seen.
  const sum = (f: (r: TrajectorySummary) => number) => rows.reduce((a, r) => a + f(r), 0);
  return (
    <Card title="Where the tokens went" right="all trajectories · live">
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

export function Overview() {
  const st = useAsync((s) => trajectories(200, s), [], 5_000);
  const rows = useMemo(() => (st.status === "ok" ? st.data.data : []), [st]);
  if (st.status === "loading") return <div className="label py-20 text-center">loading…</div>;
  if (st.status === "error") return <Empty title="Couldn't reach the daemon" body={st.error} />;
  return (
    <div className="space-y-4">
      <div className="flex items-end justify-between">
        <div>
          <h1 className="text-[18px] font-semibold tracking-tight text-fg">Overview</h1>
          <p className="mt-0.5 text-[12.5px] text-fg-3">What every agent on this machine is doing, and what it's costing.</p>
        </div>
        <div className="mono text-[11px] text-fg-4">{st.data.source === "demo" ? "demo data" : "live · updates every 5s"} · <span className="text-agent">preview</span> = mocked until v2</div>
      </div>
      <Strip rows={rows} />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)]">
        <div className="space-y-4">
          <Active rows={rows} />
          <Where rows={rows} />
        </div>
        <Findings />
      </div>
      <Charts />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,7fr)_minmax(0,5fr)]">
        <Recent rows={rows} />
        <Versions />
      </div>
    </div>
  );
}
