import type { ReactNode, ButtonHTMLAttributes, AnchorHTMLAttributes } from "react";
import { cx } from "@/lib/format";

export type Kind = "user" | "model" | "tool" | "agent" | "complete" | "error";

export const kindColor: Record<Kind, string> = {
  user: "var(--color-user)",
  model: "var(--color-model)",
  tool: "var(--color-tool)",
  agent: "var(--color-agent)",
  complete: "var(--color-ok)",
  error: "var(--color-err)",
};

/** Node glyph: distinct *shape* per kind so status isn't color-only. */
export function Glyph({ kind, size = 10, className, pulse }: { kind: Kind; size?: number; className?: string; pulse?: boolean }) {
  const c = kindColor[kind];
  const s = size;
  const common = { width: s, height: s, className: cx("shrink-0", className), "aria-hidden": true } as const;
  switch (kind) {
    case "user":
      return (
        <svg {...common} viewBox="0 0 10 10">
          <circle cx="5" cy="5" r="4" fill="none" stroke={c} strokeWidth="1.4" />
        </svg>
      );
    case "model":
      return (
        <svg {...common} viewBox="0 0 10 10" className={cx(common.className, pulse && "animate-pulse-dot")}>
          <circle cx="5" cy="5" r="4.2" fill={c} />
        </svg>
      );
    case "tool":
      return (
        <svg {...common} viewBox="0 0 10 10">
          <rect x="1.2" y="1.2" width="7.6" height="7.6" rx="1.2" fill={c} />
        </svg>
      );
    case "agent":
      return (
        <svg {...common} viewBox="0 0 10 10">
          <path d="M5 0.8 L9.2 5 L5 9.2 L0.8 5 Z" fill={c} />
        </svg>
      );
    case "complete":
      return (
        <svg {...common} viewBox="0 0 10 10">
          <circle cx="5" cy="5" r="4.2" fill={c} />
          <path d="M3 5.2 L4.5 6.6 L7.2 3.6" stroke="#06110f" strokeWidth="1.3" fill="none" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      );
    case "error":
      return (
        <svg {...common} viewBox="0 0 10 10">
          <circle cx="5" cy="5" r="4.2" fill={c} />
          <path d="M3.4 3.4 L6.6 6.6 M6.6 3.4 L3.4 6.6" stroke="#1a0606" strokeWidth="1.3" strokeLinecap="round" />
        </svg>
      );
  }
}

export function StatusDot({ status, className }: { status: "running" | "ok" | "warn" | "err" | "idle"; className?: string }) {
  const c = { running: "var(--color-model)", ok: "var(--color-ok)", warn: "var(--color-warn)", err: "var(--color-err)", idle: "var(--color-fg-4)" }[status];
  return (
    <span className={cx("relative inline-flex h-2 w-2 shrink-0", className)} aria-hidden>
      {status === "running" && <span className="absolute inset-0 rounded-full animate-ring" style={{ background: c }} />}
      <span className="relative inline-block h-2 w-2 rounded-full" style={{ background: c }} />
    </span>
  );
}

export function Eyebrow({ children, className, kind }: { children: ReactNode; className?: string; kind?: Kind }) {
  return (
    <div className={cx("label flex items-center gap-2", className)}>
      {kind && <Glyph kind={kind} size={8} />}
      <span>{children}</span>
    </div>
  );
}

type BtnProps = { variant?: "primary" | "secondary" | "ghost"; size?: "md" | "lg"; className?: string; children: ReactNode };

const btnBase =
  "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md font-medium transition-[background-color,border-color,color,transform] duration-200 ease-out-quart select-none";
const btnVariant = {
  primary: "bg-fg text-bg hover:bg-white active:translate-y-px",
  secondary: "border border-line-2 bg-bg-2/60 text-fg hover:border-line-3 hover:bg-bg-3 active:translate-y-px",
  ghost: "text-fg-2 hover:text-fg",
};
const btnSize = { md: "h-9 px-3.5 text-[13.5px]", lg: "h-11 px-5 text-[15px]" };

export function Button({ variant = "primary", size = "md", className, children, ...rest }: BtnProps & ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button className={cx(btnBase, btnVariant[variant], btnSize[size], className)} {...rest}>
      {children}
    </button>
  );
}

export function ButtonLink({ variant = "primary", size = "md", className, children, ...rest }: BtnProps & AnchorHTMLAttributes<HTMLAnchorElement>) {
  return (
    <a className={cx(btnBase, btnVariant[variant], btnSize[size], className)} {...rest}>
      {children}
    </a>
  );
}

export function Arrow({ className }: { className?: string }) {
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" fill="none" aria-hidden className={className}>
      <path d="M2.5 7H11M7.5 3.5L11 7L7.5 10.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function GitHubMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" width="15" height="15" fill="currentColor" aria-hidden className={className}>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}

/** Product-panel chrome: a plausible window frame, not a floating card. */
export function Panel({ children, className, title, right }: { children: ReactNode; className?: string; title?: ReactNode; right?: ReactNode }) {
  return (
    <div className={cx("overflow-hidden rounded-lg border border-line bg-bg-1 shadow-[0_1px_0_rgba(255,255,255,0.03)_inset,0_24px_60px_-30px_rgba(0,0,0,0.8)]", className)}>
      {(title || right) && (
        <div className="flex h-9 items-center justify-between border-b border-line bg-bg-2/70 px-3">
          <div className="label flex items-center gap-2 normal-case tracking-normal text-fg-2">{title}</div>
          <div className="label flex items-center gap-3">{right}</div>
        </div>
      )}
      {children}
    </div>
  );
}
