import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

// `npm run dev` proxies the API to a pinnad: a local one by default, or a
// satellite with PINNA_URL=http://pinna.local.
const target = process.env.PINNA_URL ?? "http://127.0.0.1:7470";

export default defineConfig({
  plugins: [svelte()],
  server: {
    fs: { allow: [".."] },
    proxy: { "/pinna.v1.DeviceService": { target, changeOrigin: true } },
  },
});
