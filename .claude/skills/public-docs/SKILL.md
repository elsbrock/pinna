---
name: public-docs
description: Bring Pinna's public text (website docs, landing page, README, PRODUCT.md) in line with the code, in the project's voice. Use after a change in behaviour, when docs and code disagree, or when asked to write or review website or README text.
---

# Public docs

The language rules are in `AGENTS.md`, under "Language". This skill is the
procedure for applying them.

## 1. Find every page the change affects

Search `website/src/content/docs/`, `website/src/pages/index.astro`,
`README.md` and `PRODUCT.md` for the feature's names, RPC and entity names,
setting names, and the old behaviour.

| Area | Pages |
| --- | --- |
| Ring, buttons, notifications | `ring-and-buttons.md` |
| Voice, wake words, replies, speaker | `audio.md`, `home-assistant.md`, `troubleshooting.md` |
| Home Assistant entities | `entities.md`, checked against `crates/pinna-esphome/src/profile.rs` |
| The control API | `control-api.md`, checked against `proto/pinna/v1/device.proto` |
| Settings and the dashboard | `configure.md`, `dashboard.md` |
| AirPlay and Bluetooth | `airplay.md`, `bluetooth.md` |
| Installing, updating, recovery | `flash.md`, `update.md`, `recovery.md`, `docs/runbook-first-flash.md` |
| Building, development, tests | `build.md`, `development.md`, `crates.md` |
| Supported devices | `requirements.md`, `flash.md` and the landing page (the `new-device` skill lists them all) |

These pages fall behind most often. Check them on every change:

- `status.md`: "What works", "Known gaps" and "Planned".
- `goals.md`, and its copy in the landing page's goals list
  (`website/src/pages/index.astro`).
- The README's feature list.

ADRs are decision records, not user docs. Update the record that the change
contradicts, but don't copy its text into the website, and don't name or link
the records there, not even on the developer pages: no ADR numbers, no links to
`docs/adr/`.

## 2. Write

- Describe what the code does today. Read the code path rather than the plan
  or the pull request description.
- Mark planned work as planned, in `status.md` or `goals.md`.
- Follow the shape of the existing pages: a sentence on what the page helps
  you do, "Before you begin" when there are prerequisites, numbered steps with
  one action each, and "What's next" at the end.
- Give the dashboard way first. A file or SSH way is for developers, or is a
  stopgap that the page calls a stopgap.

## 3. Check

1. `cd website && npm run build`. It also checks the design tokens.
2. Read every changed page as a reviewer who hasn't seen the change. Every
   command, flag, path, RPC and entity name must exist, and every statement
   must be true today. If a review or drift-check agent is available, run it
   over the changed pages and the code they describe.
3. Commit with a subject that states the fact the docs now give:
   `docs: the wake word is armed while a timer alarm rings`.
