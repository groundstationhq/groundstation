import { DEMO, DEMO_HEALTH, DEMO_TOOL_STATS } from "./fixtures";
import type { Health, ToolStats, TrajectoryDetail, TrajectorySummary } from "./types";

/**
 * Client for gsd's local API. Falls back to demo fixtures when no daemon answers,
 * and says so through `source` so the UI can show a notice.
 */
export type Source = "gsd" | "demo";

const BASE = "";

async function get<T>(path: string, signal?: AbortSignal): Promise<T> {
  const r = await fetch(`${BASE}${path}`, { signal, headers: { accept: "application/json" } });
  if (!r.ok) {
    let msg = r.statusText;
    try { msg = ((await r.json()) as { error?: string }).error ?? msg; } catch { /* ignore */ }
    throw new Error(msg);
  }
  return (await r.json()) as T;
}

let demoMode: boolean | null = null;

export async function health(signal?: AbortSignal): Promise<{ data: Health; source: Source }> {
  try {
    const data = await get<Health>("/v1/health", signal);
    demoMode = false;
    return { data, source: "gsd" };
  } catch {
    demoMode = true;
    return { data: DEMO_HEALTH, source: "demo" };
  }
}

export async function trajectories(limit = 50, signal?: AbortSignal): Promise<{ data: TrajectorySummary[]; source: Source }> {
  if (demoMode !== true) {
    try {
      const data = await get<TrajectorySummary[]>(`/v1/trajectories?limit=${limit}`, signal);
      demoMode = false;
      return { data, source: "gsd" };
    } catch { demoMode = true; }
  }
  const data = DEMO.map(({ events: _e, ...s }) => s).sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  return { data, source: "demo" };
}

export async function trajectory(id: string, signal?: AbortSignal): Promise<{ data: TrajectoryDetail; source: Source }> {
  if (demoMode !== true) {
    try {
      const data = await get<TrajectoryDetail>(`/v1/trajectories/${encodeURIComponent(id)}`, signal);
      return { data, source: "gsd" };
    } catch (e) {
      if (demoMode === false) throw e;
      demoMode = true;
    }
  }
  const d = DEMO.find((t) => t.id === id);
  if (!d) throw new Error(`no trajectory ${id}`);
  return { data: d, source: "demo" };
}

export async function toolStats(days = 14, signal?: AbortSignal): Promise<{ data: ToolStats; source: Source }> {
  if (demoMode !== true) {
    try {
      const data = await get<ToolStats>(`/v1/stats/tools?days=${days}`, signal);
      return { data, source: "gsd" };
    } catch (e) {
      if (demoMode === false) throw e;
      demoMode = true;
    }
  }
  return { data: DEMO_TOOL_STATS, source: "demo" };
}
