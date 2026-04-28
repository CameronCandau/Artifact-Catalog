#!/usr/bin/env bash
set -euo pipefail

repo="CameronCandau/Artifact-Catalog"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

create_label() {
  local name="$1"
  local color="$2"
  local description="$3"
  if ! gh label create "$name" --repo "$repo" --color "$color" --description "$description" 2>/dev/null; then
    echo "label exists or could not be created: $name"
  else
    echo "created label: $name"
  fi
}

write_body() {
  local path="$1"
  shift
  cat >"$path" <<'EOF'
$BODY
EOF
}

create_issue() {
  local title="$1"
  local labels="$2"
  local body_file="$3"
  gh issue create --repo "$repo" --title "$title" --label "$labels" --body-file "$body_file"
}

create_label "epic" "5319e7" "Top-level roadmap issue"
create_label "backlog" "1d76db" "Planned work item"
create_label "phase-0" "bfdadc" "Product shape and positioning"
create_label "phase-1" "c2e0c6" "Schema and provenance"
create_label "phase-2" "fef2c0" "Backend and OCI transport"
create_label "phase-3" "ffd8b1" "Integrity and local state"
create_label "phase-4" "f9d0c4" "Tests and documentation"
create_label "schema" "0e8a16" "Manifest and schema work"
create_label "provenance" "006b75" "Provenance metadata and validation"
create_label "backend" "0052cc" "Backend behavior and defaults"
create_label "oci" "0366d6" "OCI-specific work"
create_label "oras" "1b7f83" "ORAS workflow and transport"
create_label "ecr" "8250df" "Amazon ECR support"
create_label "github-fallback" "6f42c1" "GitHub Releases fallback backend"
create_label "integrity" "b60205" "Integrity verification semantics"
create_label "local-state" "d93f0b" "Local payload state reporting"
create_label "docs" "0075ca" "Documentation work"
create_label "tests" "fbca04" "Test coverage"
create_label "cli" "c5def5" "CLI and user-facing behavior"

cat >"$tmpdir/epic.md" <<'EOF'
## Summary
Reshape `artifact-catalog` into a practical operator artifact catalog with OCI/ORAS as the primary transport model, ECR as a supported managed backend, and GitHub Releases as a fallback backend.

## Goals
- Present OCI/ORAS as the primary publish and sync workflow
- Keep ECR as a practical managed OCI backend
- Retain GitHub Releases as a fallback backend
- Introduce structured provenance for upstream and locally built artifacts
- Make local integrity and status reporting reflect verified reality

## Out of Scope
- SBOM support
- Signatures and attestations
- Reproducible build automation
- Large-scale mirroring

## Success Criteria
- README and CLI help describe the tool as an operator-first curated catalog
- OCI/ORAS is the documented primary workflow
- GitHub Releases is consistently described as fallback
- Provenance and integrity semantics are explicit and test-covered

## Follow-on Issues
The child issues created from `docs/ISSUE_BACKLOG.md` should be linked back here.
EOF

epic_url="$(create_issue \
  "Epic: Reposition artifact-catalog as an operator-first artifact catalog" \
  "epic,backlog" \
  "$tmpdir/epic.md")"
echo "epic: $epic_url"

cat >"$tmpdir/issue-0.md" <<EOF
Parent epic: $epic_url

## Problem
Current docs frame the tool around both GitHub Releases and OCI, which makes the operating model ambiguous.

## Scope
- product positioning
- supported workflows
- backend priority and terminology
- catalog curation boundaries

## Tasks
- Update README language from generic artifact locker language to curated operator artifact catalog language.
- Add a short architecture section covering local manifest as source of truth, staged artifacts as publish inputs, ORAS as the primary transport interface, OCI registry as the primary distribution channel, ECR as a managed backend option, and GitHub Releases as fallback.
- Document supported artifact classes: upstream release assets, locally built binaries, scripts and text assets, and compatibility-specific variants.
- Document non-goals for the current roadmap.

## Acceptance Criteria
- A contributor can identify the primary transport, primary backend class, and fallback path within one minute of reading the README.
- README, config examples, and release docs consistently say OCI primary and GitHub fallback.
- No docs imply GitHub Releases is the preferred long-term design.
EOF

cat >"$tmpdir/issue-1.md" <<EOF
Parent epic: $epic_url

## Problem
\`source_type\` and \`source_ref\` are too lossy for practical provenance and too weak for later audit or refresh workflows.

## Scope
- schema design
- field definitions
- validation rules
- example coverage

## Tasks
- Design a structured \`provenance\` object.
- Define provenance kinds for upstream release or direct download, locally built artifact from pinned source, and local/manual ingestion.
- Define fields such as \`kind\`, \`uri\`, \`repo\`, \`tag\` or \`version_ref\`, \`commit\`, \`asset_name\`, \`archive_path\`, \`build_method\`, and \`notes\`.
- Replace \`source_type\` and \`source_ref\` with the new structured schema.
- Rename \`release_asset_name\` to a backend-neutral field such as \`object_name\`.
- Add example manifests for release binaries, archive-backed assets, locally built pinned-commit artifacts, and manual local ingestion.

## Acceptance Criteria
- The schema can represent both mirrored upstream artifacts and locally built artifacts without overloading one flat string field.
- Required fields are clear for each provenance kind.
- Example manifests and CLI usage match the new schema exactly.

## Dependencies
- Issue 0
EOF

cat >"$tmpdir/issue-2.md" <<EOF
Parent epic: $epic_url

## Problem
Without a complete CLI and manifest cutover, the new provenance model will leave the repo half-converted and error-prone.

## Scope
- manifest structs
- serialization and deserialization
- schema cutover
- CLI add/show/list support

## Tasks
- Update manifest structs to support the new provenance model.
- Update \`add\` to capture structured provenance cleanly.
- Update \`show\` and \`list\` output to render provenance readably.
- Update bootstrap tooling and local metadata as needed.

## Acceptance Criteria
- Existing repo examples and tests use only the new schema.
- New manifests round-trip cleanly.
- \`locker add\` can create structured entries for both release-backed and built artifacts.
- Bootstrap tooling still works with the migrated schema.

## Dependencies
- Issue 1
EOF

cat >"$tmpdir/issue-3.md" <<EOF
Parent epic: $epic_url

## Problem
The repo currently relies on README guidance for provenance quality, but the CLI does not enforce or even warn on weak metadata.

## Scope
- CLI validation
- warning policy
- operator ergonomics

## Tasks
- Warn on weak version values such as \`manual\`.
- Warn on missing pinned commit metadata for built artifacts.
- Warn on unpinned repository references where a pinned commit is expected.
- Hard-fail only on clearly invalid cases, such as missing required fields for a provenance kind.
- Keep the workflow usable for common upstream prebuilt downloads.

## Acceptance Criteria
- Built artifacts without basic provenance do not silently look complete.
- Common upstream release ingestion remains low-friction.
- Validation behavior is documented and test-covered.

## Dependencies
- Issue 2
EOF

cat >"$tmpdir/issue-4.md" <<EOF
Parent epic: $epic_url

## Problem
OCI support already exists, but \`github-releases\` still acts like the default architecture.

## Scope
- backend defaults
- docs and config
- CLI messaging

## Tasks
- Change default backend selection to OCI unless explicitly overridden.
- Update sample config and README examples to use OCI-first settings.
- Update help text and release docs to describe GitHub as fallback.
- Normalize wording so the product is described as publishing to OCI first, with optional GitHub fallback.

## Acceptance Criteria
- Default backend resolves to OCI unless overridden.
- Sample config defaults to OCI repository settings.
- Docs and diagnostics no longer frame GitHub Releases as the normal path.

## Dependencies
- Issue 0
EOF

cat >"$tmpdir/issue-5.md" <<EOF
Parent epic: $epic_url

## Problem
The current OCI layout works, but it looks implementation-driven rather than product-defined.

## Scope
- OCI tag and reference conventions
- media types
- ORAS publish and sync semantics

## Tasks
- Define canonical OCI references for manifest object, checksums object, and artifact payload objects.
- Decide whether per-artifact object naming continues to use the current transport name or a backend-neutral replacement.
- Document expected ORAS commands and media types.
- Confirm behavior for binary files, text assets, and archive-backed artifacts.
- Add non-network tests or fixtures around the chosen object model where feasible.

## Acceptance Criteria
- OCI reference naming is documented and stable.
- Publish and sync logic matches the documented object model.
- ORAS interactions are predictable across compliant registries.

## Dependencies
- Issue 1
- Issue 4
EOF

cat >"$tmpdir/issue-6.md" <<EOF
Parent epic: $epic_url

## Problem
ECR is a practical managed backend for this use case, but it should be an operational choice, not the architectural center of the product.

## Scope
- ECR-specific documentation
- auth expectations
- validation of current OCI assumptions

## Tasks
- Document repository setup expectations for ECR.
- Document auth flows such as \`oras login\` and AWS credential-based usage.
- Verify naming and media types are acceptable for ECR.
- Add ECR-specific examples where useful in config and release docs.

## Acceptance Criteria
- An operator can configure the tool against ECR without guessing repository shape.
- No shared schema or naming decisions remain GitHub-centric because of ECR.
- ECR is clearly documented as a supported managed OCI backend.

## Dependencies
- Issue 5
EOF

cat >"$tmpdir/issue-7.md" <<EOF
Parent epic: $epic_url

## Problem
GitHub Releases is useful, but it should stop shaping shared schema names, defaults, and architecture claims.

## Scope
- fallback backend behavior
- naming cleanup
- documentation demotion

## Tasks
- Keep GitHub publish and sync operational.
- Mark GitHub backend as fallback in docs and help output.
- Remove GitHub-specific wording from shared schema and transport naming.
- Add regression coverage for fallback behavior.

## Acceptance Criteria
- GitHub publish and sync still work.
- Shared schema and transport naming are backend-neutral.
- Docs consistently describe GitHub Releases as fallback.

## Dependencies
- Issue 4
- Issue 5
EOF

cat >"$tmpdir/issue-8.md" <<EOF
Parent epic: $epic_url

## Problem
\`list\`, \`show\`, \`doctor\`, and the TUI derive local state using different rules. The same artifact can appear healthy in one surface and stale in another.

## Scope
- shared local-state model
- CLI and TUI consistency
- machine-readable status output

## Tasks
- Define a canonical local-state model with at least \`present\`, \`verified\`, \`stale\`, and a machine-readable reason or code.
- Decide how metadata, expected destination, actual file presence, version drift, path drift, and digest drift map into that model.
- Replace duplicated local-state logic in CLI and TUI paths with one helper.
- Keep staged catalog state separate from local payload state.

## Acceptance Criteria
- The same artifact resolves to the same local-state result in \`list\`, \`show\`, \`doctor\`, and TUI.
- No command infers healthy local state from metadata presence alone.
- JSON output exposes explicit local-state fields.

## Dependencies
- Issue 2
EOF

cat >"$tmpdir/issue-9.md" <<EOF
Parent epic: $epic_url

## Problem
A synced file can be modified after download and still appear healthy because current logic trusts metadata and file existence more than actual content.

## Scope
- local hash verification
- verified versus present semantics
- stale-state rules

## Tasks
- Compute local SHA256 when evaluating verified state.
- Distinguish \`present\` from \`verified\`.
- Mark artifacts stale when local path, version, or digest diverges from the manifest.
- Decide how to treat files at expected destinations without metadata.
- Treat local metadata as sync history, not proof of current integrity.

## Acceptance Criteria
- A locally modified file reports \`present=true\`, \`verified=false\`, and \`stale=true\`.
- A missing file with lingering metadata is stale, not synced.
- A file at the expected destination without metadata is surfaced deterministically.

## Dependencies
- Issue 8
EOF

cat >"$tmpdir/issue-10.md" <<EOF
Parent epic: $epic_url

## Problem
\`verify\` currently means catalog verification, while local verification is spread inconsistently across other commands.

## Scope
- command semantics
- verification boundaries
- clearer error reporting

## Tasks
- Decide on the user-facing model, for example \`verify --catalog\`, \`verify --local\`, \`verify --all\`, or a distinct local status or inspect command.
- Keep staged release-asset checks in a clearly named catalog verification path.
- Add local verification output with counts for verified, present-but-unverified, stale, and missing artifacts.
- Update \`doctor\` to consume the shared local verification result.

## Acceptance Criteria
- Users can explicitly verify local payload integrity.
- \`verify\` and \`doctor\` clearly distinguish catalog failures from local payload failures.
- Command naming does not imply stronger guarantees than the implementation provides.

## Dependencies
- Issue 8
- Issue 9
EOF

cat >"$tmpdir/issue-11.md" <<EOF
Parent epic: $epic_url

## Problem
Current tests focus mostly on config and path discovery. The riskiest integrity and local-state behavior is under-tested.

## Scope
- unit tests
- state classification coverage
- verification helper coverage

## Tasks
- Add tests for absent local file, present file without metadata, metadata without file, verified file, stale by digest, stale by path, and stale by version.
- Add tests proving catalog verification and local verification can fail independently.
- Add test fixtures and helpers for temp manifests, staged assets, payload trees, and metadata.

## Acceptance Criteria
- Each local-state branch has direct test coverage.
- Tests prove a tampered local file is not reported as verified.
- Tests prove catalog verification can pass while local verification fails, and vice versa where appropriate.

## Dependencies
- Issue 10
EOF

cat >"$tmpdir/issue-12.md" <<EOF
Parent epic: $epic_url

## Problem
The docs currently overstate what local state means, and there is little protection against regressing back to metadata-only semantics.

## Scope
- CLI-facing tests
- help text
- README accuracy

## Tasks
- Add tests for \`list --json\`, \`show --json\`, and \`doctor\` output fields once the new state model lands.
- Add tests for stable human-readable labels such as \`present\`, \`verified\`, and \`stale\`.
- Update README command examples to distinguish staged catalog verification from local payload verification.
- Document whether local verification hashes files eagerly, lazily, or only under explicit flags.
- Update help text to use \`present\`, \`verified\`, and \`stale\` consistently.

## Acceptance Criteria
- README and \`--help\` text match actual command behavior.
- CLI tests fail if output regresses back to metadata-only synced semantics.
- Documentation no longer implies that local presence alone equals verified integrity.

## Dependencies
- Issue 10
- Issue 11
EOF

declare -a urls
urls+=("$(create_issue "Issue 0: Define and document the target product shape" "backlog,phase-0,docs" "$tmpdir/issue-0.md")")
urls+=("$(create_issue "Issue 1: Design the structured provenance schema" "backlog,phase-1,schema,provenance" "$tmpdir/issue-1.md")")
urls+=("$(create_issue "Issue 2: Implement manifest migration and CLI schema support" "backlog,phase-1,schema,provenance,cli" "$tmpdir/issue-2.md")")
urls+=("$(create_issue "Issue 3: Add provenance validation and operator-focused warnings" "backlog,phase-1,provenance,cli" "$tmpdir/issue-3.md")")
urls+=("$(create_issue "Issue 4: Make OCI/ORAS the primary backend and default workflow" "backlog,phase-2,backend,oci,oras,docs" "$tmpdir/issue-4.md")")
urls+=("$(create_issue "Issue 5: Normalize the OCI object model and ORAS publish/sync behavior" "backlog,phase-2,backend,oci,oras" "$tmpdir/issue-5.md")")
urls+=("$(create_issue "Issue 6: Treat Amazon ECR as a supported managed OCI backend" "backlog,phase-2,backend,oci,ecr,docs" "$tmpdir/issue-6.md")")
urls+=("$(create_issue "Issue 7: Demote GitHub Releases to a fallback backend" "backlog,phase-2,backend,github-fallback,docs" "$tmpdir/issue-7.md")")
urls+=("$(create_issue "Issue 8: Unify local artifact state semantics across CLI and TUI" "backlog,phase-3,integrity,local-state,cli" "$tmpdir/issue-8.md")")
urls+=("$(create_issue "Issue 9: Make local integrity verification content-based" "backlog,phase-3,integrity,local-state" "$tmpdir/issue-9.md")")
urls+=("$(create_issue "Issue 10: Separate catalog verification from local payload verification" "backlog,phase-3,integrity,cli" "$tmpdir/issue-10.md")")
urls+=("$(create_issue "Issue 11: Add focused tests for integrity and local-state transitions" "backlog,phase-4,tests,integrity,local-state" "$tmpdir/issue-11.md")")
urls+=("$(create_issue "Issue 12: Add CLI-facing tests and make documentation honest about guarantees" "backlog,phase-4,tests,docs,cli" "$tmpdir/issue-12.md")")

printf '%s\n' "${urls[@]}"
