import { useEffect, useRef, useState } from "react";
import { useInView, useReducedMotion as useMotionReducedMotion } from "motion/react";

export function useReducedMotion(): boolean {
  return !!useMotionReducedMotion();
}

/** True once the element has entered the viewport (never resets). */
export function useRevealed<T extends HTMLElement>(margin = "-15% 0px -10% 0px") {
  const ref = useRef<T>(null);
  const inView = useInView(ref, { once: true, margin: margin as never });
  return [ref, inView] as const;
}

/** Whether the element is currently on screen (resets). */
export function useOnScreen<T extends HTMLElement>(margin = "0px") {
  const ref = useRef<T>(null);
  const inView = useInView(ref, { margin: margin as never });
  return [ref, inView] as const;
}

/**
 * Drives a numeric clock forward at `rate` units per second while `running`.
 * Returns the current value. Pauses when tab is hidden (rAF).
 */
export function useTicker(running: boolean, rate = 1, start = 0, fps = 30) {
  const [v, setV] = useState(start);
  const last = useRef<number | null>(null);
  const acc = useRef(0);
  useEffect(() => {
    if (!running) return;
    let raf = 0;
    const step = (t: number) => {
      if (last.current == null) last.current = t;
      const dt = (t - last.current) / 1000;
      last.current = t;
      acc.current += dt;
      if (acc.current >= 1 / fps) {
        const d = acc.current;
        acc.current = 0;
        setV((x) => x + d * rate);
      }
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => {
      cancelAnimationFrame(raf);
      last.current = null;
    };
  }, [running, rate, fps]);
  return v;
}

export function useMediaQuery(q: string): boolean {
  const [m, setM] = useState(() => (typeof window !== "undefined" ? window.matchMedia(q).matches : false));
  useEffect(() => {
    const mq = window.matchMedia(q);
    setM(mq.matches);
    const h = (e: MediaQueryListEvent) => setM(e.matches);
    mq.addEventListener("change", h);
    return () => mq.removeEventListener("change", h);
  }, [q]);
  return m;
}
