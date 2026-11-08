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
- **New socket methods** must be classified in the daemon's access table
  (`crates/mirante-daemon/src/access.rs`). Unclassified methods are person-only, and the
  tests fail until they are classified.

## Checks

Run these before you open a PR. CI, once set up, will run the same ones.

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # rustup's toolchain, not Homebrew cargo
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
pnpm --dir apps/cockpit test
pnpm --dir apps/cockpit typecheck
```

Format the Rust files you touch with `rustfmt` (the repo has a `rustfmt.toml`; not every
existing file is formatted yet, so don't reformat unrelated files in your PR).

For app changes, also run the smoke test (see `README.md`, Development). For changes to
agent behavior, also run the relevant checklist in `docs/dogfood/` against real agents.

## License of contributions

Contributions are accepted under the Apache License 2.0 (see `LICENSE`, section 5). A
contributor agreement may be introduced before 1.0. If that happens, it will be announced
here before it applies.
