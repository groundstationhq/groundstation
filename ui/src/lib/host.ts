import { createContext, useContext } from "react";
import type { Health } from "./types";

/** What `/v1/health` says about the host serving this UI: gsd on this machine, or the hosted backend. */
export interface Host {
  hosted: boolean;
  /** Optional endpoint groups the host serves, e.g. `insights`. */
  features: string[];
}

export const HostContext = createContext<Host>({ hosted: false, features: [] });

export function useHost(): Host {
  return useContext(HostContext);
}

export function hostOf(health: Health): Host {
  return { hosted: health.deployment === "hosted", features: health.features ?? [] };
}
