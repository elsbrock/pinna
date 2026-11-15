// Single place that decides where the built site is published.
//
// Defaults target the GitHub Pages *project* site:
//   https://elsbrock.github.io/pinna/
//
// For a custom domain (served from the root), build with:
//   SITE_URL=https://pinna.example SITE_BASE=/ npm run build

export const SITE_URL = process.env.SITE_URL ?? "https://elsbrock.github.io";
export const SITE_BASE = process.env.SITE_BASE ?? "/pinna";

/** Join the base path and a site-absolute path ("/docs/" -> "/pinna/docs/"). */
export function withBase(path) {
  const base = SITE_BASE.replace(/\/+$/, "");
  const rest = path.startsWith("/") ? path : `/${path}`;
  return `${base}${rest}`;
}
