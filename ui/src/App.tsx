import { useEffect, useState } from "react";
import { Shell } from "@/components/Shell";
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

export default function App() {
  const route = useHash();
  const m = route.match(/^#\/t\/(.+)$/);
  return (
    <Shell route={route}>
      {m ? <Trajectory id={decodeURIComponent(m[1])} /> : <Trajectories />}
    </Shell>
  );
}
