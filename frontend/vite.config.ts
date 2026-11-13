import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In dev, gRPC-Web calls (/lkube.v1.*) are proxied to the Rust server.
const target = process.env.LKUBE_API ?? "http://localhost:50051";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: { "^/lkube\\.v1\\.": { target, changeOrigin: true } },
  },
  test: { environment: "node" },
});
