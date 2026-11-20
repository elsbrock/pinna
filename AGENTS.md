# Working on Pinna

Pinna is an open, local-first voice satellite OS for the Echo Dot (2nd gen)
and the Echo (2nd gen): Rust daemons in `crates/`, a Debian/systemd image in
`mkosi/`, a patched Linux kernel in `kernel/`, the dashboard in `dashboard/`,
the design system in `design/`, and the website and user docs in `website/`.
Home Assistant talks to it over the ESPHome native API.

Read what the change needs:

- `PRODUCT.md`: who Pinna is for, and what is out of scope.
- `docs/adr/`: the decision records. Follow the one that covers the area you
  change. If the record is wrong, fix the record first.
- `website/src/content/docs/development.md`: the development loop and every
  test, with what each one needs. `crates.md` maps the repository.
- `CONTRIBUTING.md`: what gets merged.

## Tools

Nix is recommended, not required. `nix develop` provides every tool that
`flake.nix` lists. Without Nix, people install the same tools themselves.

- If `just`, `cargo` or `mkosi` isn't on `PATH` and `nix` is, run the command
  through the development shell: `nix develop -c just check`. Don't install
  tools globally to work around a missing one. Ask instead.
- Don't write recipes, scripts or docs that work only inside Nix. Call tools
  by name, and when one is missing, say which one ("adb not found").
- `just` lists the tasks. Build output goes to `build/`.

## Code

- Rust 2024 in one workspace. `cargo fmt` and `cargo clippy -- -D warnings`
  must be clean. Device binaries are static musl builds, cross-compiled with
  `cargo zigbuild`.
- Libraries return typed errors (`thiserror`). Binaries use `anyhow`, with
  context that names what failed.
- Every dependency ships to the device. Prefer the standard library and the
  crates the workspace already uses.
- Comments say why, not what. A module's doc comment says what it does and
  cites its ADR. A hardware fact carries its evidence: what was measured, and
  on which device.
- Refer to the firmware Pinna replaced as "the previous firmware" or "the 6.1
  kernel", never by its project name: in code, comments, commits and docs.
- Generated files are committed, and the checks fail when they are stale:
  - `dashboard/src/gen/` comes from `proto/` (`cd dashboard && npm run generate`).
  - `design/tokens.css` and `design/tailwind.css` come from
    `design/tokens.json` (`node design/build.mjs`).
  - `design/ring.ts` mirrors `crates/pinna-hal/src/anim.rs`. Change both in
    the same commit.
- Owners manage the satellite from the dashboard, not from files or SSH. A new
  owner-facing setting is an RPC in `proto/pinna/v1/device.proto`, a control
  in the dashboard, and an entity in `crates/pinna-esphome/src/profile.rs`,
  unless it makes sense only in Home Assistant. The reverse holds too: a new
  entity gets a dashboard control. `pinnad.toml` is for developers.
- Home Assistant is optional. Boot progress, the health check, rescue mode and
  support tools never wait for it or require it.
- Never commit `secrets/`, private keys, or the downloaded models in `blobs/`.

## Testing

Run `just check` on every change. Then run the tests that cover what you
changed:

| You changed | Also run |
| --- | --- |
| `pinna-esphome`, entities, anything Home Assistant sees | `just esphome-conformance` |
| `pinna-sendspin`, pinnad's Sendspin player, the timed source in `pinna-audio` | `just sendspin-conformance` (needs `uv`, and PyPI the first time) |
| The voice flow in `pinnad`: wake word, replies, timers | `just e2e` |
| `pinna-wakeword` | `just wakeword-parity` |
| `dashboard/`, `design/` or `proto/` | `just web-check` |
| The kernel configuration | `just kernel-config` |
| `pinna-init`, `pinna-bootimg`, boot, slots or updates | `just vm-test`, `just vm-update-test`, `just bootimg-roundtrip` |
| Rescue mode or boot stages | `just vm-rescue-test`, `just vm-rescue-test-x86` |
| The image in `mkosi/`, or anything else the virtual machine runs | `just vm-test-x86` |
| The setup hotspot, or Wi-Fi joins (`hotspot.rs`, `wifi.rs`) | `just vm-hotspot-test` |

- No CI runs these tests. Run them yourself and say which ones you ran.
- `just e2e` and `just wakeword-parity` pass without running anything when
  the wake word models are missing (`just models` fetches them). A skip is
  not a pass.
- A fix comes with a test that fails without it, at the lowest layer that
  can show the bug: a unit test, the end-to-end voice flow or the
  conformance suite.
- Say where a change was tested: unit tests, the virtual Echo Dot
  (`just dev-vm`), or a named device. Never claim a device test that didn't
  run.
- A device belongs to its owner. Don't flash it, write its partitions, restart
  it or change its settings unless you're asked to. Before you write a boot
  image, save the current `boot_a` and `boot_b`. Never run `dmesg -C` on a
  device.
- A build compiles in `secrets/release.pub` when that file exists, otherwise
  `keys/release.pub`. A boot image built with a throwaway key (for example one
  from `just keygen` for the VM tests) doesn't boot a root filesystem signed
  with a different key.

## Commits and pull requests

- Use Conventional Commits. The scope is the crate or the area: `pinnad`,
  `pinna-hal`, `dashboard`, `design`, `kernel`, `image`, `website`, `adr`,
  `scripts`.
- The subject is a plain sentence about behaviour:
  `feat(pinnad): the wake word stops a ringing timer alarm`,
  `fix(kernel): apply Echo Wi-Fi settings to radar`. Not "update X" or
  "improve Y".
- The body says what changed and why, in words a reviewer can follow without
  reading the diff first. Measurements and hardware evidence go there.
- Keep one change per commit. Docs and tests can be separate commits on the
  same branch.
- A new ADR takes the next number across all branches, not only yours:
  `git log --all --name-only --format= -- docs/adr/ | sort -u`. Propose it
  in its own `docs(adr):` commit before the code.

## Language: docs and UI text

These rules cover the website, the README, the dashboard, error messages, and
any log line an owner reads. The docs follow the
[Google developer documentation style guide](https://developers.google.com/style).

- Write plainly and precisely, in short sentences. No hype, marketing tone,
  exclamation marks or emoji. If it reads like an advert, rewrite it.
- Address the reader as "you". Call the project "Pinna", in the third person.
  Use sentence case for headings, buttons and page titles.
- Say what the state is and what to do. An error names the cause and the fix:
  "Key must be 32 bytes, base64 encoded." Never "Oops" or "Something went
  wrong".
- Use the project's nouns. A device that runs Pinna is a **satellite**. The
  light ring is the **ring**. Write **Home Assistant** in full in UI text.
  Name hardware exactly: "Echo Dot (2nd gen)", "Echo (2nd gen)", "MT8163",
  "Volume Down".
- The phase words are the proto's: Idle, Listening, Thinking, Speaking, Muted,
  Error. Don't use synonyms.
- Show commands verbatim in code blocks, and say where each one runs: on your
  computer or on the satellite.
- Before a step that writes to the device, say what it writes and how to undo
  it. Recovery comes before risk.
- Docs describe what the code does today. Mark planned work as planned. When
  behaviour changes, change the affected website pages and the README on the
  same branch.
- Don't send owners to SSH or configuration files for routine tasks. If a
  stopgap needs them, say that it's a stopgap.
- Pinna is an independent project. Echo and Echo Dot are Amazon's trademarks:
  no Amazon logos, product photos, or the look of the Alexa ring.
- Screens use the design system in `design/`: its tokens and components. Its
  README links the brand book.
