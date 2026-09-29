/** Small SVG charts for the dashboard. One axis, thin marks, recessive grid (see STYLEGUIDE §10). */
import { cx } from "@/lib/format";

export function Bars({ values, labels, highlight, tone = "model", ariaLabel, height = 64 }: { values: number[]; labels?: [string, string, string]; highlight?: (i: number) => boolean; tone?: "model" | "tool" | "neutral"; ariaLabel: string; height?: number }) {
  const max = Math.max(1, ...values);
  const fill = tone === "model" ? "bg-model/70" : tone === "tool" ? "bg-tool/70" : "bg-fg-4";
  return (
    <div>
      <div className="flex items-end gap-[2px]" style={{ height }} role="img" aria-label={ariaLabel}>
        {values.map((v, i) => (
          <div key={i} className={cx("flex-1 rounded-t-[2px]", highlight?.(i) ? "bg-warn/80" : fill)} style={{ height: `${(v / max) * 100}%` }} />
        ))}
      </div>
      {labels && (
        <div className="mono mt-1 flex justify-between text-[10px] text-fg-4">
          <span>{labels[0]}</span><span className="text-fg-3">{labels[1]}</span><span>{labels[2]}</span>
        </div>
      )}
    </div>
  );
}

export function Line({ values, min = 0, max = 1, marks, ariaLabel, height = 64, tone = "var(--color-model)" }: { values: number[]; min?: number; max?: number; marks?: number[]; ariaLabel: string; height?: number; tone?: string }) {
  const W = 100, H = 30;
  const x = (i: number) => (i / Math.max(1, values.length - 1)) * W;
  const y = (v: number) => H - 2 - ((v - min) / (max - min)) * (H - 4);
  const d = values.map((v, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(2)} ${y(v).toFixed(2)}`).join(" ");
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="w-full" style={{ height }} preserveAspectRatio="none" role="img" aria-label={ariaLabel}>
      {[0.25, 0.5, 0.75].map((g) => <line key={g} x1="0" x2={W} y1={H - 2 - g * (H - 4)} y2={H - 2 - g * (H - 4)} stroke="var(--color-line)" strokeWidth="0.3" vectorEffect="non-scaling-stroke" />)}
      <path d={d} fill="none" stroke={tone} strokeWidth="1.2" vectorEffect="non-scaling-stroke" />
      {marks?.map((i) => <circle key={i} cx={x(i)} cy={y(values[i])} r="1.6" fill="var(--color-warn)" />)}
    </svg>
  );
}

export function Stacked({ parts, ariaLabel }: { parts: Array<{ k: string; v: number; c: string }>; ariaLabel: string }) {
  const total = Math.max(1, parts.reduce((a, p) => a + p.v, 0));
  return (
    <div>
      <div className="flex h-2.5 w-full gap-[2px] overflow-hidden rounded-[3px]" role="img" aria-label={ariaLabel}>
        {parts.map((p) => <div key={p.k} className="h-full rounded-[2px]" style={{ width: `${(p.v / total) * 100}%`, background: p.c }} />)}
      </div>
      <ul className="mono mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[10.5px] text-fg-3">
        {parts.map((p) => (
          <li key={p.k} className="flex items-center gap-1.5"><span className="inline-block h-1.5 w-1.5 rounded-[1px]" style={{ background: p.c }} aria-hidden />{p.k} <span className="text-fg-2">{Math.round((p.v / total) * 100)}%</span></li>
        ))}
      </ul>
    </div>
  );
}

export function HBars({ rows, ariaLabel, log }: { rows: Array<{ k: string; p50: number; p95: number; fmt: (n: number) => string }>; ariaLabel: string; log?: boolean }) {
  const max = Math.max(1, ...rows.map((r) => r.p95));
  const sc = (v: number) => Math.max(1.5, (log ? Math.log10(v + 1) / Math.log10(max + 1) : v / max) * 100);
  return (
    <div className="mono space-y-1.5 text-[10.5px]" role="img" aria-label={ariaLabel}>
      {rows.map((r) => (
        <div key={r.k} className="grid grid-cols-[76px_minmax(0,1fr)_56px] items-center gap-2">
          <span className="truncate text-fg-3">{r.k}</span>
          <div className="relative h-2 rounded-[2px] bg-bg-3">
            <div className="absolute inset-y-0 left-0 rounded-[2px] bg-tool/40" style={{ width: `${sc(r.p95)}%` }} />
            <div className="absolute inset-y-0 left-0 rounded-[2px] bg-tool" style={{ width: `${sc(r.p50)}%` }} />
          </div>
          <span className="text-right text-fg-2">{r.fmt(r.p95)}</span>
        </div>
      ))}
      <div className="flex gap-3 pt-0.5 text-fg-4"><span><span className="text-tool">■</span> p50</span><span><span className="text-tool/40">■</span> p95{log ? " · log scale" : ""}</span></div>
    </div>
  );
}
