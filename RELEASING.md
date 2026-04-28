# Releasing artifact-catalog

## Preflight

```bash
cargo check
cargo test
cargo publish --dry-run
cargo package --allow-dirty --list
```

Initialize a local catalog if needed:

```bash
locker init
```

## Publish To crates.io

```bash
cargo publish
```

GitHub Actions:

- pushes of tags matching `v<semver>` trigger `.github/workflows/publish-crate.yml`
- the pushed tag must match the package version in `Cargo.toml`
- `workflow_dispatch` runs the same preflight checks but does not publish
- the workflow expects `CARGO_REGISTRY_TOKEN` in repository secrets

Example:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Publish Catalog Contents

This is separate from publishing the Rust crate itself.

Recommended model:

- keep the `artifact-catalog` crate release workflow in this repo
- keep your real manifest/checksums and catalog publish automation in a separate catalog-content repo
- use this repo’s `examples/` and `scripts/bootstrap_release_assets.py` as reference material for that second repo

Recommended operator-first backend: OCI registry with `oras`.

Preconditions:

- `LOCKER_BACKEND=oci-registry`
- `ARTIFACT_CATALOG_OCI_REPOSITORY` points at the target registry/repository
- `oras` is installed and available on `PATH`
- registry auth is handled outside the app, for example with `oras login`

Example:

```bash
LOCKER_BACKEND=oci-registry
ARTIFACT_CATALOG_OCI_REPOSITORY=public.ecr.aws/alias/artifact-catalog
locker publish vYYYY-MM-DD
```

What gets published:

- `artifacts.yaml` as the OCI tag `artifacts-manifest`
- `sha256sums.txt` as the OCI tag `artifacts-sha256sums`
- each staged artifact under tag `object_name`

Fallback backend: GitHub Releases.

Preconditions:

- `GITHUB_TOKEN` is present
- `LOCKER_BACKEND=github-releases`

Example:

```bash
GITHUB_TOKEN=... locker publish vYYYY-MM-DD
```

Current implementation note:

- if no backend is configured, the code defaults to `oci-registry`
- GitHub Releases remains supported as an explicit fallback backend

Auth model:

- keep non-secret defaults such as backend, repo name, payload directory, and OCI repository in `~/.config/artifact-catalog/config.yaml`
- keep secrets out of config files
- provide GitHub auth through `GITHUB_TOKEN`
- provide OCI auth through `oras login` or your registry’s external credential flow

Integrity model:

- `locker publish` always runs `locker verify` first
- `locker verify` only proves the local staged catalog is internally consistent
- publish does not currently perform a post-upload round-trip verification of remote contents
- sync verifies each downloaded artifact against the manifest `sha256`
- local `.locker/artifacts.json` records sync state for operator workflows; it is not a tamper-evident store

GitHub Releases example with explicit backend:

```bash
LOCKER_BACKEND=github-releases
GITHUB_TOKEN=... locker publish vYYYY-MM-DD
```

OCI example with explicit backend:

```bash
LOCKER_BACKEND=oci-registry
ARTIFACT_CATALOG_OCI_REPOSITORY=public.ecr.aws/alias/artifact-catalog
locker publish vYYYY-MM-DD
```

## Post-release Checks

- confirm `locker doctor` is clean for the intended backend
- confirm `locker sync --dry-run` resolves the published catalog
- confirm a real `locker sync` of one representative artifact succeeds and hashes cleanly
- confirm README install and init steps still match the released CLI
- confirm the crates.io page renders the README and metadata as expected
