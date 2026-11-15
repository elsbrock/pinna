#!/usr/bin/env node
// Generates tokens.css and tailwind.css from tokens.json.
//
//   node design/build.mjs          write both files
//   node design/build.mjs --check  exit 1 if either file is out of date

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const dir = dirname(fileURLToPath(import.meta.url));
const tokens = JSON.parse(readFileSync(join(dir, "tokens.json"), "utf8"));

const [light, dark] = tokens.color.themes.map((t) => t.id);
const header = (what) =>
  `/* Pinna ${what} — generated from tokens.json by build.mjs. Do not edit. */\n`;

/** "{signal}" -> "var(--signal)"; anything else is a literal. */
const cssValue = (v) => {
  const alias = /^\{(.+)\}$/.exec(v);
  return alias ? `var(--${alias[1]})` : v;
};
const themed = (token, theme) =>
  typeof token.value === "string" ? token.value : (token.value[theme] ?? token.value[light]);
const differsInDark = (token) =>
  typeof token.value !== "string" && token.value[dark] !== undefined && token.value[dark] !== token.value[light];

const decl = (name, value) => `  --${name}: ${value};`;
const themedFamilies = [tokens.color, tokens.shadow];
const plainFamilies = Object.entries(tokens).filter(
  ([key, fam]) => !["color", "shadow", "type"].includes(key) && fam && Array.isArray(fam.tokens),
);

function tokensCss() {
  const out = [header("design tokens")];

  for (const font of tokens.type.fonts) {
    out.push(
      `@font-face {\n  font-family: "${font.family}";\n  src: url("${font.file}") format("woff2");\n` +
        `  font-weight: ${font.weight};\n  font-style: ${font.style ?? "normal"};\n  font-display: swap;\n}\n`,
    );
  }

  const lightDecls = themedFamilies.flatMap((fam) => fam.tokens.map((t) => decl(t.name, cssValue(themed(t, light)))));
  const plainDecls = plainFamilies.flatMap(([, fam]) => fam.tokens.map((t) => decl(t.name, t.value)));
  const fontDecls = Object.entries(tokens.type.families).map(([key, stack]) => decl(`font-${key}`, stack));
  out.push(`:root {\n  color-scheme: light;\n${[...lightDecls, ...plainDecls, ...fontDecls].join("\n")}\n}\n`);

  const darkDecls = themedFamilies
    .flatMap((fam) => fam.tokens.filter(differsInDark).map((t) => decl(t.name, cssValue(themed(t, dark)))))
    .join("\n");
  const indented = darkDecls.replace(/^/gm, "  ");
  out.push(
    `/* Dark follows the system unless the page sets data-theme. */\n` +
      `@media (prefers-color-scheme: dark) {\n  :root:not([data-theme="${light}"]) {\n    color-scheme: dark;\n${indented}\n  }\n}\n`,
  );
  out.push(`:root[data-theme="${dark}"] {\n  color-scheme: dark;\n${darkDecls}\n}\n`);

  for (const group of tokens.type.groups) {
    for (const style of group.styles) {
      const props = [
        `font-family: var(--font-${style.family ?? group.family});`,
        `font-size: ${style.fontSize};`,
        `line-height: ${style.lineHeight};`,
        `font-weight: ${style.fontWeight};`,
        style.letterSpacing && `letter-spacing: ${style.letterSpacing};`,
        style.name === "label" && "text-transform: uppercase;",
        group.family === "mono" && "font-variant-numeric: tabular-nums;",
      ].filter(Boolean);
      out.push(`.pn-type-${style.name} {\n  ${props.join("\n  ")}\n}\n`);
    }
  }
  return out.join("\n");
}

function tailwindCss() {
  const colors = tokens.color.tokens.map((t) => `  --color-${t.name}: var(--${t.name});`);
  const radii = tokens.radius.tokens.map((t) => `  --${t.name}: var(--${t.name});`);
  const text = tokens.type.groups.flatMap((g) =>
    g.styles.flatMap((s) =>
      [
        `  --text-${s.name}: ${s.fontSize};`,
        `  --text-${s.name}--line-height: ${s.lineHeight};`,
        `  --text-${s.name}--font-weight: ${s.fontWeight};`,
        s.letterSpacing && `  --text-${s.name}--letter-spacing: ${s.letterSpacing};`,
      ].filter(Boolean),
    ),
  );
  return (
    header("Tailwind v4 theme") +
    `/* Import after "tailwindcss" and after tokens.css (in a layer later than theme, e.g. base).
   Utilities point at the design tokens, so light and dark switch with them. Colours and radii
   are Pinna's only; Tailwind's default palette and radius scale are removed. */

@theme {
  --color-*: initial;
  --radius-*: initial;
${text.join("\n")}
}

@theme inline {
  --font-sans: var(--font-text);
  --font-display: var(--font-display);
  --font-mono: var(--font-mono);
  --container-measure: var(--measure);
  --container-page: var(--page-max);
${radii.join("\n")}
${colors.join("\n")}
}
`
  );
}

const outputs = { "tokens.css": tokensCss(), "tailwind.css": tailwindCss() };

if (process.argv.includes("--check")) {
  const stale = Object.entries(outputs).filter(([file, text]) => {
    try {
      return readFileSync(join(dir, file), "utf8") !== text;
    } catch {
      return true;
    }
  });
  if (stale.length) {
    console.error(`design/${stale.map(([f]) => f).join(", design/")} out of date: run node design/build.mjs`);
    process.exit(1);
  }
} else {
  for (const [file, text] of Object.entries(outputs)) writeFileSync(join(dir, file), text);
  console.log(`wrote design/${Object.keys(outputs).join(", design/")}`);
}
