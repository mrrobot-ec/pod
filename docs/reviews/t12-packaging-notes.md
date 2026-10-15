# T12 packaging notes

## Plan deviation (approved by the coordinator via `ask`)

herdr 0.9.3 does not read `plugin.toml`. Quoted from
`plugins.mdx` (linked by https://herdr.dev/llms.txt, tag v0.9.3):

> A plugin is a directory with a `herdr-plugin.toml` manifest and commands Herdr can launch.
> Top-level `id`, `name`, `version`, and `min_herdr_version` are required.
> `command` values are argv arrays. Herdr does not run them through a shell

The manifest is therefore `herdr-plugin.toml`; no `plugin.toml` exists, and
`tests/plugin_manifest.rs` asserts that. The plan should name `herdr-plugin.toml`.
Verified with `herdr plugin link . --disabled` (accepted, listed as `pod`).

## ASSUMPTION: entries

- ASSUMPTION: plugin `id` and `name` are `pod`; `version` is `0.1.0` (matches Cargo.toml).
- ASSUMPTION: the `[[build]]` command is `["sh", "scripts/install.sh"]` (argv, no shell, script is not required to be executable).
- ASSUMPTION: `open-board` runs `pod board`; `doctor` runs `pod doctor`; both rely on `pod` being on `PATH` (install.sh links `~/.local/bin/pod`).
- ASSUMPTION: `worker-list` opens a popup pane. It is an action that runs `herdr plugin pane open --plugin pod --entrypoint worker-list`, plus a `[[panes]]` entry `worker-list` with `placement = "popup"` running `pod worker-list`. Actions carry no `contexts` key (herdr accepted the manifest without it).
- ASSUMPTION: `skills get --reference <name>` accepts the file names under `docs/reference` without extension (commands.md already states this).
- ASSUMPTION: `--full` is the guide followed by all six reference pages in alphabetical file order, because the guides contain no links to reference pages. Pages are separated by a blank line.
- ASSUMPTION: `--reference` takes precedence over `--full` when both are given; it is accepted with either role.
- ASSUMPTION: text mode prints the markdown verbatim (byte-equal to the file); `--json` returns `{"ok": true, "content": "..."}`.
- ASSUMPTION: an unknown role or reference is a usage error (exit 2, code `usage`); role is already rejected by clap, the library function also rejects it.
- ASSUMPTION: release assets are `pod-<tag>-<target>.tar.gz` plus `.tar.gz.sha256` (sha256sum format) and a combined `SHA256SUMS`, targets `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`. Tags matching `v*` trigger the release; the runners are `macos-14`, `macos-13`, `ubuntu-24.04`, `ubuntu-24.04-arm`.
- ASSUMPTION: `install.sh` treats a release as matching only when `HEAD` has an exact tag; otherwise it runs `cargo build --release`. The download repo comes from `POD_RELEASE_REPO` or the `origin` remote (GitHub URLs only). A checksum mismatch aborts rather than falling back to source. `POD_INSTALL_FROM_SOURCE=1` forces a source build; `POD_INSTALL_BIN_DIR` overrides `~/.local/bin` (used for testing).
- ASSUMPTION: the linked binary is `<checkout>/target/release/pod` (symlink), so a downloaded release is placed there too.
- ASSUMPTION: CI uses the toolchain pinned in `rust-toolchain.toml` (1.97.1) on `macos-14` and `ubuntu-24.04`, runs `cargo fmt --check`, and shellchecks the install script. Workflows are never run or pushed from this worktree.
- ASSUMPTION: `herdr_plugin_link_dry_run` falls back to `herdr plugin uninstall pod` when `unlink` fails: on 0.9.3 `unlink` needs a running herdr server, while `link` and `uninstall` do not. It is `#[ignore]` and also returns early unless `POD_E2E=1`.
- ASSUMPTION: the SKILL.md stubs carry `name`/`description` frontmatter (`pod-coordinator`, `pod-worker`) for `npx skills add`.

## Not done

- `install.sh` release download is untested against a real release (no remote exists); the source-build path was run end to end.
- Hook semantics (`pod hook startup|event`) and `pod board`, `pod worker-list` belong to other tasks; the manifest only names them.

## Edit outside Ownership (approved by the coordinator)

Deleted the single `&["skills", "get", "worker", "--reference", "errors", "--full"]` line from the not_implemented list in `tests/t0.rs::cli_commands_accept_documented_flags`; `tests/cli_skills.rs` covers `--reference` and `--full`. Nothing else in `tests/t0.rs` changed.

## Review (Opus)

Checked against spec 3.5, 15 and 18, `docs/reference/commands.md` and
`errors.md`, the T12 test list, and herdr v0.9.3's `plugins.mdx` and
`src/cli/plugin.rs` (install flow). All five listed tests exist under their
listed names. Main already renames the manifest to `herdr-plugin.toml` in the
plan (bb33ad9), so the plan is not edited here. `git merge-tree` against main
reports no conflicts.

Verdicts on the ASSUMPTION entries:

- id/name `pod`, version `0.1.0`: accepted.
- `[[build]]` is `["sh", "scripts/install.sh"]`: accepted. herdr runs build
  commands from the plugin root with stdin closed.
- `open-board` and `doctor` rely on `pod` on `PATH`: accepted. Their output
  goes to the plugin log, not a pane; that matches spec 3.5, which asks for a
  popup only for `worker-list`.
- `worker-list` action plus `[[panes]]` popup: corrected. A herdr popup closes
  when its command exits, so `pod worker-list` flashed and vanished. The pane
  now waits for Enter after printing. The action calls herdr through
  `HERDR_BIN_PATH` (falling back to `herdr`), as plugins.mdx recommends. The
  manifest test now checks the popup pane.
- `--reference` names are file stems: accepted. Added a check that every page
  under `docs/reference` is served, so a new page cannot be silently missing.
- `--full` is the guide plus all six references: accepted. The plan says
  "every reference the guide links", and the guides link none; commands.md
  says "all its references", which this satisfies. Correction to the note: the
  order is the `REFERENCES` table order in `src/skills.rs` (`commands` first),
  not file-name order (`commands-messages.md` sorts before `commands.md`).
- `--reference` wins over `--full`: accepted.
- Text mode verbatim, `--json` as `{"ok": true, "content": ...}`: accepted.
- Unknown role or reference is a usage error (exit 2, `usage`): accepted;
  `usage` is in errors.md.
- Release asset names and targets: corrected. GitHub retired the `macos-13`
  image, so the x86-64 macOS job would never get a runner; it now
  cross-compiles on `macos-14`. Not run (no remote, x86-64 target not
  installed locally).
- install.sh release matching by exact tag: corrected. `herdr plugin install`
  fetches `--depth 1` with no tags, so the exact-tag check never matched and
  the download path was dead for every real install. It now falls back to
  `git ls-remote --tags origin` for the tag that points at HEAD.
  `POD_RELEASE_REPO`, `POD_INSTALL_FROM_SOURCE`, `POD_INSTALL_BIN_DIR` kept.
- Linked binary is `<checkout>/target/release/pod`: corrected. herdr runs the
  build in `<config>/herdr/plugins/.tmp-install-*/checkout` and renames that
  directory afterwards, so `~/.local/bin/pod` dangled after every
  `herdr plugin install`. When the checkout is under `*/herdr/plugins/*`, the
  script now copies the binary to `${XDG_DATA_HOME:-~/.local/share}/pod/bin/pod`
  (override `POD_INSTALL_LIB_DIR`) and links that. A linked working tree is
  still linked directly so rebuilds take effect.
- CI toolchain, runners, fmt, shellcheck: corrected. The shellcheck step runs
  on Linux only: the Ubuntu image ships shellcheck, the macOS one may not,
  and one run covers the POSIX script.
- `herdr_plugin_link_dry_run` unlink falls back to uninstall: accepted, but
  the test linked and unlinked `pod` in the user's real, global plugin
  registry, which would remove an existing real link. It now sets
  `XDG_CONFIG_HOME` and `XDG_STATE_HOME` to a temp dir and drops inherited
  `HERDR_*` variables. Ran it with `POD_E2E=1 --include-ignored`: passes, and
  `~/.config/herdr/plugins.json` is untouched.
- SKILL.md frontmatter names: accepted.

Other fixes in install.sh: binaries are replaced by copy-then-rename, not
`cp` over a file a running hook may be executing (macOS kills a binary
modified in place); `cargo build` gets `--target-dir` so `CARGO_TARGET_DIR`
cannot leave `target/release/pod` missing; the script fails clearly when no
binary exists; `ln -sfn` replaces an existing link.

Verified with fake `curl` and `cargo` on PATH and a local bare origin: managed
install from a release (tag found via ls-remote, link survives the rename),
checksum mismatch (exit 1, previous binary kept), missing release (source
build), and the real `cargo` source path in this worktree.

Not verified: the GitHub workflows (never pushed), a download from a real
release, the popup in a running herdr, and Linux.

Left as is, for the owner: uninstalling the plugin leaves `~/.local/bin/pod`
and the copied binary in place. Linux release binaries are built on Ubuntu
24.04 and need glibc 2.39 or newer.
