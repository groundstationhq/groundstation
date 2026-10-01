import type { Health, ToolStats, TrajectoryDetail, TrajectorySummary } from "./types";

/**
 * Client for the `/v1` API on the origin that served the UI: gsd locally, the
 * hosted backend later. Local sends no credentials; hosted relies on its cookie.
 */
const BASE = "";

/** A failed API call. `status` is null when no response arrived at all. */
export class ApiError extends Error {
  constructor(readonly status: number | null, message: string) {
    super(message);
    this.name = "ApiError";
  }
}

// A 401 from any call means the session is gone, wherever it happened, so the
// app as a whole switches to the login screen.
const unauthorized = new Set<() => void>();

export function onUnauthorized(fn: () => void): () => void {
  unauthorized.add(fn);
  return () => unauthorized.delete(fn);
}

async function get<T>(path: string, signal?: AbortSignal): Promise<T> {
  let r: Response;
  try {
    r = await fetch(`${BASE}${path}`, { signal, headers: { accept: "application/json" } });
  } catch (e) {
    if (signal?.aborted) throw e;
    throw new ApiError(null, e instanceof Error ? e.message : String(e));
  }
  if (!r.ok) {
    let msg = r.statusText;
    try { msg = ((await r.json()) as { error?: string }).error ?? msg; } catch { /* ignore */ }
    if (r.status === 401) unauthorized.forEach((fn) => fn());
    throw new ApiError(r.status, msg);
  }
  return (await r.json()) as T;
}

export function health(signal?: AbortSignal): Promise<Health> {
  return get<Health>("/v1/health", signal);
}

export function trajectories(limit = 50, signal?: AbortSignal): Promise<TrajectorySummary[]> {
  return get<TrajectorySummary[]>(`/v1/trajectories?limit=${limit}`, signal);
}

export function trajectory(id: string, signal?: AbortSignal): Promise<TrajectoryDetail> {
  return get<TrajectoryDetail>(`/v1/trajectories/${encodeURIComponent(id)}`, signal);
}

export function toolStats(days = 14, signal?: AbortSignal): Promise<ToolStats> {
  return get<ToolStats>(`/v1/stats/tools?days=${days}`, signal);
}
