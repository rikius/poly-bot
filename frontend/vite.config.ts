import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Allow overriding the backend host at build/run time so the dev server
// can reach the bot when running inside Docker Compose (service name "bot")
// while still defaulting to localhost for local development.
const apiHost = process.env.VITE_API_HOST ?? "localhost";
const apiPort = process.env.VITE_API_PORT ?? "3001";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: `http://${apiHost}:${apiPort}`,
        changeOrigin: true,
      },
      "/ws": {
        target: `ws://${apiHost}:${apiPort}`,
        ws: true,
        changeOrigin: true,
      },
      "/metrics": {
        target: `http://${apiHost}:${apiPort}`,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: false,
  },
});
