#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  add-artifact.sh <source_path_or_url> --platform PLATFORM --category CATEGORY \
    --filename FILENAME --version VERSION [--source-type TYPE] [--source-ref REF] [--inactive]

Examples:
  add-artifact.sh /path/to/winpeas.exe \
    --platform windows \
    --category bin \
    --filename winpeas.exe \
    --version 2025.01 \
    --source-type local

  add-artifact.sh https://github.com/OWNER/REPO/releases/download/v1.2.3/tool.exe \
    --platform windows \
    --category bin \
    --filename tool.exe \
    --version v1.2.3 \
    --source-type github-release
EOF
}

fail() {
  printf '[!] %s\n' "$*" >&2
  exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest_path="$repo_root/manifests/artifacts.yaml"
checksums_path="$repo_root/checksums/sha256sums.txt"
release_dir="$repo_root/staging/release-assets"
tmpdir=""

[ "$#" -ge 1 ] || {
  usage >&2
  exit 1
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

source_path="$1"
shift

platform=""
category=""
filename=""
version=""
source_type="local"
source_ref=""
active="true"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --platform)
      platform="$2"
      shift 2
      ;;
    --category)
      category="$2"
      shift 2
      ;;
    --filename)
      filename="$2"
      shift 2
      ;;
    --version)
      version="$2"
      shift 2
      ;;
    --source-type)
      source_type="$2"
      shift 2
      ;;
    --source-ref)
      source_ref="$2"
      shift 2
      ;;
    --inactive)
      active="false"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      fail "Unknown argument: $1"
      ;;
  esac
done

[ -n "$platform" ] || fail "--platform is required"
[ -n "$category" ] || fail "--category is required"
[ -n "$filename" ] || fail "--filename is required"
[ -n "$version" ] || fail "--version is required"

case "$platform" in
  linux|windows) ;;
  *)
    fail "Unsupported platform: $platform"
    ;;
esac

if [[ "$source_path" =~ ^https?:// ]]; then
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT
  download_path="$tmpdir/source"
  printf '[+] Downloading %s\n' "$source_path"
  curl -fsSL --retry 2 --retry-delay 2 "$source_path" -o "$download_path"
  source_file="$download_path"
  if [ -z "$source_ref" ]; then
    source_ref="$source_path"
  fi
else
  [ -f "$source_path" ] || fail "Source file not found: $source_path"
  source_file="$source_path"
  if [ -z "$source_ref" ]; then
    source_ref="$(realpath "$source_path")"
  fi
fi

mkdir -p "$release_dir"

release_asset_name="${platform}--${category}--${filename}"
dest_path="$release_dir/$release_asset_name"
cp "$source_file" "$dest_path"
sha256="$(sha256sum "$dest_path" | awk '{print $1}')"

python3 - "$manifest_path" "$platform" "$category" "$filename" "$version" "$source_type" "$source_ref" "$sha256" "$release_asset_name" "$active" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
entry = {
    "platform": sys.argv[2],
    "category": sys.argv[3],
    "filename": sys.argv[4],
    "version": sys.argv[5],
    "source_type": sys.argv[6],
    "source_ref": sys.argv[7],
    "sha256": sys.argv[8],
    "release_asset_name": sys.argv[9],
    "active": sys.argv[10].lower() == "true",
}

if manifest_path.exists():
    data = json.loads(manifest_path.read_text())
else:
    data = {"artifacts": []}

artifacts = data.setdefault("artifacts", [])
updated = False
for index, existing in enumerate(artifacts):
    if existing.get("release_asset_name") == entry["release_asset_name"]:
        artifacts[index] = entry
        updated = True
        break

if not updated:
    artifacts.append(entry)

artifacts.sort(key=lambda item: (item["platform"], item["category"], item["filename"]))
manifest_path.write_text(json.dumps(data, indent=2) + "\n")
PY

python3 - "$manifest_path" "$checksums_path" "$release_dir" <<'PY'
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
checksums_path = pathlib.Path(sys.argv[2])
release_dir = pathlib.Path(sys.argv[3])
data = json.loads(manifest_path.read_text())
lines = []
for artifact in data.get("artifacts", []):
    asset_path = release_dir / artifact["release_asset_name"]
    if asset_path.is_file():
      lines.append(f'{artifact["sha256"]}  {artifact["release_asset_name"]}')
checksums_path.write_text("\n".join(lines) + ("\n" if lines else ""))
PY

printf '[+] Added %s -> %s\n' "$source_path" "$release_asset_name"
printf '[+] Staged at %s\n' "$dest_path"
printf '[+] Updated %s\n' "$manifest_path"
printf '[+] Updated %s\n' "$checksums_path"
