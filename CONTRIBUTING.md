# Contributing to Mirante

Thanks for helping. Mirante is an early prototype, so open an issue first for anything
larger than a small fix. That way we can agree on the shape before you write it.

## Clean-room rule

Never copy code from other projects, whatever their license. That includes snippets,
translated code and "lightly adapted" code. Implement from public specs, protocol docs and
your own understanding, and say in the PR which spec you used. Studying how other tools
behave is fine; reproducing their source is not. Third-party code arrives only as declared
dependencies, under their own licenses (see `THIRD_PARTY_NOTICES.md`).

## How we work

- **Test first.** Write a failing test, make it pass, then clean up. Bug fixes start with a
  test that reproduces the bug.
- **One PR per change.** Keep each PR to one fix or one feature, with a message that says
  why.
- **Keep the invariants.** Status comes from hooks and protocol events, never screen
  scraping. Nothing is auto-approved. Agents never get a person's methods on the daemon
  socket. No network service, account or telemetry. A change that touches one of these
  needs a discussion first.
- **Fake executables in tests** are written with `mirante_core::test_script::write_sh`, which
  runs each new script once before the test times it: on macOS the first exec of a fresh
  file can take seconds (XProtect scans it), long enough to trip a test's timeout.
- **New socket methods** must be classified in the daemon's access table
  (`crates/mirante-daemon/src/access.rs`). Unclassified methods are person-only, and the
  tests fail until they are classified.

## Checks

Run these before you open a PR. CI runs the same ones (see below), on the same Rust: the
toolchain is pinned in `rust-toolchain.toml` (1.99.1, with clippy and rustfmt), which also
covers `apps/cockpit/src-tauri`. Bump it in its own PR.

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # rustup's toolchain, not Homebrew cargo
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features mirante-bridge/fake -- -D warnings
pnpm --dir apps/cockpit test
pnpm --dir apps/cockpit typecheck
cargo deny check                   # if you changed dependencies (cargo install cargo-deny --locked)
```

Dependabot ([`.github/dependabot.yml`](.github/dependabot.yml)) opens weekly update PRs for
the actions, both Cargo workspaces and the app's npm packages.

### CI

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every pull request and every
push to `main` (a new push to a pull request cancels its older run; runs on `main` always
finish). It uses no secrets and has read-only access to the repo.

- **rust (macOS)**, the main gate: `cargo build --workspace --all-targets`,
  `cargo test --workspace` (the e2e tests run against a fake `claude`; the real-agent tests
  stay ignored) and both clippy passes above with `-D warnings`.
- **rust (Linux)**: clippy on the whole workspace (`-D warnings`, as on macOS), and the `mirante-core` and `mirante-ipc`
  tests as the gate. The rest of the workspace's tests also run there, but only as
  information, until the daemon's Linux paths have been proven.
- **cockpit (macOS)**: in `apps/cockpit`, `pnpm install --frozen-lockfile`, `pnpm test`,
  `pnpm typecheck` and `pnpm build`, then `cargo test` and
  `cargo clippy --all-targets -- -D warnings` in `src-tauri` (its own workspace).
- **cargo deny**: licenses and RustSec advisories ([`deny.toml`](deny.toml)) for the
  workspace and for `apps/cockpit/src-tauri`. Locally: `cargo install cargo-deny --locked`,
  then `cargo deny check` and `cargo deny --manifest-path apps/cockpit/src-tauri/Cargo.toml
  check` from the repo root. A new dependency under another license needs a discussion.
- **shellcheck**: `scripts/install.sh`, `uninstall.sh` and `try.sh` (warnings and errors).
- **install.sh (macOS, main only)**: `scripts/install.sh` into a temp prefix with its own
  `MIRANTE_HOME`, `mirante --version`, `daemon status`, `doctor`, a second `install.sh`
  run, `scripts/try.sh --smoke` and `scripts/uninstall.sh`.
- **rustfmt (non-blocking)**: `cargo fmt --all --check`, reported as a warning only, since
  the tree is not formatted yet.

CI does not run the app smoke test or the `docs/dogfood/` checklists: they open windows
or need real agents.

Format the Rust files you touch with `rustfmt` (the repo has a `rustfmt.toml`; not every
existing file is formatted yet, so don't reformat unrelated files in your PR).

For app changes, also run the smoke test (see `README.md`, Development). For changes to
agent behavior, also run the relevant checklist in `docs/dogfood/` against real agents.

## License of contributions

Contributions are accepted under the Apache License 2.0 (see `LICENSE`, section 5). A
contributor agreement may be introduced before 1.0. If that happens, it will be announced
here before it applies.
