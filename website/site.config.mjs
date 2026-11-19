// Single place that decides where the built site is published.
//
// Defaults target the custom domain, served from the root:
//   https://getpinna.dev/
//
// For a GitHub Pages project site under a path, build with:
//   SITE_URL=https://elsbrock.github.io SITE_BASE=/pinna npm run build

export const SITE_URL = process.env.SITE_URL ?? "https://getpinna.dev";
export const SITE_BASE = process.env.SITE_BASE ?? "/";

/** Join the base path and a site-absolute path ("/docs/" -> "/pinna/docs/" under /pinna). */
export function withBase(path) {
  const base = SITE_BASE.replace(/\/+$/, "");
  const rest = path.startsWith("/") ? path : `/${path}`;
  return `${base}${rest}`;
}
