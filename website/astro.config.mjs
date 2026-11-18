import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { defineConfig } from "astro/config";
import { satteri } from "@astrojs/markdown-satteri";
import sitemap from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";

import { SITE_BASE, SITE_URL } from "./site.config.mjs";

const base = SITE_BASE.replace(/\/+$/, "");

// The project's release key: the browser installer checks a release's
// signature against it (ADR 0022). Never a developer's own key.
const RELEASE_KEY = readFileSync(new URL("../keys/release.pub", import.meta.url), "utf8").trim();

/**
 * The commit the site was built from, for the footer: `git describe` names
 * the nearest release tag, and the hash links to the commit. Outside a git
 * checkout the footer says "unknown".
 */
function git(...args) {
  try {
    return execFileSync("git", args, {
      cwd: new URL(".", import.meta.url),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "";
  }
}
const SITE_VERSION = {
  describe: git("describe", "--tags", "--always", "--dirty") || "unknown",
  commit: git("rev-parse", "HEAD"),
};

/**
 * Markdown links are written site-absolute ("/docs/build/"). Prefix them with
 * the deployment base path so they keep working on GitHub Pages project sites.
 */
const basePathLinks = {
  name: "base-path-links",
  element: {
    filter: ["a", "img"],
    visit(node, ctx) {
      for (const prop of ["href", "src"]) {
        const value = node.properties?.[prop];
        if (
          typeof value === "string" &&
          value.startsWith("/") &&
          !value.startsWith("//")
        ) {
          ctx.setProperty(node, prop, `${base}${value}`);
        }
      }
    },
  },
};

/**
 * Wrap each table in a scroll container so wide tables scroll horizontally
 * on narrow screens instead of squashing their columns.
 */
const tableScroll = {
  name: "table-scroll",
  element: {
    filter: ["table"],
    visit(node, ctx) {
      ctx.wrapNode(node, {
        type: "element",
        tagName: "div",
        properties: { className: ["table-wrap"] },
        children: [],
      });
    },
  },
};

export default defineConfig({
  site: SITE_URL,
  base: SITE_BASE,

  // Keep HTML-aware whitespace (the v6 behaviour) and minify otherwise.
  compressHTML: true,

  vite: {
    plugins: [tailwindcss()],
    define: {
      __PINNA_RELEASE_KEY__: JSON.stringify(RELEASE_KEY),
      __PINNA_SITE_VERSION__: JSON.stringify(SITE_VERSION),
    },
    // The design system lives in ../design (shared with the dashboard).
    server: { fs: { allow: [".."] } },
  },

  integrations: [sitemap()],

  markdown: {
    processor: satteri({ hastPlugins: [basePathLinks, tableScroll] }),
    shikiConfig: {
      themes: {
        light: "github-light-default",
        dark: "github-dark-default",
      },
    },
  },
});
