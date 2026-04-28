# Artifact Catalog Backlog

This backlog reshapes `artifact-catalog` into a practical operator artifact
catalog with OCI/ORAS as the primary transport model, ECR as a supported
managed backend, and GitHub Releases as a fallback backend.

Out of scope for these phases:

- SBOM support
- signatures and attestations
- reproducible build automation
- large-scale mirroring

## Epic

### Epic: Reposition `artifact-catalog` as an Operator-First Artifact Catalog

Problem:
The repo already has useful primitives, but the product shape is split between
"release asset mirror" and "catalog CLI". GitHub Releases still behaves like
the primary path, provenance is too flat, and local integrity semantics are not
consistent across commands.

Outcome:

- OCI/ORAS is the primary publish and sync path
- ECR is documented as a supported managed OCI backend
- GitHub Releases remains supported as fallback
- provenance is structured enough for upstream and locally built artifacts
- local state reporting reflects verified reality, not file presence alone

Success criteria:

- README and CLI help describe the tool as an operator-first curated catalog
- OCI/ORAS is the documented primary workflow
- GitHub Releases is consistently described as fallback
- provenance and integrity semantics are explicit and test-covered

## Phase 0

### Issue 0: Define and Document the Target Product Shape

Problem:
Current docs frame the tool around both GitHub Releases and OCI, which makes the
operating model ambiguous.

Scope:

- product positioning
- supported workflows
- backend priority and terminology
- catalog curation boundaries

Tasks:

- Update README language from generic artifact locker language to curated
  operator artifact catalog language.
- Add a short architecture section covering:
  - local manifest as source of truth
  - staged artifacts as publish inputs
  - ORAS as the primary transport interface
  - OCI registry as the primary distribution channel
  - ECR as a managed backend option
  - GitHub Releases as fallback
- Document supported artifact classes:
  - upstream release assets
  - locally built binaries
  - scripts and text assets
  - compatibility-specific variants
- Document non-goals for the current roadmap.

Acceptance criteria:

- A contributor can identify the primary transport, primary backend class, and
  fallback path within one minute of reading the README.
- README, config examples, and release docs consistently say OCI primary and
  GitHub fallback.
- No docs imply GitHub Releases is the preferred long-term design.

Dependencies:

- none

## Phase 1

### Issue 1: Design the Structured Provenance Schema

Problem:
`source_type` and `source_ref` are too lossy for practical provenance and too
weak for later audit or refresh workflows.

Scope:

- schema design
- field definitions
- validation rules
- example coverage

Tasks:

- Design a structured `provenance` object.
- Define provenance kinds for at least:
  - upstream release or direct download
  - locally built artifact from pinned source
  - local/manual ingestion
- Define fields such as:
  - `kind`
  - `uri`
  - `repo`
  - `tag` or `version_ref`
  - `commit`
  - `asset_name`
  - `archive_path`
  - `build_method`
  - `notes`
- Replace `source_type` and `source_ref` with the new structured schema.
- Rename `release_asset_name` to a backend-neutral field such as `object_name`.
- Add example manifests for:
  - GitHub release binary
  - archive-backed upstream asset
  - locally built pinned-commit artifact
  - manual local script ingestion

Acceptance criteria:

- The schema can represent both mirrored upstream artifacts and locally built
  artifacts without overloading one flat string field.
- Required fields are clear for each provenance kind.
- Example manifests and CLI usage match the new schema exactly.

Dependencies:

- Issue 0

### Issue 2: Implement Manifest Migration and CLI Schema Support

Problem:
Without a complete CLI and manifest cutover, the new provenance model will
leave the repo half-converted and error-prone.

Scope:

- manifest structs
- serialization and deserialization
- schema cutover
- CLI add/show/list support

Tasks:

- Update manifest structs to support the new provenance model.
- Update `add` to capture structured provenance cleanly.
- Update `show` and `list` output to render provenance readably.
- Update bootstrap tooling and local metadata as needed.

Acceptance criteria:

- Existing repo examples and tests use only the new schema.
- New manifests round-trip cleanly.
- `locker add` can create structured entries for both release-backed and built
  artifacts.
- Bootstrap tooling still works with the migrated schema.

Dependencies:

- Issue 1

### Issue 3: Add Provenance Validation and Operator-Focused Warnings

Problem:
The repo currently relies on README guidance for provenance quality, but the CLI
does not enforce or even warn on weak metadata.

Scope:

- CLI validation
- warning policy
- operator ergonomics

Tasks:

- Warn on weak version values such as `manual`.
- Warn on missing pinned commit metadata for built artifacts.
- Warn on unpinned repository references where a pinned commit is expected.
- Hard-fail only on clearly invalid cases, such as missing required fields for a
  provenance kind.
- Keep the workflow usable for common upstream prebuilt downloads.

Acceptance criteria:

- Built artifacts without basic provenance do not silently look complete.
- Common upstream release ingestion remains low-friction.
- Validation behavior is documented and test-covered.

Dependencies:

- Issue 2

## Phase 2

### Issue 4: Make OCI/ORAS the Primary Backend and Default Workflow

Problem:
OCI support already exists, but `github-releases` still acts like the default
architecture.

Scope:

- backend defaults
- docs and config
- CLI messaging

Tasks:

- Change default backend selection to OCI unless explicitly overridden.
- Update sample config and README examples to use OCI-first settings.
- Update help text and release docs to describe GitHub as fallback.
- Normalize wording so the product is described as publishing to OCI first, with
  optional GitHub fallback.

Acceptance criteria:

- Default backend resolves to OCI unless overridden.
- Sample config defaults to OCI repository settings.
- Docs and diagnostics no longer frame GitHub Releases as the normal path.

Dependencies:

- Issue 0

### Issue 5: Normalize the OCI Object Model and ORAS Publish/Sync Behavior

Problem:
The current OCI layout works, but it looks implementation-driven rather than
product-defined.

Scope:

- OCI tag and reference conventions
- media types
- ORAS publish and sync semantics

Tasks:

- Define canonical OCI references for:
  - manifest object
  - checksums object
  - artifact payload objects
- Decide whether per-artifact object naming continues to use the current
  transport name or a backend-neutral replacement.
- Document expected ORAS commands and media types.
- Confirm behavior for binary files, text assets, and archive-backed artifacts.
- Add non-network tests or fixtures around the chosen object model where
  feasible.

Acceptance criteria:

- OCI reference naming is documented and stable.
- Publish and sync logic matches the documented object model.
- ORAS interactions are predictable across compliant registries.

Dependencies:

- Issue 1
- Issue 4

### Issue 6: Treat Amazon ECR as a Supported Managed OCI Backend

Problem:
ECR is a practical managed backend for this use case, but it should be an
operational choice, not the architectural center of the product.

Scope:

- ECR-specific documentation
- auth expectations
- validation of current OCI assumptions

Tasks:

- Document repository setup expectations for ECR.
- Document auth flows such as `oras login` and AWS credential-based usage.
- Verify naming and media types are acceptable for ECR.
- Add ECR-specific examples where useful in config and release docs.

Acceptance criteria:

- An operator can configure the tool against ECR without guessing repository
  shape.
- No shared schema or naming decisions remain GitHub-centric because of ECR.
- ECR is clearly documented as a supported managed OCI backend.

Dependencies:

- Issue 5

### Issue 7: Demote GitHub Releases to a Fallback Backend

Problem:
GitHub Releases is useful, but it should stop shaping shared schema names,
defaults, and architecture claims.

Scope:

- fallback backend behavior
- naming cleanup
- documentation demotion

Tasks:

- Keep GitHub publish and sync operational.
- Mark GitHub backend as fallback in docs and help output.
- Remove GitHub-specific wording from shared schema and transport naming.
- Add regression coverage for fallback behavior.

Acceptance criteria:

- GitHub publish and sync still work.
- Shared schema and transport naming are backend-neutral.
- Docs consistently describe GitHub Releases as fallback.

Dependencies:

- Issue 4
- Issue 5

## Phase 3

### Issue 8: Unify Local Artifact State Semantics Across CLI and TUI

Problem:
`list`, `show`, `doctor`, and the TUI derive local state using different rules.
The same artifact can appear healthy in one surface and stale in another.

Scope:

- shared local-state model
- CLI and TUI consistency
- machine-readable status output

Tasks:

- Define a canonical local-state model with at least:
  - `present`
  - `verified`
  - `stale`
  - machine-readable reason or code
- Decide how metadata, expected destination, actual file presence, version
  drift, path drift, and digest drift map into that model.
- Replace duplicated local-state logic in CLI and TUI paths with one helper.
- Keep staged catalog state separate from local payload state.

Acceptance criteria:

- The same artifact resolves to the same local-state result in `list`, `show`,
  `doctor`, and TUI.
- No command infers healthy local state from metadata presence alone.
- JSON output exposes explicit local-state fields.

Dependencies:

- Issue 2

### Issue 9: Make Local Integrity Verification Content-Based

Problem:
A synced file can be modified after download and still appear healthy because
current logic trusts metadata and file existence more than actual content.

Scope:

- local hash verification
- verified versus present semantics
- stale-state rules

Tasks:

- Compute local SHA256 when evaluating verified state.
- Distinguish `present` from `verified`.
- Mark artifacts stale when local path, version, or digest diverges from the
  manifest.
- Decide how to treat files at expected destinations without metadata.
- Treat local metadata as sync history, not proof of current integrity.

Acceptance criteria:

- A locally modified file reports `present=true`, `verified=false`,
  `stale=true`.
- A missing file with lingering metadata is stale, not synced.
- A file at the expected destination without metadata is surfaced
  deterministically.

Dependencies:

- Issue 8

### Issue 10: Separate Catalog Verification from Local Payload Verification

Problem:
`verify` currently means catalog verification, while local verification is
spread inconsistently across other commands.

Scope:

- command semantics
- verification boundaries
- clearer error reporting

Tasks:

- Decide on the user-facing model, for example:
  - `verify --catalog`
  - `verify --local`
  - `verify --all`
  - or a distinct local status or inspect command
- Keep staged release-asset checks in a clearly named catalog verification
  path.
- Add local verification output with counts for verified, present-but-unverified,
  stale, and missing artifacts.
- Update `doctor` to consume the shared local verification result.

Acceptance criteria:

- Users can explicitly verify local payload integrity.
- `verify` and `doctor` clearly distinguish catalog failures from local payload
  failures.
- Command naming does not imply stronger guarantees than the implementation
  provides.

Dependencies:

- Issue 8
- Issue 9

## Phase 4

### Issue 11: Add Focused Tests for Integrity and Local-State Transitions

Problem:
Current tests focus mostly on config and path discovery. The riskiest integrity
and local-state behavior is under-tested.

Scope:

- unit tests
- state classification coverage
- verification helper coverage

Tasks:

- Add tests for:
  - absent local file
  - present file without metadata
  - metadata without file
  - verified file
  - stale by digest
  - stale by path
  - stale by version
- Add tests proving catalog verification and local verification can fail
  independently.
- Add test fixtures and helpers for temp manifests, staged assets, payload
  trees, and metadata.

Acceptance criteria:

- Each local-state branch has direct test coverage.
- Tests prove a tampered local file is not reported as verified.
- Tests prove catalog verification can pass while local verification fails, and
  vice versa where appropriate.

Dependencies:

- Issue 10

### Issue 12: Add CLI-Facing Tests and Make Documentation Honest About Guarantees

Problem:
The docs currently overstate what local state means, and there is little
protection against regressing back to metadata-only semantics.

Scope:

- CLI-facing tests
- help text
- README accuracy

Tasks:

- Add tests for `list --json`, `show --json`, and `doctor` output fields once
  the new state model lands.
- Add tests for stable human-readable labels such as `present`, `verified`, and
  `stale`.
- Update README command examples to distinguish staged catalog verification from
  local payload verification.
- Document whether local verification hashes files eagerly, lazily, or only
  under explicit flags.
- Update help text to use `present`, `verified`, and `stale` consistently.

Acceptance criteria:

- README and `--help` text match actual command behavior.
- CLI tests fail if output regresses back to metadata-only synced semantics.
- Documentation no longer implies that local presence alone equals verified
  integrity.

Dependencies:

- Issue 10
- Issue 11

## Suggested Execution Order

1. Issue 0
2. Issue 1
3. Issue 2
4. Issue 3
5. Issue 4
6. Issue 5
7. Issue 6
8. Issue 7
9. Issue 8
10. Issue 9
11. Issue 10
12. Issue 11
13. Issue 12

## Suggested Parallelization Later

Once the repo is modular enough to avoid heavy merge conflict risk:

- Track A: schema and provenance
  - Issue 1
  - Issue 2
  - Issue 3
- Track B: backend and docs
  - Issue 4
  - Issue 5
  - Issue 6
  - Issue 7
- Track C: integrity and tests
  - Issue 8
  - Issue 9
  - Issue 10
  - Issue 11
  - Issue 12
