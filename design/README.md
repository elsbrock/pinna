# Pinna design system

Tokens, component styles, fonts and the LED ring for everything Pinna shows on
a screen: the website (`website/`) and the dashboard (`dashboard/`). The brand book
(voice, colour and type rules, the ring's states, component guidelines with live
previews) is the design system page:
<https://claude.ai/artifact/PNXQTXcpiYpNeeoeT7vtvR>. This directory is what code
imports.

| File | What |
|---|---|
| `tokens.json` | The source: colours (light and dark), type, spacing, radii, shadows, sizes |
| `tokens.css` | Generated. Every token as a CSS custom property, the `@font-face` rules and `.pn-type-*` text styles. Dark follows `prefers-color-scheme` unless the page sets `data-theme` |
| `tailwind.css` | Generated. A Tailwind v4 theme whose utilities point at the tokens (`bg-surface`, `text-ink-muted`, `font-display`, `text-display-l`, `rounded-md`) |
| `pinna.css` | Component classes (`pn-btn`, `pn-badge`, `pn-ring`, `pn-notice`, `pn-file`, …). Plain CSS on the tokens, no framework |
| `ring.ts` | Port of `crates/pinna-hal/src/anim.rs` plus the ring's drawing geometry |
| `ring.test.ts` | The `anim.rs` tests, against `ring.ts` |
| `devices/` | One drawing per supported model, picked by `Status.model` (`devices/index.ts`); the Echo Dot (2nd gen), the Echo (2nd gen) and the Echo Dot (5th gen). A model without a drawing gets the flat Ring |
| `icons.ts` | The stroked icon set as SVG markup, for the dashboard (the website can use it too) |
| `mark.svg` | The mark: a folded hexagon around a play triangle, shading from the dark `signal` blue to the dark Thinking violet. Favicon and app icon, the same in light and dark |
| `fonts/` | Unbounded, Atkinson Hyperlegible Next and Mono, variable WOFF2 (latin), SIL OFL 1.1, licences beside them |

## Use

```css
@import "tailwindcss";
@import "../../design/tokens.css" layer(base);
@import "../../design/tailwind.css";
@import "../../design/pinna.css" layer(components);
```

Without Tailwind, import `tokens.css` and `pinna.css` alone. Serve the fonts
yourself: the dashboard runs on the satellite and must work offline, so nothing
loads from a font CDN.

## Pages

How the dashboard cuts its screens into pages. `pages/Voice.svelte` and its
subpages are the model.

- **One page, one job.** A page answers one question an owner comes with:
  update the satellite, lock the dashboard, choose the wake word. The flows of
  that job share its page: installing and switching slots are both updates.
- **A section page is a hub.** It holds the everyday settings of its area and
  one row per subpage (`pn-row` as a link, with a chevron). The row's value and
  hint show that subpage's state, a failure first, so the hub still says where
  things stand: an install that failed, a dashboard open to the network.
- **A topic gets a subpage** when it has a flow of its own (steps, a
  confirmation at the satellite, a wait) or needs more than one group.
- **Look again at a page with more than four groups,** counting the ones that
  show only in some states.
- **Two levels only**: a section and its subpages, at `/section/subpage`. A
  subpage has no subpages.
- **Overview is the status page.** Its tiles and notices link to the page that
  explains them or fixes the cause.

## Change

- Edit `tokens.json`, then run `node design/build.mjs` and commit both generated
  files. `node design/build.mjs --check` fails when they are stale; the website
  build runs it.
- When `anim.rs` changes, change `ring.ts` and `ring.test.ts` in the same
  commit: `node --test design/ring.test.ts`.
- Keep the design system page in step with `tokens.json` and `pinna.css`.
