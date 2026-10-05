import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The control-plane API bind (override with LQ_API_PROXY when the engine runs
// on a non-default port, e.g. LQ_API_PROXY=http://localhost:8090).
const apiTarget = process.env.LQ_API_PROXY ?? "http://localhost:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: apiTarget, changeOrigin: true },
      "/healthz": { target: apiTarget, changeOrigin: true },
    },
  },
});