/**
 * Demo data in the exact wire shape gsd returns, used when no daemon is reachable.
 * Everything here is invented but plausible; the UI shows a "demo data" notice when it's in use.
 */
import { attr, SCHEMA, type Event, type EventKind, type TrajectoryDetail, type TrajectorySummary } from "./types";

type Spec = {
  id: string;
  agent: string;
  version: string;
  title: string;
  repo: string;
  cwd: string;
  branch: string;
  startedAgoMin: number;
  status: "completed" | "failed" | "running";
  script: Step[];
};
type Step =
  | { t: number; user: string }
  | { t: number; model: string; input: number; cacheRead: number; output: number; ms: number; side?: true }
  | { t: number; tool: string; cat: string; ms: number; cmd?: string; path?: string; pattern?: string; exit?: number; outBytes?: number; failed?: true; side?: true }
  | { t: number; subagent: string; ms: number }
  | { t: number; compacted: string }
  | { t: number; end: "completed" | "failed" | "cancelled"; reason: string };

let seq = 0;
const uuid = () => {
  seq++;
  const h = seq.toString(16).padStart(12, "0");
  return `019270a0-0000-7000-8000-${h}`;
};

function build(spec: Spec): TrajectoryDetail {
  const start = Date.now() - spec.startedAgoMin * 60_000;
  const agent = { name: spec.agent, version: spec.version };
  const ev = (kind: EventKind, t: number, a: Record<string, unknown>, span?: string): Event => ({
    id: uuid(),
    trajectory_id: spec.id,
    kind,
    timestamp: new Date(start + t * 1000).toISOString(),
    agent,
    ...(span ? { span_id: span } : {}),
    attributes: Object.fromEntries(Object.entries(a).filter(([, v]) => v != null)),
  });
  const events: Event[] = [ev("agent.started", 0, { [attr.SESSION_ID]: `ses_${spec.id.slice(-6)}`, [attr.SESSION_SOURCE]: "startup", [attr.CWD]: spec.cwd, [attr.PERMISSION_MODE]: "default" })];
  let s = { user_turns: 0, model_calls: 0, tool_calls: 0, tool_errors: 0, input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 0 };
  let ended: string | null = null;
  let status: string = spec.status;
  let n = 0;
  for (const st of spec.script) {
    if ("user" in st) {
      s.user_turns++;
      events.push(ev("turn.user", st.t, { [attr.PROMPT_TEXT]: st.user, [attr.PROMPT_BYTES]: st.user.length }));
    } else if ("model" in st) {
      const span = `sp_${spec.id.slice(-4)}_${++n}`;
      events.push(ev("model.started", st.t, { [attr.GEN_AI_PROVIDER]: "anthropic", [attr.SIDECHAIN]: st.side ? true : undefined }, span));
      events.push(ev("model.completed", st.t + st.ms / 1000, { [attr.GEN_AI_PROVIDER]: "anthropic", [attr.GEN_AI_RESPONSE_MODEL]: st.model, [attr.GEN_AI_INPUT_TOKENS]: st.input, [attr.CACHE_READ_TOKENS]: st.cacheRead, [attr.GEN_AI_OUTPUT_TOKENS]: st.output, [attr.GEN_AI_FINISH_REASONS]: ["tool_use"], [attr.DURATION_MS]: st.ms, [attr.SIDECHAIN]: st.side ? true : undefined }, span));
      s.model_calls++; s.input_tokens += st.input; s.cache_read_tokens += st.cacheRead; s.output_tokens += st.output;
    } else if ("tool" in st) {
      const span = `sp_${spec.id.slice(-4)}_${++n}`;
      const common = { [attr.GEN_AI_TOOL_NAME]: st.tool, [attr.TOOL_CATEGORY]: st.cat, [attr.SHELL_COMMAND]: st.cmd, [attr.FILE_PATH]: st.path, [attr.SEARCH_PATTERN]: st.pattern, [attr.SIDECHAIN]: st.side ? true : undefined };
      events.push(ev("tool.started", st.t, { ...common, [attr.TOOL_INPUT]: st.cmd ? { command: st.cmd } : st.path ? { file_path: st.path } : st.pattern ? { pattern: st.pattern } : undefined }, span));
      const closeKind: EventKind = st.failed ? "tool.failed" : "tool.completed";
      events.push(ev(closeKind, st.t + st.ms / 1000, { ...common, [attr.DURATION_MS]: st.ms, [attr.SHELL_EXIT_CODE]: st.exit, [attr.TOOL_OUTPUT_BYTES]: st.outBytes, [attr.ERROR_MESSAGE]: st.failed ? "tool returned an error" : undefined }, span));
      s.tool_calls++; if (st.failed || (st.exit != null && st.exit !== 0)) s.tool_errors++;
    } else if ("subagent" in st) {
      const span = `sp_${spec.id.slice(-4)}_${++n}`;
      events.push(ev("subagent.started", st.t, { [attr.SUBAGENT_ID]: `sub_${n}`, [attr.SUBAGENT_TYPE]: st.subagent }, span));
      events.push(ev("subagent.completed", st.t + st.ms / 1000, { [attr.SUBAGENT_ID]: `sub_${n}`, [attr.SUBAGENT_TYPE]: st.subagent, [attr.DURATION_MS]: st.ms }, span));
    } else if ("compacted" in st) {
      events.push(ev("context.compacted", st.t, { [attr.COMPACTION_TRIGGER]: st.compacted }));
    } else if ("end" in st) {
      events.push(ev(`agent.${st.end}`, st.t, { [attr.END_REASON]: st.reason }));
      ended = new Date(start + st.t * 1000).toISOString();
      status = st.end;
    }
  }
  const last = events[events.length - 1].timestamp;
  const duration = (ended ? new Date(ended).getTime() : Date.now()) - start;
  const summary: TrajectorySummary = {
    id: spec.id, agent: spec.agent, agent_version: spec.version, title: spec.title, status,
    cwd: spec.cwd, repository: spec.repo, branch: spec.branch, host: "ana-mbp.local",
    started_at: new Date(start).toISOString(), updated_at: last, ended_at: ended, duration_ms: duration,
    event_count: events.length, ...s,
  };
  return { ...summary, events };
}

const cargo = (t: number, ms: number, exit: number) => ({ t, tool: "Bash", cat: "shell", ms, cmd: "cargo test", exit, outBytes: exit === 0 ? 18_421 : 22_930 });
const read = (t: number, path: string, side?: true) => ({ t, tool: "Read", cat: "file_read", ms: 12 + (t % 7), path, outBytes: 24_310, side });

const FLAKY: Spec = {
  id: "trj_8f3a1c2e", agent: "claude-code", version: "1.9.2", title: "Fix the flaky checkout tests.", repo: "acme/payments", cwd: "/Users/ana/work/payments", branch: "fix/flaky-checkout", startedAgoMin: 14, status: "completed",
  script: [
    { t: 0.2, user: "Fix the flaky checkout tests." },
    { t: 0.6, model: "claude-sonnet-4-5", input: 4_204, cacheRead: 14_000, output: 2_118, ms: 1_820 },
    read(2.6, "/Users/ana/work/payments/tests/checkout_test.rs"),
    { t: 2.7, tool: "Grep", cat: "search", ms: 31, pattern: "timeout", outBytes: 1_204 },
    read(2.8, "/Users/ana/work/payments/src/checkout/session.rs"),
    { t: 3.0, model: "claude-sonnet-4-5", input: 3_402, cacheRead: 18_000, output: 1_388, ms: 1_204 },
    { t: 4.3, tool: "Edit", cat: "file_write", ms: 41, path: "/Users/ana/work/payments/tests/checkout_test.rs" },
    cargo(4.5, 47_193, 101),
    { t: 51.9, model: "claude-sonnet-4-5", input: 6_711, cacheRead: 32_000, output: 1_902, ms: 2_401 },
    { t: 54.4, tool: "Edit", cat: "file_write", ms: 38, path: "/Users/ana/work/payments/src/checkout/session.rs" },
    cargo(54.6, 46_912, 0),
    { t: 101.7, model: "claude-sonnet-4-5", input: 2_130, cacheRead: 40_000, output: 612, ms: 1_102 },
    { t: 103.0, end: "completed", reason: "end_turn" },
  ],
};

const OAUTH: Spec = {
  id: "trj_41a0f7d9", agent: "claude-code", version: "1.9.2", title: "Refactor authentication to use OAuth", repo: "acme/backend", cwd: "/Users/ana/work/backend", branch: "main", startedAgoMin: 62, status: "completed",
  script: [
    { t: 0.2, user: "Refactor authentication to use OAuth" },
    { t: 0.5, model: "claude-opus-4-1", input: 5_718, cacheRead: 18_400, output: 3_204, ms: 4_100 },
    read(4.7, "/Users/ana/work/backend/src/auth.ts"),
    { t: 4.8, tool: "Grep", cat: "search", ms: 87, pattern: "refresh_token", outBytes: 2_410 },
    read(4.9, "/Users/ana/work/backend/src/session/store.ts"),
    { t: 5.2, model: "claude-opus-4-1", input: 3_800, cacheRead: 28_000, output: 2_900, ms: 3_600 },
    { t: 9.0, subagent: "explore", ms: 14_200 },
    { t: 9.1, model: "claude-sonnet-4-5", input: 12_400, cacheRead: 0, output: 800, ms: 1_900, side: true },
    { t: 11.1, tool: "Grep", cat: "search", ms: 41, pattern: "verifyToken(", outBytes: 880, side: true },
    read(11.2, "/Users/ana/work/backend/src/api/users.ts", true),
    { t: 12.0, model: "claude-sonnet-4-5", input: 18_700, cacheRead: 0, output: 500, ms: 1_100, side: true },
    { t: 24.0, model: "claude-opus-4-1", input: 4_200, cacheRead: 34_000, output: 4_400, ms: 5_200 },
    { t: 29.3, tool: "Edit", cat: "file_write", ms: 44, path: "/Users/ana/work/backend/src/auth.ts" },
    { t: 29.4, tool: "Edit", cat: "file_write", ms: 31, path: "/Users/ana/work/backend/src/session/store.ts" },
    { t: 30.0, tool: "Bash", cat: "shell", ms: 41_200, cmd: "npm test", exit: 0, outBytes: 39_014 },
    { t: 71.3, model: "claude-opus-4-1", input: 2_700, cacheRead: 50_000, output: 900, ms: 2_400 },
    { t: 74.0, end: "completed", reason: "end_turn" },
  ],
};

const LOOP: Spec = {
  id: "trj_c2d91e88", agent: "claude-code", version: "1.9.2", title: "Refactor payments module to idempotency keys", repo: "acme/payments", cwd: "/Users/ana/work/payments", branch: "feat/idempotency", startedAgoMin: 19, status: "running",
  script: (() => {
    const s: Step[] = [{ t: 0.2, user: "Refactor payments module to use idempotency keys for refunds." }];
    let t = 0.6;
    for (let i = 0; i < 9; i++) {
      s.push({ t, model: "claude-sonnet-4-5", input: 3_000 + i * 400, cacheRead: 30_000 + i * 12_000, output: 900 + (i % 3) * 300, ms: 6_000 + i * 900 });
      t += 7 + i;
      s.push(read(t, "/Users/ana/work/payments/src/refunds.rs"));
      t += 0.2;
      if (i % 2 === 0) { s.push({ t, tool: "Edit", cat: "file_write", ms: 40, path: "/Users/ana/work/payments/src/refunds.rs" }); t += 0.2; }
      s.push(cargo(t, 47_000 + (i % 4) * 1_300, i === 8 ? 0 : 101));
      t += 48 + (i % 4) * 1.3;
    }
    s.push({ t, compacted: "auto" });
    s.push({ t: t + 0.5, model: "claude-sonnet-4-5", input: 2_000, cacheRead: 61_000, output: 400, ms: 5_000 });
    s.push({ t: t + 6, tool: "Bash", cat: "shell", ms: 12_000, cmd: "cargo test", exit: 101, outBytes: 22_930 });
    return s;
  })(),
};

const DEPS: Spec = {
  id: "trj_9be04c11", agent: "codex", version: "0.42.0", title: "Update dependencies and fix the two breaking changes", repo: "acme/web", cwd: "/Users/ana/work/web", branch: "chore/deps", startedAgoMin: 190, status: "completed",
  script: [
    { t: 0.2, user: "Update dependencies and fix whatever breaks." },
    { t: 0.5, model: "gpt-5-codex", input: 2_100, cacheRead: 0, output: 600, ms: 2_100 },
    { t: 2.8, tool: "exec_command", cat: "shell", ms: 31_400, cmd: "pnpm update --latest", exit: 0, outBytes: 12_003 },
    { t: 34.5, tool: "exec_command", cat: "shell", ms: 18_900, cmd: "pnpm test", exit: 1, outBytes: 8_120 },
    { t: 53.6, model: "gpt-5-codex", input: 9_800, cacheRead: 0, output: 1_400, ms: 4_800 },
    { t: 58.6, tool: "apply_patch", cat: "file_write", ms: 22, path: "/Users/ana/work/web/src/app/checkout/page.tsx" },
    { t: 58.9, tool: "exec_command", cat: "shell", ms: 17_200, cmd: "pnpm test", exit: 0, outBytes: 7_902 },
    { t: 76.3, model: "gpt-5-codex", input: 1_200, cacheRead: 0, output: 300, ms: 1_500 },
    { t: 78.0, end: "completed", reason: "end_turn" },
  ],
};

const FAILED: Spec = {
  id: "trj_5d7e2a90", agent: "claude-code", version: "1.9.2", title: "Fix integration tests after the schema migration", repo: "acme/backend", cwd: "/Users/ana/work/backend", branch: "fix/integration", startedAgoMin: 420, status: "failed",
  script: [
    { t: 0.2, user: "Fix the integration tests, they broke after the schema migration." },
    { t: 0.5, model: "claude-sonnet-4-5", input: 4_000, cacheRead: 20_000, output: 1_800, ms: 2_300 },
    { t: 3.0, tool: "Bash", cat: "shell", ms: 92_000, cmd: "make integration-test", exit: 2, outBytes: 61_210 },
    { t: 95.5, model: "claude-sonnet-4-5", input: 21_000, cacheRead: 24_000, output: 2_000, ms: 6_100 },
    { t: 102.0, tool: "Bash", cat: "shell", ms: 4_000, cmd: "docker compose up -d db", exit: 125, outBytes: 900, failed: true },
    { t: 106.5, model: "claude-sonnet-4-5", input: 1_800, cacheRead: 45_000, output: 700, ms: 2_000 },
    { t: 109.0, end: "failed", reason: "tool_error: docker daemon not running" },
  ],
};

export const DEMO: TrajectoryDetail[] = [LOOP, FLAKY, OAUTH, DEPS, FAILED].map(build);

export const DEMO_HEALTH = {
  status: "ok", version: "0.1.0-demo", schema: SCHEMA, pid: 0, data_dir: "(demo data)",
  trajectories: DEMO.length, events: DEMO.reduce((a, d) => a + d.events.length, 0), spool_pending: 0,
  upload: { endpoint: null, pending: 0 },
};
