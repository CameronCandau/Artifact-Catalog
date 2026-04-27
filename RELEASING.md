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

Before publishing, do one last review of:

- crate metadata in `Cargo.toml`
- the public-facing `README.md`
- any checked-in catalog examples you do or do not want associated with your public profile

```bash
cargo publish
```

## Publish Catalog Contents

This is separate from publishing the Rust crate itself.

GitHub Releases backend:

```bash
GITHUB_TOKEN=... locker publish vYYYY-MM-DD
```

OCI backend:

```bash
LOCKER_BACKEND=oci-registry
ARTIFACT_CATALOG_OCI_REPOSITORY=public.ecr.aws/alias/artifact-catalog
locker publish vYYYY-MM-DD
```

## Post-release Checks

- confirm `locker doctor` is clean for the intended backend
- confirm `locker sync --dry-run` resolves the published catalog
- confirm README install and init steps still match the released CLI
- confirm the crates.io page renders the README and metadata as expected
