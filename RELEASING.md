# Releasing Mirante

A release is a `v<version>` tag. Pushing it runs
[`.github/workflows/release.yml`](.github/workflows/release.yml), which:

1. **build** (macOS, one job per target): builds `mirante`, `mirante-daemon`, `mirante-hook`
   and `mirante-mcp` with the toolchain pinned in `rust-toolchain.toml`
   (`cargo build --release --locked --target <target>`) for `aarch64-apple-darwin` and
   `x86_64-apple-darwin`, from a clean tree (no build cache), and packages
   `mirante-<version>-<target>.tar.gz`: the four binaries, `LICENSE`,
   `THIRD_PARTY_NOTICES.md` and `README.md` in one top-level folder. The native target is
   smoke-tested (`--version`, `help`). The job fails if the tag is not the version in
   `crates/mirante-cli/Cargo.toml`.
2. **release** (the only job with `contents: write`): writes `SHA256SUMS` and creates the
   GitHub Release with the two tarballs and `SHA256SUMS`. The notes are the version's
   section of `CHANGELOG.md`. If the release already exists, it replaces the assets.
3. **homebrew-tap** (optional): when the `HOMEBREW_TAP_TOKEN` secret exists, rewrites
   `Formula/mirante.rb` in
   [sirus-alpha-canis/homebrew-tap](https://github.com/sirus-alpha-canis/homebrew-tap) with
   `scripts/homebrew-formula.sh` (new version, URLs and sha256) and pushes it to `main`.
   Without the secret the job only prints a notice and stays green; update the tap by hand
   (below).

Nothing is signed with a Developer ID or notarized yet. The Apple silicon binaries carry the
ad-hoc signature the linker adds (macOS requires one there); a command-line tool installed
by Homebrew or `curl` is not quarantined, so Gatekeeper does not block it. The desktop app
is not released: it waits for an Apple Developer ID (signing and notarization), and is
built from source.

## Cut a release

1. Every crate in `crates/` and the app (`apps/cockpit/package.json`,
   `apps/cockpit/src-tauri/Cargo.toml`, `apps/cockpit/src-tauri/tauri.conf.json`) carry
   the new version; `cargo build` updates `Cargo.lock` (commit it). `mirante --version`
   prints it.
2. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [<version>] — <YYYY-MM-DD>`, add a
   new empty `## [Unreleased]` above it, and update the link references at the bottom.
3. Commit, push `main`, and wait for CI to be green.
4. Tag and push the tag:

   ```bash
   git tag -a v<version> -m "Mirante <version>"
   git push origin v<version>
   ```

5. Watch the run (Actions → Release). To rebuild or republish an existing tag, run the
   workflow by hand (**Run workflow**, with `tag` = `v<version>`).
6. Check the release page, then the formula:

   ```bash
   brew update
   brew upgrade sirus-alpha-canis/tap/mirante   # or: brew install sirus-alpha-canis/tap/mirante
   mirante --version
   brew test sirus-alpha-canis/tap/mirante
   ```

## `HOMEBREW_TAP_TOKEN`

A secret in this repository (Settings → Secrets and variables → Actions → New repository
secret) that lets the release workflow push to the tap. Use a fine-grained personal access
token:

- Resource owner: `sirus-alpha-canis`; repository access: only
  `sirus-alpha-canis/homebrew-tap`.
- Repository permissions: **Contents: Read and write** (Metadata: read is added
  automatically). Nothing else.
- Give it an expiry and put a reminder to rotate it; when it expires the tap job fails
  (the release itself is already published by then).

The token is only exposed to the `homebrew-tap` job; the `GITHUB_TOKEN` of this repository
cannot push to another repository.

## Update the tap by hand

When the secret is not set (or the tap job failed), from a checkout of this repository:

```bash
v=<version>
curl -fsSLO "https://github.com/sirus-alpha-canis/mirante/releases/download/v$v/SHA256SUMS"
arm=$(awk "/mirante-$v-aarch64-apple-darwin.tar.gz/ {print \$1}" SHA256SUMS)
intel=$(awk "/mirante-$v-x86_64-apple-darwin.tar.gz/ {print \$1}" SHA256SUMS)
scripts/homebrew-formula.sh "$v" "$arm" "$intel" > ../homebrew-tap/Formula/mirante.rb
cd ../homebrew-tap
brew style Formula/mirante.rb
git commit -am "mirante $v" && git push
```
