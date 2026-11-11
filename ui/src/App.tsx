import { useEffect, useState } from "react";
import { Empty, Shell } from "@/components/Shell";
import { useHost } from "@/lib/host";
import { Findings } from "@/routes/Findings";
import { Overview } from "@/routes/Overview";
import { Tokens } from "@/routes/Tokens";
import { Trajectories } from "@/routes/Trajectories";
import { Trajectory } from "@/routes/Trajectory";

function useHash(): string {
  const [h, set] = useState(() => location.hash || "#/");
  useEffect(() => {
    const on = () => set(location.hash || "#/");
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  return h;
}

/** A hand-typed or cut-off link can carry a broken `%` escape. */
function decoded(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    return segment;
  }
}

function Page({ route }: { route: string }) {
  const { features } = useHost();
  const m = route.match(/^#\/t\/(.+)$/);
  if (m) {
    const id = decoded(m[1]);
    return <Trajectory key={id} id={id} />;
  }
  if (route.startsWith("#/trajectories")) return <Trajectories />;
  // Hosted only: a host serves them when `/v1/health` lists them.
  if (route.startsWith("#/findings")) {
    return features.includes("insights") ? <Findings /> : <Empty title="No findings here" body="Findings come from a Ground Station backend: its detectors look at the events gsd uploads. groundstation cloud login connects this gsd to one." />;
  }
  if (route.startsWith("#/tokens")) {
    return features.includes("tokens") ? <Tokens /> : <Empty title="No tokens here" body="Ingest tokens come from a Ground Station backend, for gsd to upload with. groundstation cloud login gets one for this computer." />;
  }
  return <Overview />;
}

export default function App() {
  const route = useHash();
  return (
    <Shell route={route}>
      <Page route={route} />
    </Shell>
  );
}
