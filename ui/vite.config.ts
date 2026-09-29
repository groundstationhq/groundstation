import { defineConfig } from "vite";
import { fileURLToPath, URL } from "node:url";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// In development the UI talks to a local gsd through this proxy so the browser
// never has to deal with CORS. gsd only accepts loopback Host headers, so the
// origin is rewritten. In production the UI is embedded and served by gsd itself.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: {
    port: 5180,
    proxy: { "/v1": { target: process.env.GSD_URL ?? "http://127.0.0.1:4318", changeOrigin: true } },
  },
  build: { outDir: "dist", emptyOutDir: true },
});
