#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  resolve-url.sh <name> [platform]

Environment:
  ARTIFACT_CATALOG_BASE_URL  Override release base URL
EOF
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/manifests/artifacts.yaml"
base_url="${ARTIFACT_CATALOG_BASE_URL:-https://github.com/CameronCandau/Artifact-Catalog/releases/latest/download}"

[ "$#" -ge 1 ] || {
  usage >&2
  exit 1
}

name="$1"
platform="${2:-}"

python3 - "$manifest_path" "$base_url" "$name" "$platform" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
base_url = sys.argv[2].rstrip("/")
name = sys.argv[3]
platform = sys.argv[4]

data = json.loads(manifest_path.read_text())
for artifact in data.get("artifacts", []):
    if not artifact.get("active", True):
        continue
    if artifact["name"] != name:
        continue
    if platform and artifact["platform"] != platform:
        continue
    print(f'{base_url}/{artifact["release_asset_name"]}')
    raise SystemExit(0)

raise SystemExit(f"artifact not found: {name}")
PY
