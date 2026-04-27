# Artifact Catalog

`artifact-catalog` is a CLI for managing a small curated set of pentest and
red-team artifacts. It tracks artifact metadata, checksums, staged release
assets, and local sync state.

This repo tracks:

- artifact metadata
- SHA256 checksums
- staged release assets
- publish tooling for GitHub Releases or OCI registries
- a Rust CLI/TUI for managing the catalog

## Layout

```text
Artifact-Catalog/
├── src/
├── Cargo.toml
├── builds/
├── checksums/
├── manifests/
├── scripts/
└── staging/
    └── release-assets/
```

## Source Of Truth

`manifests/artifacts.yaml` is the catalog source of truth. It uses JSON-compatible YAML so it can be edited and validated without extra dependencies.

Use `artifact-catalog` when you want to:

- keep a checked and versioned list of approved assessment artifacts
- stage artifacts for release publication
- publish catalog contents to GitHub Releases or an OCI registry
- sync the published catalog into a local working directory

The primary interface is the Rust `locker` CLI.

For contributors inside this repo, `scripts/locker` is the easiest entrypoint.
For installed use, `locker` supports a default catalog root under
`$XDG_DATA_HOME/artifact-catalog` (falling back to
`~/.local/share/artifact-catalog`).

Catalog root precedence is:

1. `--root <PATH>`
2. `LOCKER_ROOT`
3. a detected repo checkout
4. the XDG default root

Each artifact entry contains:

- `platform`
- `category`
- `filename`
- `version`
- `source_type`
- `source_ref`
- `sha256`
- `release_asset_name`
- `active`

## Versioning Convention

Use source-based versions that reflect where the artifact came from.

Recommended patterns:

- Official upstream release:
  - `v<upstream-version>`
  - example: `v2.1.0`
- No official release, built from source:
  - `git-<shortsha>-<arch>`
  - example: `git-a1b2c3d-x64`
- Built from source with local modifications:
  - `git-<shortsha>-<arch>-patched`
  - example: `git-a1b2c3d-x64-patched`

Avoid vague values like:

- `latest`
- `1.0`
- `custom`
- `x64`

For source-built artifacts, prefer keeping commit-pinned provenance in `source_ref`:

```text
https://github.com/OWNER/REPO@<full-commit-sha>
```

Older catalog entries may still need provenance backfill; use the pinned form for new or refreshed built artifacts.

Example for a locally built Windows binary:

```bash
scripts/locker add /path/to/SweetPotato.exe \
  --platform windows \
  --category bin \
  --filename SweetPotato.exe \
  --version git-a1b2c3d-x64 \
  --source-type built \
  --source-ref https://github.com/CCob/SweetPotato@a1b2c3d4e5f678901234567890abcdef12345678
```

## Variant Naming Convention

For compatibility-sensitive tools, treat framework and architecture variants as separate curated artifacts.

Do not model this as one artifact with many loosely tracked versions. Model it as a small set of explicit filenames.

Recommended naming pattern:

- `<tool>-net48`
- `<tool>-net35`
- `<tool>-net20`
- `<tool>-x64`
- `<tool>-x86`
- combine when needed: `<tool>-net48-x64`

Recommended filename pattern:

- `Rubeus-net48.exe`
- `Rubeus-net35.exe`
- `Seatbelt-net48.exe`

Recommended version pattern for built variants:

- `git-<shortsha>-net48-x64`
- `git-<shortsha>-net35-x64`

Practical rule:

- keep one primary modern build
- keep one legacy compatibility build if you actually need it
- add more variants only when they solve a real target-driven problem

Avoid:

- mirroring every historical version
- keeping many near-identical binaries with unclear naming
- using one filename to represent multiple framework variants

Example:

```bash
scripts/locker add /path/to/Rubeus.exe \
  --platform windows \
  --category bin \
  --filename Rubeus-net48.exe \
  --version git-a1b2c3d-net48-x64 \
  --source-type built \
  --source-ref https://github.com/Flangvik/SharpCollection@a1b2c3d4e5f678901234567890abcdef12345678
```

## Scope

The catalog is meant to stay small and explicit rather than mirror everything.
Typical entries include:

- third-party release assets mirrored with checksums
- internally built artifacts with pinned provenance
- scripts and other transferable text assets
- compatibility-specific variants where filename and versioning matter

General curation rules:

- prefer official upstream releases when they exist
- use `built` with pinned commit provenance when you compile artifacts locally
- keep variants explicit in filenames when compatibility matters
- treat scripts as first-class artifacts with `category: scripts`

## Workflow

### Install and initialize

Contributor workflow:

```bash
scripts/locker init
```

Installed CLI workflow:

```bash
cargo install artifact-catalog
locker init
```

Use an explicit root when you want a portable project directory instead of the
default XDG location:

```bash
locker --root /path/to/catalog init
```

### CLI commands

```bash
scripts/locker init
scripts/locker add /path/to/file
scripts/locker list
scripts/locker list --platform windows --synced false
scripts/locker list --json
scripts/locker show SweetPotato.exe
scripts/locker show SweetPotato.exe --json
scripts/locker verify
scripts/locker doctor
scripts/locker publish v2026-04-23
scripts/locker sync
scripts/locker tui
```

The `add` flow:

- copies the file into `staging/release-assets/`
- computes SHA256
- updates `manifests/artifacts.yaml`
- rebuilds `checksums/sha256sums.txt`
- infers `filename`, `source_type`, `source_ref`, and release `version` when possible
- suggests `platform` and `category` from the filename
- prompts for missing metadata unless you pass it directly
- supports `-y/--yes` for fast non-interactive adds using inferred/default values

If you already built the debug binary, you can run:

```bash
cargo run --bin locker -- add /path/to/file
```

The staged release asset is stored locally at:

```text
staging/release-assets/<platform>--<category>--<filename>
```

That directory is intentionally gitignored, so the staged binaries do not show up in normal git status output.

### 1b. Add directly from a GitHub release URL

```bash
scripts/locker add \
  https://github.com/OWNER/REPO/releases/download/v1.2.3/tool.exe \
  --platform windows \
  --category bin \
  --filename tool.exe \
  --version v1.2.3 \
  --source-type github-release
```

When the source is a URL and `--source-ref` is not provided, `source_ref` is automatically set to the exact URL you passed.

### 1c. Import from an existing GitHub release asset

```bash
scripts/locker add \
  https://github.com/OWNER/REPO/releases/download/v1.2.3/tool.exe \
  --platform windows \
  --category bin \
  --filename tool.exe \
  --version v1.2.3
```

This is the same happy path as a generic URL add.

### 1d. Browse and inspect

```bash
scripts/locker list --platform windows
scripts/locker list --active true --synced false
scripts/locker list --json
scripts/locker show SweetPotato.exe
scripts/locker show SweetPotato.exe --json
```

`list` now includes staged and synced state so you can see whether an artifact is only in the catalog, already staged, or already present in your local payload shelf.

### 2. Verify the catalog

```bash
scripts/locker verify
```

### 3. Publish a release

```bash
GITHUB_TOKEN=... scripts/locker publish v2026-04-23
```

Defaults:

- `GITHUB_OWNER=CameronCandau`
- `GITHUB_REPO=Artifact-Catalog`

Override them if your GitHub repo uses a different owner or name.

Storage backend defaults to GitHub Releases. The backend is now explicit:

```bash
LOCKER_BACKEND=github-releases
```

OCI registry backend:

```bash
LOCKER_BACKEND=oci-registry
ARTIFACT_CATALOG_OCI_REPOSITORY=public.ecr.aws/your-alias/artifact-catalog
scripts/locker publish v2026-04-23
```

OCI backend notes:

- `oras` must be installed and on `PATH`
- authenticate separately if your registry requires it, for example `oras login ...`
- each staged artifact is published to the configured OCI repository with tag `release_asset_name`
- `artifacts.yaml` is published under tag `artifacts-manifest`
- `sha256sums.txt` is published under tag `artifacts-sha256sums`
- `resolve-url` returns `oci://...` references when `LOCKER_BACKEND=oci-registry`
- set `ARTIFACT_CATALOG_OCI_PLAIN_HTTP=true` only for insecure local registries

If publishing fails with HTTP `422`, the script now prints the GitHub API response body. Common causes are:

- the repository has no initial commit/default branch yet
- the token lacks `Contents: Read and write`
- the release/tag already exists in an invalid state

### 4. Sync it locally

```bash
scripts/locker sync
scripts/locker sync --platform windows
scripts/locker sync --category bin
scripts/locker sync --only SweetPotato.exe
scripts/locker sync --dry-run
```

Synced files land under `~/tools/payloads/` by default, with metadata written to:

```text
~/tools/payloads/.locker/artifacts.json
```

Useful overrides:

```bash
ARTIFACT_CATALOG_OWNER=CameronCandau
ARTIFACT_CATALOG_REPO=Artifact-Catalog
ARTIFACT_CATALOG_BASE_URL=https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download
ARTIFACT_CATALOG_MANIFEST_URL=https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download/artifacts.yaml
ARTIFACT_CATALOG_CHECKSUMS_URL=https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download/sha256sums.txt
```

Useful OCI overrides:

```bash
ARTIFACT_CATALOG_OCI_REPOSITORY=public.ecr.aws/your-alias/artifact-catalog
ARTIFACT_CATALOG_OCI_MANIFEST_TAG=artifacts-manifest
ARTIFACT_CATALOG_OCI_CHECKSUMS_TAG=artifacts-sha256sums
ARTIFACT_CATALOG_OCI_PLAIN_HTTP=false
```

## Notes

- `locker init` scaffolds `manifests/`, `checksums/`, and `staging/release-assets/` under the selected catalog root
- Release assets are published with deterministic names: `platform--category--filename`
- `artifacts.yaml` and `sha256sums.txt` are published alongside staged assets on both backends
- `locker tui` can now search, reload, sync, verify, open an action menu, copy values to the clipboard, and show progress in the footer:
  - `/` search by filename
  - `Esc` clear the current filter
  - `s` sync the selected artifact
  - `S` sync the current filtered view
  - `v` verify the catalog
  - `Enter` open an action menu for the selected artifact
  - `R` reload manifest and local metadata
  - `a` toggle active
  - `y` filename
  - `p` synced or staged path
  - `u` source ref
  - `r` resolved download URL or `oci://` reference
- V1 keeps containerized builds out of the critical path; see `builds/README.md`
