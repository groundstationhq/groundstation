import { Glyph, StatusDot } from "@/components/ui/primitives";
import { Empty } from "@/components/Shell";
import { trajectories } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { cx, fmtDur, fmtInt, fmtTokens } from "@/lib/format";
import { totalTokens, type TrajectorySummary } from "@/lib/types";

function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

const STATUS: Record<string, { dot: "running" | "ok" | "err" | "warn" | "idle"; label: string; cls: string }> = {
  running: { dot: "running", label: "Running", cls: "text-model" },
  completed: { dot: "ok", label: "Complete", cls: "text-fg-2" },
  failed: { dot: "err", label: "Failed", cls: "text-err" },
  cancelled: { dot: "idle", label: "Cancelled", cls: "text-fg-3" },
};

/** [header, classes]; the same classes are applied to the cells so columns hide consistently. */
const COLS: Array<[string, string]> = [
  ["Trajectory", ""],
  ["Agent", "hidden sm:table-cell"],
  ["Repository", "hidden md:table-cell"],
  ["Started", "hidden lg:table-cell"],
  ["Duration", "text-right whitespace-nowrap"],
  ["Turns", "hidden lg:table-cell text-right"],
  ["Tools", "hidden md:table-cell text-right"],
  ["Tokens", "hidden sm:table-cell text-right"],
  ["Status", "text-right"],
];

function median(xs: number[]): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)];
}

function Overview({ rows }: { rows: TrajectorySummary[] }) {
  const done = rows.filter((r) => r.status !== "running");
  const ok = done.filter((r) => r.status === "completed").length;
  const cells: Array<[string, string, string?]> = [
    ["Trajectories", fmtInt(rows.length), `${rows.filter((r) => r.status === "running").length} running`],
    ["Success", done.length ? `${((ok / done.length) * 100).toFixed(1)}%` : "—", `${ok} / ${done.length}`],
    ["Median runtime", fmtDur(median(done.map((r) => r.duration_ms))), ""],
    ["Tokens", fmtTokens(rows.reduce((a, r) => a + totalTokens(r), 0)), ""],
    ["Tool calls", fmtInt(rows.reduce((a, r) => a + r.tool_calls, 0)), ""],
    ["Tool errors", fmtInt(rows.reduce((a, r) => a + r.tool_errors, 0)), ""],
  ];
  return (
    <div className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line sm:grid-cols-3 lg:grid-cols-6">
      {cells.map(([k, v, d]) => (
        <div key={k} className="bg-bg-1 px-4 py-3">
          <div className="label text-[10px]">{k}</div>
          <div className="mono mt-1 text-[17px] text-fg">{v}</div>
          <div className="mono h-4 text-[10.5px] text-fg-4">{d}</div>
        </div>
      ))}
    </div>
  );
}

export function Trajectories() {
  const st = useAsync((s) => trajectories(100, s), [], 5_000);
  if (st.status === "loading") return <div className="label py-20 text-center">loading…</div>;
  if (st.status === "error") return <Empty title="Couldn't load trajectories" body={st.error} />;
  const rows = st.data.data;
  return (
    <div className="space-y-5">
      <div className="flex items-end justify-between">
        <div>
          <h1 className="text-[18px] font-semibold tracking-tight text-fg">Trajectories</h1>
          <p className="mt-0.5 text-[12.5px] text-fg-3">Every agent run this daemon has seen, newest first.</p>
        </div>
        <div className="mono whitespace-nowrap text-[11px] text-fg-4">{st.data.source === "demo" ? "demo data" : "local · updates every 5s"}</div>
      </div>
      <Overview rows={rows} />
      {rows.length === 0 ? (
        <Empty
          title="No trajectories yet"
          body={
            <>
              Connect an agent and run it once: <span className="mono text-fg-2">groundstation connect claude-code</span>. The trajectory appears here as soon as the first event arrives.
            </>
          }
        />
      ) : (
        <div className="overflow-x-auto rounded-lg border border-line bg-bg-1 scroll-thin">
          <table className="w-full min-w-[340px]">
            <thead>
              <tr className="border-b border-line">
                {COLS.map(([h, cls]) => (
                  <th key={h} className={cx("label px-3 py-2 text-left text-[10px] font-normal first:pl-4 last:pr-4", cls)}>
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => {
                const s = STATUS[r.status] ?? { dot: "idle" as const, label: r.status, cls: "text-fg-3" };
                return (
                  <tr key={r.id} className="cursor-pointer border-b border-line last:border-0 hover:bg-bg-2" onClick={() => (location.hash = `#/t/${encodeURIComponent(r.id)}`)}>
                    <td className="w-full max-w-0 px-3 py-2.5 pl-4">
                      <a href={`#/t/${encodeURIComponent(r.id)}`} className="block truncate text-[13px] text-fg" onClick={(e) => e.stopPropagation()}>
                        {r.title ?? <span className="text-fg-3">(no prompt captured)</span>}
                      </a>
                      <div className="mono mt-0.5 text-[10.5px] text-fg-4">{r.id}</div>
                    </td>
                    <td className={cx("px-3 py-2.5 text-[12.5px] text-fg-2", COLS[1][1])}>
                      <div className="flex items-center gap-1.5"><Glyph kind="agent" size={7} />{r.agent}</div>
                      <div className="mono text-[10.5px] text-fg-4">{r.agent_version ?? ""}</div>
                    </td>
                    <td className={cx("mono px-3 py-2.5 text-[12px] text-fg-2", COLS[2][1])}>
                      {r.repository ?? r.cwd?.split("/").slice(-1)[0] ?? "—"}
                      {r.branch && <div className="text-[10.5px] text-fg-4">{r.branch}</div>}
                    </td>
                    <td className={cx("mono px-3 py-2.5 text-[12px] text-fg-3", COLS[3][1])}>{ago(r.started_at)}</td>
                    <td className={cx("mono px-3 py-2.5 text-right text-[12.5px]", r.status === "running" ? "text-model" : "text-fg")}>{fmtDur(r.duration_ms)}</td>
                    <td className={cx("mono px-3 py-2.5 text-[12.5px] text-fg-2", COLS[5][1])}>{r.user_turns}</td>
                    <td className={cx("mono px-3 py-2.5 text-[12.5px] text-fg-2", COLS[6][1])}>
                      {r.tool_calls}
                      {r.tool_errors > 0 && <span className="ml-1 text-err">({r.tool_errors})</span>}
                    </td>
                    <td className={cx("mono px-3 py-2.5 text-[12.5px] text-fg-2", COLS[7][1])}>{fmtTokens(totalTokens(r))}</td>
                    <td className="whitespace-nowrap px-3 py-2.5 pr-4 text-right">
                      <span className={cx("mono inline-flex items-center gap-1.5 whitespace-nowrap text-[11.5px]", s.cls)}><StatusDot status={s.dot} />{s.label}</span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
