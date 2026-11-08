export function fmtInt(n: number): string {
  return Math.round(n).toLocaleString("en-US");
}

/** 612 / 18.2k / 298k / 2.1M / 84M. Rounds to what it shows before picking
 * the unit, so 999,600 reads 1.0M, never 1000k. */
export function fmtTokens(n: number): string {
  if (Math.round(n) < 1000) return fmtInt(n);
  const tenthsK = Math.round(n / 100);
  if (tenthsK < 1000) return `${(tenthsK / 10).toFixed(1)}k`;
  const k = Math.round(n / 1000);
  if (k < 1000) return `${k}k`;
  const tenthsM = Math.round(n / 100_000);
  if (tenthsM < 100) return `${(tenthsM / 10).toFixed(1)}M`;
  return `${Math.round(n / 1_000_000)}M`;
}

/** $0.39 / $0.061 / $742 (STYLEGUIDE §6): three decimals under a dime, none
 * from $100, rounded before the choice is made. */
export function fmtUsd(n: number): string {
  if (n > 0 && n < 0.0995) return `$${n.toFixed(3)}`;
  if (n < 99.995) return `$${n.toFixed(2)}`;
  return `$${n.toFixed(0)}`;
}

/** A share as a percent: 61%, 2.1%, <0.1% (STYLEGUIDE §6). */
export function fmtPct(fraction: number): string {
  const p = fraction * 100;
  if (p > 0 && p < 0.05) return "<0.1%";
  if (p > 0 && p < 9.95) return `${p.toFixed(1)}%`;
  return `${Math.round(p)}%`;
}

/** HH:MM:SS.mmm in local time, the same in every locale. */
export function fmtStamp(iso: string): string {
  const d = new Date(iso);
  const two = (n: number) => String(n).padStart(2, "0");
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}

/**
 * A trajectory id short enough for a sentence, as the worker shortens it in
 * finding text: the first 7 characters of a UUID or hash, or of what follows
 * a prefix such as `ses_`; an id a person chose stays whole up to 24.
 */
export function shortId(id: string): string {
  const m = id.match(/^([a-z]+_)(.{9,})$/);
  const prefix = m ? m[1].length : 0;
  const rest = id.slice(prefix);
  const hex = rest.length > 8 && /^[0-9a-fA-F]{8}/.test(rest);
  const keep = prefix ? prefix + 7 : hex ? 7 : 24;
  return [...id].slice(0, keep).join("");
}

/** Nearest-rank percentile of an ascending list, as both hosts compute p50 and p95. */
export function percentile(sorted: number[], p: number): number {
  if (sorted.length === 0) return 0;
  return sorted[Math.min(sorted.length, Math.max(1, Math.ceil(p * sorted.length))) - 1];
}

/** Timestamps as instants, so `…:05Z` and `…:05.123Z` order correctly. */
export function byTime(a: string, b: string): number {
  return Date.parse(a) - Date.parse(b);
}

/** mm:ss or h:mm:ss */
export function fmtClock(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m).padStart(2, "0");
  return `${h > 0 ? `${h}:` : ""}${mm}:${String(r).padStart(2, "0")}`;
}

/** 34ms / 47.2s / 4m 18s / 1h 03m. Rounds to what it shows before picking
 * the unit, so 59,960ms reads 1m 00s, never 60.0s or 59m 60s. */
export function fmtDur(ms: number): string {
  const v = Math.max(0, ms);
  if (Math.round(v) < 1000) return `${Math.round(v)}ms`;
  const tenths = Math.round(v / 100);
  if (tenths < 600) return `${(tenths / 10).toFixed(1)}s`;
  const secs = Math.round(v / 1000);
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${String(secs % 60).padStart(2, "0")}s`;
  const mins = Math.round(v / 60_000);
  return `${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, "0")}m`;
}

export function pct(n: number, digits = 0): string {
  return `${n.toFixed(digits)}%`;
}

export const cx = (...c: Array<string | false | null | undefined>) => c.filter(Boolean).join(" ");

/** Shorten a home-directory prefix to ~, matching the CLI's rendering. */
export function tilde(path: string): string {
  return path.replace(/^\/(Users|home)\/[^/]+/, "~");
}

/** A commit hash shortened for display, the way git abbreviates it. */
export function shortRev(rev: string): string {
  return rev.slice(0, 7);
}

/** "just now", "4m ago", "3h ago", "2d ago". */
export function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
