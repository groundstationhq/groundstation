import { useEffect, useState } from "react";

export type Async<T> = { status: "loading" } | { status: "ok"; data: T } | { status: "error"; error: string; cause: unknown };

/** Runs `fn` when `deps` change, with abort on unmount. Re-runs every `pollMs` if set, also after errors. */
export function useAsync<T>(fn: (signal: AbortSignal) => Promise<T>, deps: unknown[], pollMs?: number): Async<T> {
  const [state, set] = useState<Async<T>>({ status: "loading" });
  useEffect(() => {
    const ac = new AbortController();
    let timer: number | undefined;
    const run = () =>
      fn(ac.signal)
        .then((data) => !ac.signal.aborted && set({ status: "ok", data }))
        .catch((e: unknown) => !ac.signal.aborted && set({ status: "error", error: e instanceof Error ? e.message : String(e), cause: e }))
        .finally(() => { if (pollMs && !ac.signal.aborted) timer = window.setTimeout(run, pollMs); });
    run();
    return () => { ac.abort(); if (timer) clearTimeout(timer); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return state;
}
