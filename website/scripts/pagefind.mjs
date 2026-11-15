// Build the Pagefind search index from dist/.
//
// Uses the Pagefind Node API instead of the CLI so that result URLs can carry
// the deployment base path ("/pinna/docs/..." for GitHub Pages project sites),
// which `pagefind --site` alone cannot do.
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";

import { close, createIndex } from "pagefind";

import { SITE_BASE } from "../site.config.mjs";

const DIST = path.resolve("dist");
const prefix = SITE_BASE.replace(/\/+$/, "");

async function* htmlFiles(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "pagefind") continue;
      yield* htmlFiles(full);
    } else if (entry.name.endsWith(".html")) {
      yield full;
    }
  }
}

const { index, errors } = await createIndex({ verbose: false });
if (!index) {
  console.error("pagefind: could not create index");
  for (const error of errors) console.error(`  ${error}`);
  process.exit(1);
}

let pages = 0;
for await (const file of htmlFiles(DIST)) {
  const rel = path.relative(DIST, file).split(path.sep).join("/");
  if (rel === "404.html") continue;

  const url = `${prefix}/${rel.replace(/index\.html$/, "")}`;
  const content = await readFile(file, "utf8");
  const { errors: fileErrors } = await index.addHTMLFile({
    sourcePath: rel,
    url,
    content,
  });
  for (const error of fileErrors) console.warn(`pagefind: ${rel}: ${error}`);
  pages += 1;
}

const { errors: writeErrors, outputPath } = await index.writeFiles({
  outputPath: path.join(DIST, "pagefind"),
});
for (const error of writeErrors) console.error(`pagefind: ${error}`);
await close();

const entry = JSON.parse(
  await readFile(path.join(outputPath, "pagefind-entry.json"), "utf8"),
);
const indexed = Object.values(entry.languages).reduce(
  (sum, language) => sum + language.page_count,
  0,
);

console.log(
  `pagefind: scanned ${pages} pages, indexed ${indexed} -> ${path.relative(process.cwd(), outputPath)}`,
);
