export const SITE = {
  name: "Pinna",
  tagline:
    "A voice satellite for Home Assistant on the Echo (2nd gen) and Echo Dot (2nd gen)",
  description:
    "Pinna is an open-source operating system for the Echo (2nd gen) and the " +
    "Echo Dot (2nd gen). It turns the device into a voice satellite for Home " +
    "Assistant, with the wake word detected on the device.",
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
    group: "Get started",
    items: [
      { slug: "", title: "Overview" },
      { slug: "requirements", title: "What you need" },
      { slug: "build", title: "Build Pinna" },
      { slug: "flash", title: "Install Pinna" },
      { slug: "home-assistant", title: "Connect to Home Assistant" },
      { slug: "recovery", title: "Recover a device" },
    ],
  },
  {
    group: "Use Pinna",
    items: [
      { slug: "ring-and-buttons", title: "Light ring and buttons" },
      { slug: "dashboard", title: "Use the dashboard" },
      { slug: "airplay", title: "Play music with AirPlay" },
      { slug: "bluetooth", title: "Play music over Bluetooth" },
      { slug: "configure", title: "Configure Pinna" },
      { slug: "update", title: "Update Pinna" },
      { slug: "troubleshooting", title: "Troubleshooting" },
    ],
  },
  {
    group: "Reference",
    items: [
      { slug: "entities", title: "Home Assistant entities" },
      { slug: "control-api", title: "pinnactl and the control API" },
      { slug: "architecture", title: "Architecture" },
      { slug: "audio", title: "Audio" },
      { slug: "crates", title: "Repository layout" },
    ],
  },
  {
    group: "Project",
    items: [
      { slug: "goals", title: "Goals" },
      { slug: "status", title: "Status and roadmap" },
      { slug: "development", title: "Contribute" },
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
