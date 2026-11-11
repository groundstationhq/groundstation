import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { StatusDot } from "@/components/ui/primitives";
import { ApiError, health, onUnauthorized } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
import { HostContext, hostOf } from "@/lib/host";
import type { Health } from "@/lib/types";
import { cx, fmtInt } from "@/lib/format";

export function Mark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden>
      <rect width="32" height="32" rx="7" fill="#151619" stroke="rgba(255,255,255,0.12)" />
      <path d="M7 25.5 H25" stroke="rgba(255,255,255,0.3)" strokeWidth="1.6" strokeLinecap="round" />
      <path d="M8 10.5 A11.5 11.5 0 0 0 21.5 24" stroke="#ededef" strokeWidth="2.8" fill="none" strokeLinecap="round" />
      <path d="M14.2 17.8 L12 25.5" stroke="#ededef" strokeWidth="2.2" strokeLinecap="round" />
      <circle cx="24" cy="8" r="3" fill="var(--color-model)" />
    </svg>
  );
}

/** `feature`: shown only when `/v1/health` lists it; `hide`: left out otherwise rather than greyed. */
const NAV: Array<{ href: string; label: string; feature?: string; hide?: boolean; soon?: boolean }> = [
  { href: "#/", label: "Overview" },
  { href: "#/trajectories", label: "Trajectories" },
  { href: "#/findings", label: "Findings", feature: "insights" },
  { href: "#/agents", label: "Agents", soon: true },
  { href: "#/tokens", label: "Tokens", feature: "tokens", hide: true },
];

function current(href: string, route: string): boolean {
  if (href === "#/") return route === "#/" || route === "";
  return route.startsWith(href) || (href === "#/trajectories" && route.startsWith("#/t/"));
}

function Nav({ route, features, className }: { route: string; features: string[]; className: string }) {
  return (
    <nav className={className} aria-label="Primary">
      {NAV.map((n) => {
        const enabled = !n.soon && (!n.feature || features.includes(n.feature));
        if (!enabled && n.hide) return null;
        return enabled ? (
          <a key={n.href} href={n.href} aria-current={current(n.href, route) ? "page" : undefined} className={cx("whitespace-nowrap rounded-md px-2.5 py-1.5 text-[12.5px] transition-colors hover:bg-bg-3 hover:text-fg", "aria-[current=page]:text-fg text-fg-3")}>
            {n.label}
          </a>
        ) : (
          <span key={n.href} className="cursor-not-allowed whitespace-nowrap px-2.5 py-1.5 text-[12.5px] text-fg-4" title={n.soon ? "Coming in v2" : "Findings come from the hosted backend"}>
            {n.label}
          </span>
        );
      })}
    </nav>
  );
}

export function Shell({ children, route }: { children: ReactNode; route: string }) {
  const [attempt, setAttempt] = useState(0);
  // When a call was refused for want of a session.
  const [signedOutAt, setSignedOutAt] = useState<number | null>(null);
  useEffect(() => onUnauthorized(() => setSignedOutAt((at) => at ?? Date.now())), []);
  const h = useAsync((s) => {
    const sent = Date.now();
    return health(s).then((data) => ({ data, sent }));
  }, [attempt], 10_000);
  const answer = h.status === "ok" ? h.data : null;
  // The last good answer. A poll that fails after it leaves the page as it
  // is, under a banner, so nothing on it is lost (a token shown only once).
  const last = useRef<Health | null>(null);
  if (answer) last.current = answer.data;
  const data = last.current;
  // Signed in again, in another tab: a poll sent after the refusal answered.
  useEffect(() => {
    if (answer && signedOutAt != null && answer.sent > signedOutAt) setSignedOutAt(null);
  }, [answer, signedOutAt]);
  // Which host served this page, kept across a failed poll; before the first
  // answer, a page from a loopback address is gsd's.
  const hosted = useRef(!["127.0.0.1", "localhost", "[::1]"].includes(location.hostname));
  if (data) hosted.current = data.deployment === "hosted";
  // Health is polled; the routes only need to hear when the host itself changes.
  const hostKey = data ? `${data.deployment ?? "local"} ${(data.features ?? []).join(" ")}` : "";
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const host = useMemo(() => (data ? hostOf(data) : { hosted: false, features: [] }), [hostKey]);
  const features = host.features;
  const failure = h.status === "error" ? h.cause : null;
  const suspended = failure instanceof ApiError && failure.status === 403;
  const retry = () => setAttempt((n) => n + 1);

  if (signedOutAt != null) return <SignIn />;
  if (h.status === "error" && (!data || suspended)) return <Unreachable cause={failure} hosted={hosted.current} onRetry={retry} />;
  return (
    <div className="min-h-dvh">
      <header className="sticky top-0 z-40 border-b border-line bg-bg/85 backdrop-blur-md">
        <div className="mx-auto flex h-12 max-w-[1400px] items-center justify-between px-4 sm:px-6">
          <div className="flex items-center gap-5">
            <a href="#/" className="flex items-center gap-2.5" aria-label="Ground Station home">
              <Mark />
              <span className="whitespace-nowrap text-[14px] font-semibold tracking-tight text-fg">Ground Station</span>
            </a>
            <Nav route={route} features={features} className="hidden items-center gap-0.5 sm:flex" />
          </div>
          <div className="mono flex items-center gap-3 text-[11px] text-fg-3">
            {data ? (
              <>
                <span className="hidden sm:inline">{data.deployment === "hosted" ? "Ground Station" : "gsd"} {data.version}</span>
                {data.events != null && (
                  <>
                    <span className="hidden text-fg-4 sm:inline">·</span>
                    <span className="hidden sm:inline">{fmtInt(data.events)} events</span>
                  </>
                )}
                <Connection data={data} />
                {data.deployment === "hosted" && (
                  <a href="/auth/logout" className="text-fg-3 transition-colors hover:text-fg">Sign out</a>
                )}
              </>
            ) : (
              <span className="flex items-center gap-1.5"><StatusDot status="idle" /> connecting…</span>
            )}
          </div>
        </div>
        <Nav route={route} features={features} className="scroll-thin flex items-center gap-0.5 overflow-x-auto px-2 pb-1.5 sm:hidden" />
        {failure != null && <Stale cause={failure} hosted={hosted.current} onRetry={retry} />}
      </header>
      <main className="mx-auto max-w-[1400px] px-4 py-6 sm:px-6">
        {data ? <HostContext.Provider value={host}>{children}</HostContext.Provider> : <div className="label py-20 text-center">loading…</div>}
      </main>
    </div>
  );
}

/** A health poll failed after one worked: the page stays, and says so. */
function Stale({ cause, hosted, onRetry }: { cause: unknown; hosted: boolean; onRetry: () => void }) {
  const status = cause instanceof ApiError ? cause.status : null;
  const what = status == null ? `Can't reach ${hosted ? "Ground Station" : "gsd"}` : `HTTP ${status} · ${cause instanceof Error ? cause.message : String(cause)}`;
  return (
    <div role="status" className="mono flex flex-wrap items-center justify-center gap-x-3 gap-y-1 border-t border-line bg-bg-2 px-4 py-1.5 text-[11px] text-warn">
      <span>{what}. This page shows what was last loaded and tries again every 10s.</span>
      <button type="button" onClick={onRetry} className="text-fg-2 underline-offset-2 hover:text-fg hover:underline">Retry now</button>
    </div>
  );
}

export function Empty({ title, body }: { title: string; body: ReactNode }) {
  return (
    <div className="rounded-lg border border-dashed border-line-2 px-6 py-14 text-center">
      <div className="text-[14px] text-fg">{title}</div>
      <div className="mx-auto mt-2 max-w-[460px] text-[12.5px] leading-[1.55] text-fg-3">{body}</div>
    </div>
  );
}

/** Where the data lives, and whether uploads are failing. */
function Connection({ data }: { data: Health }) {
  if (data.deployment === "hosted") {
    return <span className="flex items-center gap-1.5 text-ok"><StatusDot status="ok" /> hosted</span>;
  }
  const up = data.upload;
  if (up?.last_error) {
    const err = up.last_error;
    const at = new Date(err.at).toLocaleTimeString([], { hour12: false });
    return (
      <span className="flex items-center gap-1.5 text-warn" title={`Last upload failed at ${at}: ${err.message}`}>
        <StatusDot status="warn" /> upload failing<span className="hidden md:inline"> · {fmtInt(up.pending)} pending</span>
      </span>
    );
  }
  return <span className="flex items-center gap-1.5 text-ok"><StatusDot status="ok" /> {up?.mode === "cloud" ? "cloud" : "local-only"}</span>;
}

/** Full-page screen for states where the app has nothing to show. */
function Gate({ children }: { children: ReactNode }) {
  return (
    <main className="flex min-h-dvh items-center justify-center px-4">
      <div className="w-full max-w-[420px] text-center">
        <div className="flex justify-center"><Mark size={32} /></div>
        {children}
      </div>
    </main>
  );
}

const ACTION = "inline-flex items-center rounded-md border border-line-2 bg-bg-2 px-3 py-1.5 text-[12.5px] text-fg transition-colors hover:bg-bg-3 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-model";

function SignIn() {
  // Back to this page afterwards: a shared trajectory link, say.
  const back = `/auth/login?return_to=${encodeURIComponent(`/${location.hash}`)}`;
  return (
    <Gate>
      <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">Sign in to Ground Station</h1>
      <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">You're signed out, or your session expired.</p>
      <div className="mt-6"><a href={back} className={ACTION}>Sign in</a></div>
    </Gate>
  );
}

function Unreachable({ cause, hosted, onRetry }: { cause: unknown; hosted: boolean; onRetry: () => void }) {
  const status = cause instanceof ApiError ? cause.status : null;
  const message = cause instanceof Error ? cause.message : String(cause);
  return (
    <Gate>
      {status === null && hosted ? (
        <>
          <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">Can't reach Ground Station</h1>
          <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">
            Nothing answered at <span className="mono text-fg-2">{location.host}</span>. Check your connection; this page tries again on its own.
          </p>
        </>
      ) : status === null ? (
        <>
          <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">Can't reach gsd</h1>
          <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">
            Nothing answered at <span className="mono text-fg-2">{location.host}</span>. Start the daemon with <span className="mono text-fg-2">groundstation daemon start</span>, then retry.
          </p>
        </>
      ) : status === 403 && hosted ? (
        <>
          <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">This workspace is suspended</h1>
          <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">Ask its administrator, or sign out and into another workspace.</p>
          <div className="mt-6"><a href="/auth/logout" className={ACTION}>Sign out</a></div>
        </>
      ) : (
        <>
          <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">The API returned an error</h1>
          <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">
            <span className="mono text-err">HTTP {status}</span> · {message}
          </p>
        </>
      )}
      {!(status === 403 && hosted) && (
        <>
          <div className="mt-6"><button type="button" onClick={onRetry} className={ACTION}>Retry</button></div>
          <p className="mono mt-3 text-[11px] text-fg-4">retrying every 10s</p>
        </>
      )}
    </Gate>
  );
}
