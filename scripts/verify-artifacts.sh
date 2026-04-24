#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/manifests/artifacts.yaml"
checksums_path="$repo_root/checksums/sha256sums.txt"
release_dir="$repo_root/staging/release-assets"

python3 - "$manifest_path" "$checksums_path" "$release_dir" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
checksums_path = pathlib.Path(sys.argv[2])
release_dir = pathlib.Path(sys.argv[3])

if not manifest_path.exists():
    raise SystemExit("manifest missing")

data = json.loads(manifest_path.read_text())
artifacts = data.get("artifacts", [])

expected_lines = []
for artifact in artifacts:
    asset_path = release_dir / artifact["release_asset_name"]
    if not asset_path.is_file():
        raise SystemExit(f'missing staged asset: {artifact["release_asset_name"]}')
    import hashlib
    digest = hashlib.sha256(asset_path.read_bytes()).hexdigest()
    if digest != artifact["sha256"]:
        raise SystemExit(f'sha mismatch for {artifact["release_asset_name"]}')
    expected_lines.append(f'{artifact["sha256"]}  {artifact["release_asset_name"]}')

actual_lines = []
if checksums_path.exists():
    actual_lines = [line.strip() for line in checksums_path.read_text().splitlines() if line.strip()]

expected_lines.sort()
actual_lines.sort()

if expected_lines != actual_lines:
    raise SystemExit("checksum file does not match manifest/release-assets")

print(f"[+] Verified {len(artifacts)} artifact(s)")
PY
