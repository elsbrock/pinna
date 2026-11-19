import type { APIRoute } from "astro";
import { href } from "../lib/site";

// Points crawlers at the sitemap that @astrojs/sitemap writes.
export const GET: APIRoute = ({ site }) => {
  const sitemap = new URL(href("/sitemap-index.xml"), site);
  return new Response(`User-agent: *\nAllow: /\n\nSitemap: ${sitemap.href}\n`, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
};
