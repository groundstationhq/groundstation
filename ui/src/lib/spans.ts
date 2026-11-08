import { attr, num, str, type Event, type TrajectoryDetail } from "./types";
import type { Kind } from "@/components/ui/primitives";

/**
 * A row in the trajectory view: one event, or a started/completed pair folded into a span.
 * Mirrors the pairing logic in `crates/groundstation/src/render.rs`.
 */
export interface Row {
  id: string;
  kind: Kind;
  eventKind: string;
  t0: number; // ms since trajectory start
  durationMs: number | null; // null for instantaneous events or spans still open
  running: boolean;
  failed: boolean;
  name: string; // tool name, model id, "user", "subagent", …
  detail: string; // path, command, prompt, …
  dims: string[]; // right-aligned mono columns
  open: Event;
  close?: Event;
  depth: number; // 1 when part of a subagent sidechain
  tokens?: { in: number; cacheWrite: number; cacheRead: number; out: number };
  exitCode?: number;
}

const ms = (iso: string) => new Date(iso).getTime();

function toolDetail(e: Event): string {
  const cmd = str(e, attr.SHELL_COMMAND);
  if (cmd) return cmd;
  const path = str(e, attr.FILE_PATH);
  if (path) return path.split("/").slice(-2).join("/");
  const pat = str(e, attr.SEARCH_PATTERN);
  if (pat) return `"${pat}"`;
  const url = str(e, attr.HTTP_URL);
  if (url) return url;
  const input = e.attributes?.[attr.TOOL_INPUT];
  if (input && typeof input === "object") {
    const o = input as Record<string, unknown>;
    for (const k of ["command", "file_path", "path", "pattern", "query", "url", "description", "prompt"]) {
      if (typeof o[k] === "string") return (o[k] as string).split("\n")[0].slice(0, 120);
    }
  }
  return "";
}

export function buildRows(d: TrajectoryDetail): Row[] {
  const start = ms(d.started_at);
  const events = [...d.events].sort((a, b) => ms(a.timestamp) - ms(b.timestamp));
  const closers = new Map<string, Event>();
  for (const e of events) {
    if ((e.kind === "tool.completed" || e.kind === "tool.failed" || e.kind === "model.completed" || e.kind === "subagent.completed") && e.span_id) closers.set(e.span_id, e);
  }
  const opened = new Set(events.filter((e) => e.kind.endsWith(".started") && e.span_id).map((e) => e.span_id as string));
  const rows: Row[] = [];
  const now = Date.now();
  const isRunning = d.status === "running";
  // How long a span still open has run. The machine that recorded it may
  // run ahead of this browser's clock; then it's unknown, never negative.
  const since = (iso: string) => {
    const d = now - ms(iso);
    return d >= 0 ? d : null;
  };

  for (const e of events) {
    const t0 = ms(e.timestamp) - start;
    const depth = e.attributes?.[attr.SIDECHAIN] ? 1 : 0;
    const base = { id: e.id, eventKind: e.kind, t0, open: e, depth, running: false, failed: false, durationMs: null as number | null, dims: [] as string[] };

    switch (e.kind) {
      case "turn.user": {
        const p = str(e, attr.PROMPT_TEXT) ?? (num(e, attr.PROMPT_BYTES) != null ? `(prompt excluded · ${num(e, attr.PROMPT_BYTES)} bytes)` : "(prompt not captured)");
        rows.push({ ...base, kind: "user", name: "user", detail: p.split("\n")[0] });
        break;
      }
      case "tool.started": {
        const close = e.span_id ? closers.get(e.span_id) : undefined;
        const dur = close ? ms(close.timestamp) - ms(e.timestamp) : isRunning ? since(e.timestamp) : null;
        const src = close ?? e;
        const exit = num(src, attr.SHELL_EXIT_CODE);
        const failed = close?.kind === "tool.failed" || (exit != null && exit !== 0);
        const dims: string[] = [];
        const outB = num(src, attr.TOOL_OUTPUT_BYTES);
        if (outB != null) dims.push(fmtBytes(outB));
        if (exit != null) dims.push(`exit ${exit}`);
        rows.push({ ...base, kind: "tool", name: str(e, attr.GEN_AI_TOOL_NAME) ?? "tool", detail: toolDetail(e), durationMs: dur, running: !close && isRunning, failed, dims, close, exitCode: exit });
        break;
      }
      case "tool.completed":
      case "tool.failed": {
        if (e.span_id && opened.has(e.span_id)) break; // folded into its tool.started
        const exit = num(e, attr.SHELL_EXIT_CODE);
        const dims: string[] = [];
        if (exit != null) dims.push(`exit ${exit}`);
        rows.push({ ...base, kind: "tool", name: str(e, attr.GEN_AI_TOOL_NAME) ?? "tool", detail: toolDetail(e), durationMs: num(e, attr.DURATION_MS) ?? null, failed: e.kind === "tool.failed" || (exit != null && exit !== 0), dims, exitCode: exit });
        break;
      }
      case "model.started": {
        const close = e.span_id ? closers.get(e.span_id) : undefined;
        if (close) break; // rendered from model.completed
        rows.push({ ...base, kind: "model", name: str(e, attr.GEN_AI_RESPONSE_MODEL) ?? "model", detail: "", running: isRunning, durationMs: isRunning ? since(e.timestamp) : null });
        break;
      }
      case "model.completed": {
        const tokens = {
          in: num(e, attr.GEN_AI_INPUT_TOKENS) ?? 0,
          cacheWrite: num(e, attr.CACHE_CREATION_TOKENS) ?? 0,
          cacheRead: num(e, attr.CACHE_READ_TOKENS) ?? 0,
          out: num(e, attr.GEN_AI_OUTPUT_TOKENS) ?? 0,
        };
        const dur = num(e, attr.DURATION_MS) ?? null;
        // Stamped when the response finished; the call started `dur` earlier.
        rows.push({ ...base, t0: base.t0 - (dur ?? 0), kind: "model", name: str(e, attr.GEN_AI_RESPONSE_MODEL) ?? "model", detail: "", durationMs: dur, dims: [], tokens });
        break;
      }
      case "subagent.started": {
        const close = e.span_id ? closers.get(e.span_id) : undefined;
        const dur = close ? ms(close.timestamp) - ms(e.timestamp) : isRunning ? since(e.timestamp) : null;
        rows.push({ ...base, kind: "agent", name: "subagent", detail: str(e, attr.SUBAGENT_TYPE) ?? "", durationMs: dur, running: !close && isRunning, close });
        break;
      }
      case "subagent.completed":
        if (!(e.span_id && opened.has(e.span_id))) rows.push({ ...base, kind: "agent", name: "subagent", detail: str(e, attr.SUBAGENT_TYPE) ?? "", durationMs: num(e, attr.DURATION_MS) ?? null });
        break;
      case "agent.completed":
        rows.push({ ...base, kind: "complete", name: "complete", detail: str(e, attr.END_REASON) ?? "" });
        break;
      case "agent.failed":
      case "agent.cancelled":
        rows.push({ ...base, kind: "error", name: e.kind.split(".")[1], detail: str(e, attr.END_REASON) ?? str(e, attr.ERROR_MESSAGE) ?? "" });
        break;
      case "agent.started":
      case "agent.resumed":
        rows.push({ ...base, kind: "user", name: e.kind, detail: str(e, attr.SESSION_SOURCE) ?? "" });
        break;
      case "context.compacted":
        rows.push({ ...base, kind: "model", name: "compacted", detail: str(e, attr.COMPACTION_TRIGGER) ?? "" });
        break;
      case "agent.notification":
        rows.push({ ...base, kind: "user", name: "notification", detail: str(e, attr.NOTIFICATION_MESSAGE) ?? "" });
        break;
      case "turn.completed":
      case "agent.paused":
        break; // structural; not shown as rows
      default:
        rows.push({ ...base, kind: "user", name: e.kind, detail: "" });
    }
  }
  return rows;
}

/** 512 B / 4.2 KB / 1.3 MB, rounded before the unit is chosen. */
export function fmtBytes(n: number): string {
  if (Math.round(n) < 1024) return `${Math.round(n)} B`;
  const tenthsK = Math.round((n / 1024) * 10);
  if (tenthsK < 10240) return `${(tenthsK / 10).toFixed(1)} KB`;
  return `${(n / 1_048_576).toFixed(1)} MB`;
}

/** How long a set of intervals covers, overlaps counted once. */
function covered(spans: Array<[number, number]>, minus: Array<[number, number]> = []): number {
  const merge = (xs: Array<[number, number]>) => {
    const out: Array<[number, number]> = [];
    for (const [a, b] of [...xs].sort((x, y) => x[0] - y[0])) {
      const last = out[out.length - 1];
      if (last && a <= last[1]) last[1] = Math.max(last[1], b);
      else out.push([a, b]);
    }
    return out;
  };
  const cut = merge(minus);
  let total = 0;
  for (const [a, b] of merge(spans)) {
    let len = b - a;
    for (const [c, d] of cut) len -= Math.max(0, Math.min(b, d) - Math.max(a, c));
    total += Math.max(0, len);
  }
  return total;
}

/**
 * Aggregate for the "where did the time go" strip: how much of the
 * trajectory's time each kind of span covered. Spans that ran at once
 * count once, and time inside a subagent counts as the subagent's, not
 * again as the tool call that started it or the tools it ran.
 */
export function breakdown(rows: Row[]) {
  const spans = { tool: [] as Array<[number, number]>, model: [] as Array<[number, number]>, sub: [] as Array<[number, number]> };
  const byTool = new Map<string, { n: number; ms: number; failed: number }>();
  for (const r of rows) {
    const d = r.durationMs ?? 0;
    if (r.depth > 0) continue; // sidechain time is counted under its subagent
    const span: [number, number] = [r.t0, r.t0 + d];
    if (r.kind === "tool") {
      spans.tool.push(span);
      const key = r.name === "Bash" || r.name === "shell" ? `${r.name} · ${r.detail.split(" ").slice(0, 2).join(" ")}` : r.name;
      const b = byTool.get(key) ?? { n: 0, ms: 0, failed: 0 };
      b.n++; b.ms += d; if (r.failed) b.failed++;
      byTool.set(key, b);
    } else if (r.kind === "model") spans.model.push(span);
    else if (r.kind === "agent") spans.sub.push(span);
  }
  const top = [...byTool.entries()].map(([k, v]) => ({ key: k, ...v })).sort((a, b) => b.ms - a.ms);
  const sub = covered(spans.sub);
  const tool = covered(spans.tool, spans.sub);
  const model = covered(spans.model, [...spans.sub, ...spans.tool]);
  return { tool, model, sub, top };
}
