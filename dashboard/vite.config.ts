import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

// `npm run dev` proxies the API to a pinnad: a local one by default, or a
// satellite with PINNA_URL=http://pinna-2a914e.local (its node name), or the
// virtual Echo Dot with PINNA_URL=http://127.0.0.1:18090 (just dev-vm).
const target = process.env.PINNA_URL ?? "http://127.0.0.1:7470";

export default defineConfig({
  plugins: [svelte()],
  server: {
    fs: { allow: [".."] },
    proxy: {
      "/pinna.v1.DeviceService": { target, changeOrigin: true },
      "/pinna.v1.AssistantService": { target, changeOrigin: true },
      // The virtual Echo Dot's socket (ADR 0014), on the development VM.
      "/virtual-device": { target, changeOrigin: true, ws: true },
    },
  },
});
