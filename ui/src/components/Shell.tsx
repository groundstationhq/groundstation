import type { ReactNode } from "react";
import { StatusDot } from "@/components/ui/primitives";
import { health } from "@/lib/api";
import { useAsync } from "@/lib/use-async";
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
  const h = useAsync((s) => health(s), [], 10_000);
  const demo = h.status === "ok" && h.data.source === "demo";
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
              demo ? (
                <span className="flex items-center gap-1.5 whitespace-nowrap rounded-full border border-warn/40 bg-warn/10 px-2 py-0.5 text-warn">
                  <StatusDot status="warn" /> demo data<span className="hidden md:inline"> · no gsd on 127.0.0.1:4318</span>
                </span>
              ) : (
                <>
                  <span className="hidden sm:inline">gsd {h.data.data.version}</span>
                  <span className="hidden text-fg-4 sm:inline">·</span>
                  <span className="hidden sm:inline">{fmtInt(h.data.data.events)} events</span>
                  <span className="flex items-center gap-1.5 text-ok"><StatusDot status="ok" /> {h.data.data.upload.endpoint ? "cloud" : "local-only"}</span>
                </>
              )
            ) : (
              <span className="flex items-center gap-1.5"><StatusDot status="idle" /> connecting…</span>
            )}
          </div>
        </div>
      </header>
      <main className="mx-auto max-w-[1400px] px-4 py-6 sm:px-6">{children}</main>
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
