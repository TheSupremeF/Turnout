import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

import { turnoutBridgePlugin } from "./scripts/turnout-bridge.mjs";

export default defineConfig({
  build: {
    outDir: "dist/client",
  },
  optimizeDeps: {
    include: ["react", "react-dom/client"],
  },
  server: {
    // Loopback only: the bridge behind `/api/turnout/*` executes the real CLI,
    // so nothing on the LAN may reach it.
    host: "127.0.0.1",
    allowedHosts: ["terminal.local"],
    warmup: {
      clientFiles: ["./src/main.jsx"],
    },
  },
  plugins: [react(), turnoutBridgePlugin()],
});
