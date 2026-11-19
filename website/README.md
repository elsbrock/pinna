# Pinna website

Static site for the project: a landing page plus documentation with sidebar
navigation and full-text search. Built with [Astro](https://astro.build) and
[Tailwind CSS](https://tailwindcss.com); search is
[Pagefind](https://pagefind.app). Deployed to GitHub Pages by
`.github/workflows/deploy-site.yml`.

## Commands

```sh
npm install
npm run dev        # http://localhost:4321/pinna/ — search is a placeholder in dev
npm run build      # dist/ + Pagefind index in dist/pagefind/
npm run preview    # serve the built site
npm run check      # astro check (types)
```

The build runs `astro build` followed by `scripts/pagefind.mjs`, which indexes
`dist/` and prefixes result URLs with the deploy base path.

## Where it is published

Both values live in `site.config.mjs` and can be overridden per build:

| Variable | Default | Notes |
| --- | --- | --- |
| `SITE_URL` | `https://getpinna.dev` | Used for canonical URLs and the sitemap |
| `SITE_BASE` | `/` | The path the site is served under, such as `/pinna` for a project page |

```sh
# a GitHub Pages project page under /pinna:
SITE_URL=https://elsbrock.github.io SITE_BASE=/pinna npm run build
```

Publishing requires **Settings → Pages → Source: GitHub Actions** in the
repository once; every push to `main` that touches `website/**` then deploys.

## Structure

| Path | What |
| --- | --- |
| `src/pages/index.astro` | Landing page |
| `src/pages/docs/[...slug].astro` | Docs route (one page per Markdown entry) |
| `src/content/docs/*.md` | Docs content — `index.md` is `/docs/` |
| `src/layouts/` | Base layout and the docs layout (header, sidebar, pager) |
| `src/components/` | Header, footer, sidebar, search, icons, logo, and the device drawings (`DeviceDrawing.astro`, staged and animated in the hero by `RingDemo.astro`) |
| `src/lib/site.ts` | Site constants, sidebar structure, link helpers |
| `src/styles/global.css` | Imports the design system (`../design/`), plus site-only styles: chips, cards, docs prose, Shiki and Pagefind theming |
| `astro.config.mjs` | Base/site, Sätteri Markdown plugins, Shiki themes |

Content is adapted from the repository's `README.md`, `docs/adr/` and
`docs/runbook-first-flash.md`; the site is self-contained and does not read
those files at build time.

## Notes

- **Markdown links** are written site-absolute (`/docs/build/`). The
  `base-path-links` plugin in `astro.config.mjs` prepends `base`, so they work
  both at `/pinna/...` and at a domain root.
- **Tables** are wrapped in a scroll container (`table-scroll` plugin) so wide
  tables do not squash on mobile.
- **After changing the Markdown pipeline** (plugins, Shiki config), clear the
  content cache or Astro will reuse the previously rendered HTML:

  ```sh
  rm -rf node_modules/.astro && npm run build
  ```

- **Search** is only available in a built site; `npm run dev` shows a
  placeholder input.
- Colours, type, fonts and component classes come from the shared design
  system in `../design/` (see its README). Dark mode follows
  `prefers-color-scheme`; there is no toggle. `npm run build` fails if
  `design/tokens.css` is stale; `npm run check` also runs the ring tests.
