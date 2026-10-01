# Ground Station style guide

How Ground Station looks, reads and is written. This applies to the product UI (`ui/`), the landing pages, docs, CLI output and anything else that carries the name. If you are an agent working in this repository, follow it without being asked; if a rule here is wrong for your case, say so in the PR rather than silently deviating.

The short version: **the data is the illustration.** Trajectories, spans, tokens, latency and cost are the graphic material. Nothing decorative that a real product wouldn't ship.

---

## 1. Principles

1. **Infrastructure, not marketing.** Dark, precise, dense where density carries information. No glowing blobs, no glassmorphism, no 3D, no illustrations of robots, no AI purple.
2. **Status is never color alone.** Every kind has a shape, every status has a dot plus a word.
3. **Numbers are honest and typeset.** Tabular figures, right-aligned, consistent units. A number that isn't real gets labelled `est.` or isn't shown.
4. **Content over chrome.** Panels are windows onto data, not floating cards. Borders are hairlines. Radii are small.
5. **Motion explains state.** Something animates because it is running, growing, resolving or aggregating. Never because it is "premium". Always respect `prefers-reduced-motion`.
6. **Say less.** Short sentences an engineer can verify. No superlatives.

---

## 2. Brand

| | |
|:--|:--|
| Product | **Ground Station** (two words, both capitalised; never "GroundStation" or "groundstation" in prose) |
| Category | Agent Platform Monitoring, abbreviated APM only when the category is the subject |
| Domain / CLI / package | `groundstation.sh`, `groundstation`, `groundstation-*` (lowercase, one word) |
| Daemon | `gsd` |
| Tagline | Open-source observability for AI agents. |
| Long line | Turn agent execution into understanding. |

**Logo.** The mark is a dish on a ground line catching a teal signal (`assets/logo.svg`). It sits on its own dark tile so it works on any background. Minimum size 16px. Don't recolour the dish; the signal dot is always `--color-model` teal. The wordmark is Inter Semibold, letter-spacing −0.02em, set next to the mark with a gap of about 0.45× the mark height.

---

## 3. Color

Defined once as CSS custom properties (Tailwind v4 `@theme` in `ui/src/index.css`). Use tokens, never raw hex in components.

### Surfaces (dark-first; there is no light theme yet)

| Token | Value | Use |
|:--|:--|:--|
| `--color-bg` | `#0a0a0b` | page |
| `--color-bg-1` | `#0e0f11` | panels |
| `--color-bg-2` | `#131417` | panel headers, hover rows |
| `--color-bg-3` | `#191a1e` | tracks, wells, inactive bars |
| `--color-bg-4` | `#202126` | meters' empty state, disabled |

### Lines

| Token | Value | Use |
|:--|:--|:--|
| `--color-line` | `rgba(255,255,255,0.08)` | default hairline |
| `--color-line-2` | `rgba(255,255,255,0.13)` | emphasised border, focused panel |
| `--color-line-3` | `rgba(255,255,255,0.22)` | hover border, dividers in dense tables |

### Text

| Token | Value | Use |
|:--|:--|:--|
| `--color-fg` | `#ededef` | primary |
| `--color-fg-2` | `#a4a5ab` | secondary, body copy |
| `--color-fg-3` | `#6f7076` | labels, tertiary, axis text |
| `--color-fg-4` | `#4b4c52` | timestamps, disabled, hints |

### Telemetry semantics (the palette that carries meaning)

| Token | Value | Meaning | Glyph |
|:--|:--|:--|:--|
| `--color-user` | `#ededef` | user / prompt turn | hollow circle ○ |
| `--color-model` | `#3ee0c0` | model call; also the brand accent | filled circle ● |
| `--color-tool` | `#f5b83d` | tool call | square ■ |
| `--color-agent` | `#c39cf7` | subagent | diamond ◆ |
| `--color-ok` | `#58d68d` | success / complete | circle with check |
| `--color-warn` | `#ff9f43` | slow, degraded, over budget | ◆ in labels |
| `--color-err` | `#ff6b6b` | failure, loop, exit ≠ 0 | circle with × / ▲ in labels |

Rules:

- Kind colors are for marks (glyphs, bars, spans), not for text. Text stays in the `--color-fg-*` scale, with a coloured glyph beside it to carry identity. Exceptions: a single emphasised value in a finding (`24m 32s` in warn) and status words next to a dot.
- Model teal doubles as the brand accent (focus rings, active tab underline, primary links). Don't introduce a second accent.
- Violet appears only for subagents. Keep it rare.
- Charts follow the same mapping: model = teal, tool = amber, subagent = violet, infra/other = `--color-fg-4`. Never cycle hues.

---

## 4. Typography

| Role | Family | Notes |
|:--|:--|:--|
| UI and prose | **Inter Variable** | `font-feature-settings: "cv11","ss01","ss03"`; headings letter-spacing −0.02em, `text-wrap: balance` |
| Telemetry, code, identifiers | **JetBrains Mono Variable** | `tabular-nums`, `"zero"` slashed zero, letter-spacing −0.01em |

Self-hosted via `@fontsource-variable/*`. Don't add fonts.

Scale (px): 10 · 10.5 · 11 · 11.5 · 12 · 12.5 · 13 · 13.5 · 14 · 15 · 17 · 20 · 24 · 28 · 34 · 44 · 56. Body in the product is 13px; body on the site is 15–17px. Line-height 1.55–1.6 for prose, 1.1–1.2 for headings.

Fixed treatments:

- **Label** (`.label`): mono, 10–11px, uppercase, tracking 0.08em, `--color-fg-3`. Section eyebrows, column headers, meter captions.
- **Mono value** (`.mono`): any number, id, path, command, model name or event kind. Never set telemetry in Inter.
- Numbers use tabular figures everywhere (`.tnum` or `.mono`). Right-align numeric columns.

---

## 5. Spacing, radius, borders, elevation

- Spacing grid: 4px. Common steps 4 · 8 · 12 · 16 · 24 · 32 · 48 · 80.
- Radius: `2px` marks and bars · `3–4px` chips, rows, inputs · `6px` buttons · `8px` panels. Nothing larger. No pills except status badges (full radius, and only there).
- Borders: 1px hairline in `--color-line`. Panels have a border, not a shadow. The only shadow is the panel's `0 24px 60px -30px rgba(0,0,0,.8)` on the site; the product uses none.
- Panel anatomy: 36px header in `--color-bg-2/70` with a mono breadcrumb on the left (`agents / acme-corp`, `trj_8f3a1c / claude-code / payments`) and status on the right; body; optional footer strip of stats separated by 1px gaps of `--color-line`.
- Page: max width 1200px, gutters 20px (phone) / 32px. The product may go full-width for tables and timelines.

---

## 6. Telemetry conventions

### Glyphs

One shape per kind (see §3). Sizes: 7px in dense rows, 8–9px in tables, 10px in headers. Implemented once in `ui/primitives.tsx` as `Glyph`; don't hand-draw dots.

### Status dot

`StatusDot`: 8px dot. `running` gets an expanding ring animation (disabled under reduced motion). Always paired with a word: Running, Complete, Failed, Slow, Loop?, Idle.

### Formatting (implemented in `ui/src/lib/format.ts`; the CLI's `render.rs` matches)

| What | Rule | Examples |
|:--|:--|:--|
| Duration | `<1s` → `NNms`; `<60s` → `N.Ns`; else `Nm SSs`; hours `Nh MMm` | `34ms`, `47.2s`, `4m 18s`, `1h 03m` |
| Clock offset within a trajectory | `mm:ss`, hours prefixed | `00:04`, `01:49`, `1:12:03` |
| Tokens | `<1k` exact; `<100k` one decimal `k`; `<1M` integer `k`; else `M` | `612`, `18.2k`, `298k`, `84M` |
| Token flow | input → output | `18.2k → 2.1k` |
| Cost | `$` two decimals; under a cent, three | `$0.39`, `$0.061`, `$742` |
| Percent | integer unless < 10% and precision matters | `61%`, `2.1%` |
| Counts | thousands separator | `126,467` |
| Diff | `+N −M` with a true minus sign | `+14 −7` |
| Exit code | `exit N`; non-zero in `--color-err` | `exit 101` |
| Ids | short id = first 6–7 chars after the prefix | `trj_8f3a1c`, `rec_7d2e` |
| Timestamps in streams | `HH:MM:SS.mmm` local | `14:02:59.494` |
| Paths | `~` for home, basename when space is tight | `~/work/payments`, `checkout_test.rs` |

Never show a computed number you can't back with data; prefix estimates with `est.`.

### Span bars

Width is proportional to duration on a linear scale within the view, minimum 2–3px so short calls remain visible. Long tools (≥ 5s) get a bar in the row; running spans fill left to right. Tool bars are amber, model bars teal, failed tool bars red once resolved. The minimap at the top of a trajectory is the whole run as one strip; the expensive span should be obvious before anyone reads a number.

---

## 7. Components

Live in `ui/src/components/ui/primitives.tsx` unless noted. Reuse before creating.

| Component | Purpose | Notes |
|:--|:--|:--|
| `Glyph` | kind marker | shape per kind, `pulse` for running model |
| `StatusDot` | run status | running / ok / warn / err / idle |
| `Eyebrow` | mono label with optional glyph | section and panel captions |
| `Panel` | bordered surface with header/right slot | the only container primitive |
| `Button`, `ButtonLink` | primary (light on dark), secondary (outlined), ghost | 36px / 44px tall, 6px radius |
| `Container` | max-width + gutters | site only |
| Tables | native `<table>`, `label` headers, hairline rows, `hover:bg-bg-2` | numeric columns right-aligned mono |
| Event row | `[clock][glyph][name][detail][dims…]` grid | detail hidden below `sm`, keep the last dim visible |
| Meter | 6px track `--color-bg-4`, fill model teal, warn above 80% | context utilization |
| Finding | header with severity (◆ warning, ▲ critical, ● info) + title + id, KV rows, evidence viz, one-sentence cause | never a generic card |

Interaction: hover raises row background one step; focus uses a 2px `--color-model` outline with 2px offset; expandable rows are `<button aria-expanded>`; drawers close on `Esc`.

---

## 8. Motion

- Easing: `cubic-bezier(0.16, 1, 0.3, 1)` (out-expo) for enters and layout, `cubic-bezier(0.25, 1, 0.5, 1)` for hovers. Durations 150–300ms for UI, 400–700ms for reveals.
- Allowed: rows appearing as events arrive, bars filling while running, counters ticking, a trajectory line drawing on scroll, chips aggregating into a pattern, a drawer sliding.
- Not allowed: background gradients moving, parallax, bouncing, anything looping that doesn't represent a live process.
- Live tickers update at ≤ 30fps for the hero player and ≤ 2fps for tables. Pause when off-screen.
- `prefers-reduced-motion: reduce` shows completed states with no transitions. Every animated view must have that static equivalent.

---

## 9. Accessibility

Semantic HTML, one `h1`, ordered headings. Interactive things are buttons or links, keyboard reachable, with visible focus. Decorative visualizations are `aria-hidden` and the meaningful content exists as text (a `sr-only` list next to a marquee, a caption under a chart). Charts get `role="img"` with an `aria-label` that states the conclusion. Contrast: body text ≥ 4.5:1, labels ≥ 3:1 against their surface (`--color-fg-3` on `--color-bg-1` passes at 11px uppercase mono).

---

## 10. Charts

Follow the data-viz rules: one axis (never dual), thin marks, 2px gaps between stacked segments, recessive grid in `--color-line`, legend whenever there are ≥ 2 series, direct labels for ≤ 4 series, no number on every point. Sequential scales are one hue light→dark; diverging scales use warn ↔ model with a neutral midpoint. Log scales are labelled `· log scale`.

---

## 11. Copy

Voice: a senior infrastructure engineer explaining something to a peer. Short sentences. Verbs. Specific numbers.

Do: "See every tool call." · "Know where the tokens went." · "Slow because the agent re-ran the full test suite after every edit." · "HTTP 200 doesn't tell you what your agent was doing."

Don't: supercharge, revolutionize, unlock, seamless, game-changing, next-generation, AI-powered, journey, empower, delight. No exclamation marks. No emoji in product UI or docs (the README may use none too).

Casing: sentence case for headings and buttons ("Get started", not "Get Started"). Product nouns in code style when they are identifiers (`gsd`, `groundstation connect`), plain when they are concepts (the daemon, a trajectory).

Terminology: **trajectory** (never "session" or "run" in UI), **turn**, **model call**, **tool call**, **subagent**, **context**, **finding** (not "alert" unless it pages someone), **recommendation**.

---

## 12. Code conventions

### UI (`ui/`)

- Vite + React 19 + TypeScript strict + Tailwind v4. `motion` is the only animation dependency. Add a dependency only when it replaces > 100 lines you'd otherwise write.
- Tokens live in `src/index.css` under `@theme`; utilities like `.label`, `.mono`, `.tnum` are defined there once.
- Files: `src/lib/*` pure helpers (format, api, spans, hooks), `src/components/ui/*` primitives, `src/components/*` composites, `src/routes/*` one file per screen. Named exports; `App` is the only default export.
- Types for the wire format mirror `crates/schema` and `crates/api` exactly (`Event`, `EventKind`, `TrajectorySummary`, `TrajectoryDetail`, `Health`). Attribute keys are the same strings as `groundstation_schema::attr`. When the schema changes, the UI types change in the same PR.
- Never render content attributes (`gs.prompt.text`, `gs.tool.input.body`, `gs.tool.output.body`, `gs.shell.command`) without truncation and a way to expand; the daemon may have redacted them, so always handle absence.
- No `any`. No inline hex. No `style=` except for computed geometry (widths from data, transforms).

### Rust (`crates/`)

- Workspace lints are the rule: `unsafe_code = "forbid"`, `clippy::all` warn and CI treats warnings as errors. Edition 2024, MSRV 1.88.
- `anyhow` in binaries, typed errors in library crates. `tracing` for logs; never log a content attribute or a token.
- Attribute keys come from `groundstation_schema::attr`; never write the string literal twice.
- Event ids must be stable across redelivery (derive from the source envelope, never `Uuid::new_v4()` at ingest).

---

## 13. Checklist before merging anything visual

- Uses tokens only; no new colors.
- Kinds have glyphs; statuses have dot + word.
- Numbers are mono, tabular, right-aligned, formatted per §6.
- Works at 390px, 834px and 1440px; no horizontal page overflow.
- Reduced motion shows a complete static state.
- Copy passes §11 (read it aloud; delete adjectives).
