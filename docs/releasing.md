# Releasing

cstore ships the `cstore` crate to crates.io and `@corbet-labs/cstore` to npm,
always with the same version. A pushed `vX.Y.Z` tag is the one release
trigger; `.github/workflows/release.yml` never holds a registry token.

## What CI does

1. Checks that the tag equals `v` + the version in `Cargo.toml`, and that
   `package.json` and `package-lock.json` record the same version.
2. Runs `check.yml` as a reusable workflow with `rust,core,typescript`.
3. Builds `cstore-X.Y.Z.crate` with `cargo package --locked` and
   `corbet-labs-cstore-X.Y.Z.tgz` with `npm pack`, then attaches both plus a
   `SHA256SUMS` file to a GitHub release created with `--verify-tag`. Tags
   containing `-` become prereleases. An existing release is never modified.
4. Publishes to crates.io through trusted publishing (OIDC) only when the
   repository variable `CRATES_TRUSTED` is `true`. It is unset (off) until
   the crate exists and has a trusted-publisher rule. A version already on
   crates.io is skipped.

A rehearsal runs everything except the release and publication:
`gh workflow run release.yml -R corbet-libs/cstore --ref main`. The bundle is
kept as the `cstore-release-X.Y.Z` workflow artifact. To recreate a missing
release, dispatch on the tag: `gh workflow run release.yml --ref vX.Y.Z -f tag=vX.Y.Z`.

## Steps for a release

1. Bump the version in `Cargo.toml`, `Cargo.lock`, `package.json` and
   `package-lock.json` (both entries); push to `main`.
2. Push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`, then wait for the
   Release workflow to finish.
3. Download and verify the bundle:

   ```sh
   gh release download vX.Y.Z -R corbet-libs/cstore -D cstore-release
   cd cstore-release && sha256sum --check --strict SHA256SUMS
   ```

4. Publish the npm tarball with the npm token from sops:

   ```sh
   umask 077; rc=$(mktemp)
   printf '//registry.npmjs.org/:_authToken=%s\n' \
     "$(sops --decrypt ~/agents/knowledge/secrets/npm.yml | yq -r .api_token)" > "$rc"
   npm publish ./corbet-labs-cstore-X.Y.Z.tgz --userconfig "$rc" --access public
   shred -u "$rc"
   ```

   Stable versions go to `latest`. For a prerelease add `--tag next`, so that
   `latest` keeps pointing at the newest stable version.

5. First crates.io release only (while `CRATES_TRUSTED` is off): publish from
   the tag with the crates.io token from sops. CI already verified the build,
   so skip the local compile:

   ```sh
   git clone --depth 1 --branch vX.Y.Z https://github.com/corbet-libs/cstore cstore-tag
   cd cstore-tag
   CARGO_REGISTRY_TOKEN="$(sops --decrypt ~/agents/knowledge/secrets/crates-io.yml | yq -r .api_token)" \
     cargo publish --locked --no-verify
   ```

   With the same stable cargo as CI, the `checksum` at
   `https://crates.io/api/v1/crates/cstore/X.Y.Z` equals the
   `cstore-X.Y.Z.crate` line in `SHA256SUMS`; another cargo version may
   normalize the manifest differently, so then compare the unpacked contents.

## Trusted publishing

- crates.io: right after the first publish, create the trusted-publisher
  rule for `corbet-libs/cstore` with workflow `release.yml` (crates.io API
  `trusted_publishing/github_configs`), then enable CI publication:
  `gh variable set CRATES_TRUSTED --body true -R corbet-libs/cstore`.
  Step 5 is then no longer needed.
- npm: once trusted publishing is configured for `corbet-libs/cstore` with
  `release.yml` (it needs the owner's 2FA; planned for December 2026 when the
  stored token expires), add an OIDC `npm publish` job and drop step 4.
