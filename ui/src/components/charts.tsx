/** Small SVG charts for the dashboard. One axis, thin marks, recessive grid (see STYLEGUIDE §10). */
import { cx } from "@/lib/format";

export function Bars({ values, labels, highlight, tone = "model", ariaLabel, height = 64, titles }: { values: number[]; labels?: [string, string, string]; highlight?: (i: number) => boolean; tone?: "model" | "tool" | "neutral"; ariaLabel: string; height?: number; titles?: string[] }) {
  const max = Math.max(1, ...values);
  const fill = tone === "model" ? "bg-model/70" : tone === "tool" ? "bg-tool/70" : "bg-fg-4";
  return (
    <div>
      <div className="flex items-end gap-[2px]" style={{ height }} role="img" aria-label={ariaLabel}>
        {values.map((v, i) => (
          <div key={i} className="flex h-full flex-1 items-end" title={titles?.[i]}>
            <div className={cx("w-full rounded-t-[2px] transition-opacity hover:opacity-80", highlight?.(i) ? "bg-warn/80" : fill)} style={{ height: `${(v / max) * 100}%`, minHeight: v > 0 ? 2 : 0 }} />
          </div>
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

/** A line over evenly spaced points. `null` leaves a gap; `titles` give each point a hover readout. */
export function Line({ values, min = 0, max = 1, marks, ariaLabel, height = 64, tone = "var(--color-model)", titles }: { values: Array<number | null>; min?: number; max?: number; marks?: number[]; ariaLabel: string; height?: number; tone?: string; titles?: string[] }) {
  const W = 100, H = 30;
  const n = Math.max(1, values.length - 1);
  const x = (i: number) => (i / n) * W;
  const y = (v: number) => H - 2 - ((v - min) / (max - min)) * (H - 4);
  const d = values.map((v, i) => (v == null ? "" : `${i === 0 || values[i - 1] == null ? "M" : "L"}${x(i).toFixed(2)} ${y(v).toFixed(2)}`)).join(" ");
  const lonely = values.map((v, i) => ({ v, i })).filter(({ v, i }) => v != null && values[i - 1] == null && values[i + 1] == null);
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="w-full" style={{ height }} preserveAspectRatio="none" role="img" aria-label={ariaLabel}>
      {[0.25, 0.5, 0.75].map((g) => <line key={g} x1="0" x2={W} y1={H - 2 - g * (H - 4)} y2={H - 2 - g * (H - 4)} stroke="var(--color-line)" strokeWidth="0.3" vectorEffect="non-scaling-stroke" />)}
      <path d={d} fill="none" stroke={tone} strokeWidth="1.2" vectorEffect="non-scaling-stroke" />
      {lonely.map(({ v, i }) => <circle key={i} cx={x(i)} cy={y(v as number)} r="1.2" fill={tone} />)}
      {marks?.map((i) => values[i] != null && <circle key={i} cx={x(i)} cy={y(values[i] as number)} r="1.6" fill="var(--color-warn)" />)}
      {titles?.map((t, i) => (
        <rect key={i} x={x(i) - W / n / 2} y="0" width={W / n} height={H} fill="transparent"><title>{t}</title></rect>
      ))}
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
