// Hash routes: the satellite serves one index.html, so the page picks the section.
export const PAGES = [
  { path: "/", title: "Overview" },
  { path: "/settings", title: "Settings" },
  { path: "/updates", title: "Updates" },
  { path: "/network", title: "Network" },
  { path: "/logs", title: "Logs" },
] as const;

export type Path = (typeof PAGES)[number]["path"];

const parse = (): Path => {
  const p = location.hash.replace(/^#/, "") || "/";
  return PAGES.some((x) => x.path === p) ? (p as Path) : "/";
};

class Router {
  path = $state<Path>(parse());

  constructor() {
    addEventListener("hashchange", () => {
      this.path = parse();
      window.scrollTo(0, 0);
    });
  }
}

export const router = new Router();
