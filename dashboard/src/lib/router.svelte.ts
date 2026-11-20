// Hash routes: the satellite serves one index.html, so the page picks the section.
import type { IconName } from "../../../design/icons.ts";
import { routeOf, type Path } from "./view.ts";

export type Section = {
  path: Path;
  title: string;
  /** Under the phone tab bar's icon. */
  short: string;
  icon: IconName;
  /** The sidebar's group label. */
  group: string;
  /** The page header's one line. */
  description: string;
};

export const SECTIONS: Section[] = [
  { path: "/", title: "Overview", short: "Overview", icon: "overview", group: "Satellite", description: "" },
  {
    path: "/voice",
    title: "Voice & sound",
    short: "Voice",
    icon: "voice",
    group: "Satellite",
    description: "What the satellite listens for, how loud it is, what may play music to it, and how bright its ring is.",
  },
  {
    path: "/assistant",
    title: "Assistant",
    short: "Assistant",
    icon: "assistant",
    group: "Satellite",
    description: "Who answers requests: Home Assistant, or services the satellite calls itself.",
  },
  {
    path: "/network",
    title: "Network",
    short: "Network",
    icon: "wifi",
    group: "Connections",
    description: "Wi-Fi, addresses, and the USB connection that works without them.",
  },
  {
    path: "/system",
    title: "System",
    short: "System",
    icon: "cpu",
    group: "Maintenance",
    description: "Name, software, updates, restarts and logs.",
  },
];

export type Page = { path: Path; title: string; description: string; section: Section };

const SUBPAGES: Omit<Page, "section">[] = [
  {
    path: "/voice/wake-word",
    title: "Wake word sensitivity",
    description: "How easily the satellite wakes, and its recent wakes and near misses.",
  },
  {
    path: "/voice/wake-words",
    title: "Wake words",
    description: "The wake words on offer, and adding your own.",
  },
  {
    path: "/voice/media-sources",
    title: "Media sources",
    description: "What may play music to the satellite from phones, computers and Music Assistant.",
  },
  {
    path: "/voice/ring",
    title: "Ring",
    description: "How bright the ring is, the ring as a light, and what each state looks like.",
  },
  {
    path: "/assistant/services",
    title: "Services",
    description:
      "What turns speech into text, answers, and speaks the reply in standalone mode: the services, their keys and their checks.",
  },
  {
    path: "/assistant/conversation",
    title: "Conversation",
    description: "The language, the place, what the brain is told, and how a conversation goes on.",
  },
  {
    path: "/assistant/tools",
    title: "Tools",
    description: "What the brain and MCP clients may do on the satellite, the radio stations it knows, and MCP.",
  },
  {
    path: "/system/logs",
    title: "Logs",
    description:
      "What the satellite is doing, newest at the bottom. Debug lines stay on the satellite, because they can contain what was said.",
  },
];

/** A section's sub-pages, in the order the sidebar lists them under it. */
export function subpagesOf(section: Path): Omit<Page, "section">[] {
  return SUBPAGES.filter((p) => p.path.startsWith(`${section}/`));
}

/** The page at `path`, with the section it belongs to (a sub-page's parent). */
export function pageOf(path: Path): Page {
  const section = SECTIONS.find((s) => s.path === path);
  if (section) return { ...section, section };
  const sub = SUBPAGES.find((p) => p.path === path)!;
  return { ...sub, section: SECTIONS.find((s) => path.startsWith(`${s.path}/`))! };
}

const parse = (): Path => {
  const p = routeOf(location.hash);
  const shown = p ?? "/";
  // A moved or unknown address: show where it went in the address bar too.
  if (`#${shown}` !== location.hash && !(shown === "/" && !location.hash)) history.replaceState(null, "", `#${shown}`);
  return shown;
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
