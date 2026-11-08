import { defineConfig } from "vite";
import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// In development the UI talks to the gsd on 127.0.0.1:4318, or to GSD_URL,
// through this proxy, so the browser sees one origin and nothing needs CORS.
// The proxy rewrites Host, which gsd requires to be loopback, but not Origin:
// an api accepts sign-in and writes only from its public URL, so to use one
// from here, run it with GROUNDSTATION_PUBLIC_URL=http://localhost:5180.
// CORS stays off, so no other local page can read gsd through the proxy.
// In production the UI is embedded and served by gsd or the api itself.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: {
    port: 5180,
    cors: false,
    proxy: Object.fromEntries(
      ["/v1", "/auth"].map((path) => [path, { target: process.env.GSD_URL ?? "http://127.0.0.1:4318", changeOrigin: true }]),
    ),
  },
  build: { outDir: "dist", emptyOutDir: true },
});
