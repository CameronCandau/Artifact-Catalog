#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  import-github-release.sh <browser_download_url> --name NAME --platform PLATFORM --category CATEGORY \
    --filename FILENAME --version VERSION [--source-type github-release]

Example:
  import-github-release.sh \
    https://github.com/OWNER/REPO/releases/download/v1.2.3/tool.exe \
    --name tool \
    --platform windows \
    --category bin \
    --filename tool.exe \
    --version v1.2.3
EOF
}

fail() {
  printf '[!] %s\n' "$*" >&2
  exit 1
}

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

url="$1"
shift

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

download_path="$tmpdir/asset"
printf '[+] Downloading %s\n' "$url"
curl -fsSL --retry 2 --retry-delay 2 "$url" -o "$download_path"

"$repo_root/scripts/add-artifact.sh" "$download_path" "$@" --source-type github-release --source-ref "$url"
