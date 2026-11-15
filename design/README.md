# Pinna design system

Tokens, component styles, fonts and the LED ring for everything Pinna shows on
a screen: the website (`website/`) now, the dashboard later. The brand book
(voice, colour and type rules, the ring's states, component guidelines with live
previews) is the design system page:
<https://claude.ai/artifact/PNXQTXcpiYpNeeoeT7vtvR>. This directory is what code
imports.

| File | What |
|---|---|
| `tokens.json` | The source: colours (light and dark), type, spacing, radii, shadows, sizes |
| `tokens.css` | Generated. Every token as a CSS custom property, the `@font-face` rules and `.pn-type-*` text styles. Dark follows `prefers-color-scheme` unless the page sets `data-theme` |
| `tailwind.css` | Generated. A Tailwind v4 theme whose utilities point at the tokens (`bg-surface`, `text-ink-muted`, `font-display`, `text-display-l`, `rounded-md`) |
| `pinna.css` | Component classes (`pn-btn`, `pn-badge`, `pn-ring`, `pn-notice`, …). Plain CSS on the tokens, no framework |
| `ring.ts` | Port of `crates/pinna-hal/src/anim.rs` plus the ring's drawing geometry |
| `ring.test.ts` | The `anim.rs` tests, against `ring.ts` |
| `mark.svg` | The mark (the ring's boot frame): favicon and app icon |
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

## Change

- Edit `tokens.json`, then run `node design/build.mjs` and commit both generated
  files. `node design/build.mjs --check` fails when they are stale; the website
  build runs it.
- When `anim.rs` changes, change `ring.ts` and `ring.test.ts` in the same
  commit: `node --test design/ring.test.ts`.
- Keep the design system page in step with `tokens.json` and `pinna.css`.
