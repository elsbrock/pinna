---
name: release-notes
description: Draft the notes for a Pinna release, releases/<version>.md, in the style of Home Assistant's release notes, from the commits since the previous release. Use before tagging a release or a beta, or when asked for a changelog or release notes.
---

# Release notes

Every release has notes in `releases/<version>.md` (ADR 0022). Their first
paragraph is the summary on Home Assistant's update card and in the signed
manifest; the whole file is the GitHub release's text. The release workflow
refuses a tag without them, so they are written, reviewed and committed
before the tag.

You draft them. The owner reviews and commits them, tags, and publishes the
draft release. Don't commit, tag or push yourself unless asked.

## 1. The version and the range

The version is `<major>.<minor>.<patch>` or `<major>.<minor>.<patch>-beta.<n>`
(ADR 0022, "Versions"). Ask for it if it wasn't given.

The previous release, on your computer:

```sh
# for a stable version: the newest stable tag before it
git describe --tags --abbrev=0 --match 'v[0-9]*' --exclude 'v*-*' --exclude v<version> HEAD
# for a beta: the newest tag of either kind
git describe --tags --abbrev=0 --match 'v[0-9]*' --exclude v<version> HEAD
```

The range is `<previous>..HEAD`.

## 2. Read what changed

```sh
git log --reverse --no-merges --format='%h %s' <range>
git log --reverse --no-merges --format='--- %h %s%n%b' <range> -- crates mkosi kernel dashboard proto
git diff <previous> HEAD -- crates/pinna-esphome/src/profile.rs proto/pinna/v1/device.proto
git diff --stat <previous> HEAD -- website/src/content/docs
```

The commit subjects say what changed; their bodies say why, and carry the
measurements. The `profile.rs` diff shows entities that were added, renamed
or removed in Home Assistant. Changed website pages point at what owners
see. For each change that matters to an owner, read enough of the commit or
the docs to describe it correctly. Leave out what doesn't ship in the image:
website, design system, docs, tests, ADRs, chores, CI.

## 3. Write `releases/<version>.md`

```markdown
# Pinna <version>

<Summary: one paragraph, at most 255 characters, of what an owner notices.>

## Before you update

## Highlights

### <A change, in the owner's words>

## New in Home Assistant

## Other changes

## Fixed

## All changes
```

- **Summary.** What an owner will notice, in two or three short sentences.
  No version number (the title has it), no list. Count the characters; the
  workflow refuses more than 255.
- **Before you update.** Anything the owner must do or know first: a step by
  hand (a new boot image over USB, a key change), a setting that changes or
  resets, a new contact with a service, behaviour that goes away. Leave the
  section out when there is nothing. For a beta, start with: "This is a
  beta: it goes to satellites whose **Updates** setting is `beta`."
- **Highlights.** One `###` section for each change an owner would tell
  someone about, at most four. A short paragraph: what it does for them,
  and where they find it (the dashboard page, the Home Assistant entity, the
  button). If it changes how something behaves, say what to do if they
  don't want it.
- **New in Home Assistant.** Entities added, renamed or removed, by their
  name in Home Assistant and their kind: "**Firmware** (update): …". Renamed
  or removed entities also go under "Before you update": automations break.
- **Other changes** and **Fixed.** One line each, in owner terms: what works
  now, not what the code does. "Home Assistant connects over IPv6, also where
  the satellite only has a link-local address", not "fix(pinna-esphome):
  accept IPv6".
- **All changes.** Every `feat` and `fix` commit subject in the range, as
  written, oldest first, under `### New` and `### Fixed`, each with its
  scope in parentheses: `- The wake word stops a ringing timer alarm
  (pinnad)`. Capitalise the first letter. Leave out the scopes that don't
  ship: `website`, `design`, `adr`, `vm`, `release`, `models`, `dev`. Start
  the section with the range in one line: "Changes since 0.3.0."

Leave out empty sections. The language rules are `AGENTS.md`, "Language":
plain, short sentences, no hype, exclamation marks or emoji; the project's
nouns (satellite, ring, Home Assistant); the phase words of the proto; the
hardware named exactly; never the name of the previous firmware's project.

## 4. Check, then hand over

- The title is exactly `# Pinna <version>`.
- The summary is the first paragraph and has at most 255 characters.
- Every highlight and fix is something in the range; nothing is invented.
- "All changes" lists every `feat` and `fix` subject in the range that
  ships, and nothing else.

Show the owner the file. If the build or the release workflow changed since
the last release, suggest a dry run first: `gh workflow run release.yml`
builds the same on main with a throwaway key. A tag whose build fails has to
be moved.

When they're happy, on their computer, on main:

```sh
git add releases/<version>.md
git commit -m "docs(release): notes for <version>"
git tag v<version>
git push origin main v<version>
```

Main has to contain the tag: development versions and the next release's
range come from `git describe`. A release prepared on a branch is merged into
main with a merge commit, not copied over commit by commit.

The release workflow builds the release with these notes and leaves it as a
draft on GitHub, a beta as a prerelease.

- Until it is published, the notes can still change: commit the new
  `releases/<version>.md`, and replace the draft's text above its `## Files`
  section with the file without its title line
  (`gh release edit v<version> --notes-file <file>`). The summary is signed
  into the manifest, so a new summary needs a new build.
- The owner publishes it. Within five minutes the release service offers it:
  `curl -sI https://updates.getpinna.dev/v1/channels/stable` (or `beta`)
  redirects to its `manifest.json`.
