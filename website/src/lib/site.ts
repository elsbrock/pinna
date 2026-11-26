export const SITE = {
  name: "Pinna",
  tagline: "A local voice satellite for your Echo",
  description:
    "Pinna is an open-source operating system for Echo speakers. It detects " +
    "the wake word on the device and answers " +
    "with your Home Assistant, or on its own with the speech services and " +
    "the language model or AI agent you choose.",
  repo: "https://github.com/elsbrock/pinna",
  repoDocs: "https://github.com/elsbrock/pinna/tree/main/docs",
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
      { slug: "devices", title: "Echo devices" },
      { slug: "requirements", title: "What you need" },
      { slug: "flash", title: "Install Pinna" },
      { slug: "install-echo-dot-5", title: "Install on the Echo Dot (5th gen)" },
      { slug: "dashboard", title: "Set up the satellite" },
    ],
  },
  {
    group: "With Home Assistant",
    items: [
      { slug: "home-assistant", title: "Connect to Home Assistant" },
      { slug: "entities", title: "Home Assistant entities" },
    ],
  },
  {
    group: "Without Home Assistant",
    items: [
      { slug: "standalone", title: "Set up standalone mode" },
      { slug: "ai-agents", title: "Use an AI agent" },
      { slug: "satellite-groups", title: "Satellite groups" },
    ],
  },
  {
    group: "Use your satellite",
    items: [
      { slug: "ring-and-buttons", title: "Light ring and buttons" },
      { slug: "wake-words", title: "Wake words" },
      { slug: "music", title: "Play music" },
      { slug: "airplay", title: "Play music with AirPlay" },
      { slug: "bluetooth", title: "Play music over Bluetooth" },
      { slug: "sendspin", title: "Play music with Sendspin" },
      { slug: "settings", title: "Satellite settings" },
    ],
  },
  {
    group: "Keep it running",
    items: [
      { slug: "update", title: "Update Pinna" },
      { slug: "troubleshooting", title: "Troubleshooting" },
      { slug: "recovery", title: "Recover a device" },
      { slug: "metrics", title: "Prometheus metrics" },
    ],
  },
  {
    group: "Develop",
    items: [
      { slug: "development", title: "Contribute" },
      { slug: "build", title: "Build Pinna" },
      { slug: "configure", title: "Developer settings" },
      { slug: "control-api", title: "pinnactl and the control API" },
      { slug: "releases", title: "Releases and updates" },
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
