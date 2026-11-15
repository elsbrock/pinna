export const SITE = {
  name: "Pinna",
  tagline: "A local-first voice satellite OS for the Echo (2nd gen)",
  description:
    "Pinna is an open, image-based Linux system that turns the Echo Dot (2nd gen) " +
    "into a Home Assistant voice satellite: on-device wake word, LED ring and " +
    "buttons, ESPHome native API, nothing sent to a cloud.",
  repo: "https://github.com/elsbrock/pinna",
  repoDocs: "https://github.com/elsbrock/pinna/tree/main/docs",
  adr: "https://github.com/elsbrock/pinna/tree/main/docs/adr",
  runbook:
    "https://github.com/elsbrock/pinna/blob/main/docs/runbook-first-flash.md",
};

/** Prefix a site-absolute path ("/docs/") with the deployment base path. */
export function href(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/+$/, "");
  const rest = path.startsWith("/") ? path : `/${path}`;
  return `${base}${rest}`;
}

export type DocItem = { slug: string; title: string };
export type DocGroup = { group: string; items: DocItem[] };

/** Sidebar structure — also the order of the prev/next pager. */
export const DOCS_NAV: DocGroup[] = [
  {
    group: "Getting started",
    items: [
      { slug: "", title: "Overview" },
      { slug: "build", title: "Build" },
      { slug: "flash", title: "Flash the device" },
      { slug: "recovery", title: "Recovery" },
    ],
  },
  {
    group: "Using Pinna",
    items: [
      { slug: "home-assistant", title: "Home Assistant" },
      { slug: "control-api", title: "Control API" },
    ],
  },
  {
    group: "Reference",
    items: [
      { slug: "architecture", title: "Architecture" },
      { slug: "crates", title: "Crates & layout" },
      { slug: "development", title: "Development" },
      { slug: "status", title: "Status & roadmap" },
    ],
  },
];

export const DOCS_FLAT: DocItem[] = DOCS_NAV.flatMap((group) => group.items);

export function docHref(slug: string): string {
  return slug === "" ? href("/docs/") : href(`/docs/${slug}/`);
}

export function docEditHref(file: string): string {
  return `${SITE.repo}/edit/main/website/src/content/docs/${file}`;
}
