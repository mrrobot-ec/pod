# Contributing to pod

pod is a personal project published for the herdr community. Bug reports, fixes, docs, and new agent launch tables are welcome. Design changes are welcome too, but start as an issue.

## Before you start

- Read the [README](README.md) and the [design spec](docs/specs/2026-10-05-pod-design.md). The spec is the single source of truth: a behavior change starts with a spec change, and the code and docs follow it.
- Open an issue before writing code for anything that adds a command, changes a JSON shape, or changes a rule in the spec. A small fix or a doc correction can go straight to a pull request.
- pod keeps Orca's orchestration contract (command names, flags, JSON shapes; spec section 14). Additions are fine; renames and removals are not.

## Set up

You need herdr 0.9.3 or newer, Rust (`rust-toolchain.toml` pins the toolchain and installs it on the first build), and at least one agent herdr supports.

```sh
git clone https://github.com/mrrobot-ec/pod.git
cd pod
POD_INSTALL_FROM_SOURCE=1 sh scripts/install.sh   # cargo build --release, links ~/.local/bin/pod
herdr plugin link "$PWD" --enabled
pod doctor
```

Rerun the install script after changing code; the link points at `target/release/pod`.

## Checks every change must pass

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` is deterministic and needs no herdr: the suite runs against a fake herdr, and timing targets are asserted in release builds only. Two more suites exist for changes that touch herdr or the agents:

- `scripts/e2e.sh` runs the end-to-end suite against a real herdr on an isolated session. It needs `herdr` on `PATH`. CI runs it on macOS and Linux as an advisory job.
- `scripts/live.sh` starts real `claude` and `codex` workers on an isolated session and spends model tokens. Run it for changes to launch, input, the wake ladder, or attention, and attach the results file it writes under `docs/reviews/live/`.

## Conventions the tests enforce

- Every file under `docs/` stays at or under 400 lines, except the design spec.
- No em-dashes anywhere in docs, source, tests, scripts, or skills.
- Every command-line flag pod defines is documented in the README or under `docs/`, and no doc names a flag that does not exist (`tests/docs_sync.rs`).
- `docs/reference/errors.md` lists exactly the error codes the binary defines.
- Every sample under `tests/contract/samples/` has a contract test.
- The guides under `docs/guides/` are embedded into the binary and served by `pod skills get`, so a guide change is a code change and ships with the next release.

## Pull requests

- Conventional commit messages (`feat:`, `fix:`, `docs:`, `test:`, `chore:`), in small commits.
- A behavior change comes with the spec section it changes, tests that pin it, and a line in `CHANGELOG.md` under the next version.
- Say how you verified it: which of the three suites ran, and on which herdr and agent versions.
- Keep the docs in plain language. Say what pod does and what the person sees; do not describe the implementation.

## Releases

Maintainers bump the version in `Cargo.toml` and `herdr-plugin.toml`, move the changelog's entry to the new version, and push a `vX.Y.Z` tag. The release workflow builds the four binaries with checksums. The install script downloads the release that matches the installed commit, else the latest release, and builds from source only when there is no release or the download fails, so a commit pushed after a tag still installs a prebuilt binary.

## License

By contributing you agree that your contribution is licensed under the MIT License in [LICENSE](LICENSE). Third-party material and its notices are listed in [NOTICE](NOTICE).
