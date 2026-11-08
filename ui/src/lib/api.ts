import type { CreatedToken, ErrorBody, Health, IngestToken, Insight, InsightState, NewToken, StateChange, ToolStats, TrajectoryDetail, TrajectorySummary } from "./types";

/**
 * Client for the `/v1` API on the origin that served the UI: gsd locally, the
 * hosted api otherwise. Local sends no credentials; hosted relies on its
 * session cookie, which same-origin requests carry without any code here.
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

async function request<T>(path: string, init: RequestInit, signal?: AbortSignal): Promise<T> {
  let r: Response;
  try {
    r = await fetch(`${BASE}${path}`, { ...init, signal, headers: { accept: "application/json", ...init.headers } });
  } catch (e) {
    if (signal?.aborted) throw e;
    throw new ApiError(null, e instanceof Error ? e.message : String(e));
  }
  if (!r.ok) {
    // Over HTTP/2 there is no reason phrase; a proxy's own page isn't JSON.
    let msg = r.statusText || `HTTP ${r.status}`;
    try { msg = ((await r.json()) as Partial<ErrorBody>).error ?? msg; } catch { /* ignore */ }
    if (r.status === 401) unauthorized.forEach((fn) => fn());
    throw new ApiError(r.status, msg);
  }
  return r.status === 204 ? (undefined as T) : ((await r.json()) as T);
}

function get<T>(path: string, signal?: AbortSignal): Promise<T> {
  return request<T>(path, { method: "GET" }, signal);
}

/** Writes send JSON: browsers can't send it cross-origin without a CORS preflight, which no host grants. */
function send<T>(method: "POST" | "DELETE", path: string, body?: unknown): Promise<T> {
  return request<T>(path, { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body ?? null) });
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

/** Hosted only: `insights` in `Health.features`. */
export function insights(state: InsightState, signal?: AbortSignal, limit = 200): Promise<Insight[]> {
  return get<Insight[]>(`/v1/insights?state=${state}&limit=${limit}`, signal);
}

export function setInsightState(id: string, state: InsightState): Promise<Insight> {
  const body: StateChange = { state };
  return send<Insight>("POST", `/v1/insights/${encodeURIComponent(id)}/state`, body);
}

/** Hosted only: `tokens` in `Health.features`. */
export function tokens(signal?: AbortSignal): Promise<IngestToken[]> {
  return get<IngestToken[]>("/v1/tokens", signal);
}

export function createToken(label: string): Promise<CreatedToken> {
  const body: NewToken = { label };
  return send<CreatedToken>("POST", "/v1/tokens", body);
}

export function revokeToken(id: string): Promise<void> {
  return send<void>("DELETE", `/v1/tokens/${encodeURIComponent(id)}`);
}
