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
 * A diagram is written as an image on a line of its own,
 * `![What it shows](/diagrams/overview.svg "Caption")`, and drawn inline from
 * `src/diagrams/`, so that it takes the page's colours in light and dark and
 * its links work. Site-absolute links in it get the base path, as Markdown
 * links do.
 */
const DIAGRAM = /\/diagrams\/([a-z0-9-]+\.svg)$/;
const escapeHtml = (s) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const inlineDiagrams = {
  name: "inline-diagrams",
  element: {
    filter: ["p"],
    visit(node, ctx) {
      const elements = node.children.filter((c) => c.type === "element");
      const img = elements[0];
      const name = img?.tagName === "img" && String(img.properties?.src ?? "").match(DIAGRAM)?.[1];
      if (!name || elements.length !== 1 || ctx.textContent(node).trim() !== "") return;
      const svg = readFileSync(new URL(`./src/diagrams/${name}`, import.meta.url), "utf8")
        .trim()
        .replace(/href="\/(?!\/)/g, `href="${base}/`)
        .replace("<svg", `<svg aria-label="${escapeHtml(String(img.properties.alt ?? ""))}"`);
      const title = img.properties.title;
      const caption = title ? `<figcaption>${escapeHtml(String(title))}</figcaption>` : "";
      ctx.replaceNode(node, {
        type: "raw",
        value: `<figure class="diagram" data-pagefind-ignore><div class="diagram-scroll">${svg}</div>${caption}</figure>`,
      });
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
    processor: satteri({ hastPlugins: [inlineDiagrams, basePathLinks, tableScroll] }),
    shikiConfig: {
      themes: {
        light: "github-light-default",
        dark: "github-dark-default",
      },
    },
  },
});
