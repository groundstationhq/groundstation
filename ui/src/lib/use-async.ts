import { useEffect, useState } from "react";

export type Async<T> = { status: "loading" } | { status: "ok"; data: T } | { status: "error"; error: string; cause: unknown };

/**
 * Runs `fn` when `deps` change, with abort on unmount. Re-runs every `pollMs`
 * if set, also after errors; `pollMs` may depend on the last answer, and
 * none means stop.
 */
export function useAsync<T>(fn: (signal: AbortSignal) => Promise<T>, deps: unknown[], pollMs?: number | ((last: Async<T>) => number | undefined)): Async<T> {
  const [state, set] = useState<Async<T>>({ status: "loading" });
  useEffect(() => {
    const ac = new AbortController();
    let timer: number | undefined;
    const run = () => {
      let last: Async<T> = { status: "loading" };
      fn(ac.signal)
        .then((data) => { last = { status: "ok", data }; })
        .catch((e: unknown) => { last = { status: "error", error: e instanceof Error ? e.message : String(e), cause: e }; })
        .finally(() => {
          if (ac.signal.aborted) return;
          set(last);
          const wait = typeof pollMs === "function" ? pollMs(last) : pollMs;
          if (wait) timer = window.setTimeout(run, wait);
        });
    };
    run();
    return () => { ac.abort(); if (timer) clearTimeout(timer); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return state;
}
