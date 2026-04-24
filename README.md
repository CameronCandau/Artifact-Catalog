# Artifact Catalog

Artifact mirror for pentest workstation tooling.

This repo tracks:

- approved artifacts and their metadata
- SHA256 checksums
- staged release assets
- simple publish tooling for public GitHub Releases

## Layout

```text
Artifact-Catalog/
├── builds/
├── checksums/
├── manifests/
├── scripts/
└── staging/
    └── release-assets/
```

## Source Of Truth

`manifests/artifacts.yaml` is the catalog source of truth. It uses JSON-compatible YAML so it can be edited and validated without extra dependencies.

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

For source-built artifacts, keep the provenance in `source_ref` too:

```text
https://github.com/OWNER/REPO@<full-commit-sha>
```

Example for a locally built Windows binary:

```bash
scripts/add-artifact.sh /path/to/SweetPotato.exe \
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
scripts/add-artifact.sh /path/to/Rubeus.exe \
  --platform windows \
  --category bin \
  --filename Rubeus-net48.exe \
  --version git-a1b2c3d-net48-x64 \
  --source-type built \
  --source-ref https://github.com/Flangvik/SharpCollection@a1b2c3d4e5f678901234567890abcdef12345678
```

## Workflow

### 1. Add a local artifact you already trust

```bash
scripts/add-artifact.sh /path/to/file \
  --platform windows \
  --category bin \
  --filename winpeas.exe \
  --version 2025.01 \
  --source-type local
```

This:

- copies the file into `staging/release-assets/`
- computes SHA256
- updates `manifests/artifacts.yaml`
- rebuilds `checksums/sha256sums.txt`

The staged release asset is stored locally at:

```text
staging/release-assets/<platform>--<category>--<filename>
```

That directory is intentionally gitignored, so the staged binaries do not show up in normal git status output.

### 1b. Add directly from a GitHub release URL

```bash
scripts/add-artifact.sh \
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
scripts/import-github-release.sh \
  https://github.com/OWNER/REPO/releases/download/v1.2.3/tool.exe \
  --platform windows \
  --category bin \
  --filename tool.exe \
  --version v1.2.3
```

This is a convenience wrapper around `add-artifact.sh` for GitHub release asset URLs. It also records the original GitHub release URL in `source_ref`.

### 2. Verify the catalog

```bash
scripts/verify-artifacts.sh
```

### 3. Publish a release

```bash
GITHUB_TOKEN=... scripts/publish-release.sh v2026-04-23
```

Defaults:

- `GITHUB_OWNER=CameronCandau`
- `GITHUB_REPO=Artifact-Catalog`

Override them if your GitHub repo uses a different owner or name.

If publishing fails with HTTP `422`, the script now prints the GitHub API response body. Common causes are:

- the repository has no initial commit/default branch yet
- the token lacks `Contents: Read and write`
- the release/tag already exists in an invalid state

### 4. Use it from `OSCP-Automation`

`OSCP-Automation/bin/refresh-payloads.sh` now pulls from this mirror by default:

```bash
refresh-payloads
```

Useful overrides:

```bash
ARTIFACT_CATALOG_OWNER=CameronCandau
ARTIFACT_CATALOG_REPO=Artifact-Catalog
ARTIFACT_CATALOG_BASE_URL=https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download
ARTIFACT_CATALOG_MANIFEST_URL=https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download/artifacts.yaml
ARTIFACT_CATALOG_CHECKSUMS_URL=https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download/sha256sums.txt
```

## Notes

- Release assets are published with deterministic names: `platform--category--filename`
- `artifacts.yaml` and `sha256sums.txt` are also uploaded as release assets
- V1 keeps containerized builds out of the critical path; see `builds/README.md`
