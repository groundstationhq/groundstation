import { useEffect, useState, type ReactNode } from "react";
import { StatusDot } from "@/components/ui/primitives";
import { ApiError, health, onUnauthorized } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
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

const NAV = [
  { href: "#/", label: "Overview", enabled: true },
  { href: "#/trajectories", label: "Trajectories", enabled: true },
  { href: "#/findings", label: "Findings", enabled: false },
  { href: "#/agents", label: "Agents", enabled: false },
];

export function Shell({ children, route }: { children: ReactNode; route: string }) {
  const [attempt, setAttempt] = useState(0);
  const [signedOut, setSignedOut] = useState(false);
  useEffect(() => onUnauthorized(() => setSignedOut(true)), []);
  const h = useAsync((s) => health(s), [attempt], 10_000);

  if (signedOut) return <SignIn />;
  if (h.status === "error") return <Unreachable cause={h.cause} onRetry={() => setAttempt((n) => n + 1)} />;
  return (
    <div className="min-h-dvh">
      <header className="sticky top-0 z-40 border-b border-line bg-bg/85 backdrop-blur-md">
        <div className="mx-auto flex h-12 max-w-[1400px] items-center justify-between px-4 sm:px-6">
          <div className="flex items-center gap-5">
            <a href="#/" className="flex items-center gap-2.5" aria-label="Ground Station home">
              <Mark />
              <span className="whitespace-nowrap text-[14px] font-semibold tracking-tight text-fg">Ground Station</span>
            </a>
            <nav className="hidden items-center gap-0.5 sm:flex" aria-label="Primary">
              {NAV.map((n) =>
                n.enabled ? (
                  <a key={n.href} href={n.href} aria-current={(n.href === "#/" ? route === "#/" || route === "" : route.startsWith(n.href) || (n.href === "#/trajectories" && route.startsWith("#/t/"))) ? "page" : undefined} className={cx("rounded-md px-2.5 py-1.5 text-[12.5px] transition-colors hover:bg-bg-3 hover:text-fg", "aria-[current=page]:text-fg text-fg-3")}>
                    {n.label}
                  </a>
                ) : (
                  <span key={n.href} className="cursor-not-allowed px-2.5 py-1.5 text-[12.5px] text-fg-4" title="Coming in v2">
                    {n.label}
                  </span>
                ),
              )}
            </nav>
          </div>
          <div className="mono flex items-center gap-3 text-[11px] text-fg-3">
            {h.status === "ok" ? (
              <>
                <span className="hidden sm:inline">{h.data.deployment === "hosted" ? "Ground Station" : "gsd"} {h.data.version}</span>
                <span className="hidden text-fg-4 sm:inline">·</span>
                <span className="hidden sm:inline">{fmtInt(h.data.events)} events</span>
                <Connection data={h.data} />
              </>
            ) : (
              <span className="flex items-center gap-1.5"><StatusDot status="idle" /> connecting…</span>
            )}
          </div>
        </div>
      </header>
      <main className="mx-auto max-w-[1400px] px-4 py-6 sm:px-6">{h.status === "ok" ? children : <div className="label py-20 text-center">loading…</div>}</main>
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
  return (
    <Gate>
      <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">Sign in to Ground Station</h1>
      <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">You're signed out, or your session expired.</p>
      <div className="mt-6"><a href="/auth/login" className={ACTION}>Sign in</a></div>
    </Gate>
  );
}

function Unreachable({ cause, onRetry }: { cause: unknown; onRetry: () => void }) {
  const status = cause instanceof ApiError ? cause.status : null;
  const message = cause instanceof Error ? cause.message : String(cause);
  return (
    <Gate>
      {status === null ? (
        <>
          <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">Can't reach gsd</h1>
          <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">
            Nothing answered at <span className="mono text-fg-2">{location.host}</span>. Start the daemon with <span className="mono text-fg-2">groundstation daemon start</span>, then retry.
          </p>
        </>
      ) : (
        <>
          <h1 className="mt-5 text-[17px] font-semibold tracking-tight text-fg">The API returned an error</h1>
          <p className="mt-2 text-[13px] leading-[1.55] text-fg-3">
            <span className="mono text-err">HTTP {status}</span> · {message}
          </p>
        </>
      )}
      <div className="mt-6"><button type="button" onClick={onRetry} className={ACTION}>Retry</button></div>
      <p className="mono mt-3 text-[11px] text-fg-4">retrying every 10s</p>
    </Gate>
  );
}
