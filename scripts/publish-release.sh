#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  publish-release.sh <tag> [--title TITLE] [--notes-file PATH]

Environment:
  GITHUB_TOKEN   Required token with repo contents access
  GITHUB_OWNER   Default: CameronCandau
  GITHUB_REPO    Default: Artifact-Catalog
EOF
}

fail() {
  printf '[!] %s\n' "$*" >&2
  exit 1
}

api_call() {
  local method="$1"
  local url="$2"
  local output_path="$3"
  shift 3

  local status
  status="$(curl -sS -o "$output_path" -w '%{http_code}' \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer $GITHUB_TOKEN" \
    -X "$method" \
    "$@" \
    "$url")"

  if [[ "$status" =~ ^2 ]]; then
    printf '%s\n' "$status"
    return 0
  fi

  printf '[!] GitHub API %s %s failed with HTTP %s\n' "$method" "$url" "$status" >&2
  if [ -s "$output_path" ]; then
    cat "$output_path" >&2
    printf '\n' >&2
  fi
  return 1
}

have() {
  command -v "$1" >/dev/null 2>&1
}

[ "$#" -ge 1 ] || {
  usage >&2
  exit 1
}

tag="$1"
shift
title="$tag"
notes_file=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --title)
      title="$2"
      shift 2
      ;;
    --notes-file)
      notes_file="$2"
      shift 2
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

have curl || fail "curl is required"
[ -n "${GITHUB_TOKEN:-}" ] || fail "GITHUB_TOKEN is required"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
owner="${GITHUB_OWNER:-CameronCandau}"
repo="${GITHUB_REPO:-Artifact-Catalog}"
api="https://api.github.com/repos/$owner/$repo"
release_dir="$repo_root/staging/release-assets"
manifest_path="$repo_root/manifests/artifacts.yaml"
checksums_path="$repo_root/checksums/sha256sums.txt"

[ -d "$release_dir" ] || fail "Release asset directory missing: $release_dir"
[ -f "$manifest_path" ] || fail "Manifest missing: $manifest_path"
[ -f "$checksums_path" ] || fail "Checksums missing: $checksums_path"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

notes_text=""
if [ -n "$notes_file" ]; then
  [ -f "$notes_file" ] || fail "Notes file not found: $notes_file"
  notes_text="$(cat "$notes_file")"
fi

release_json="$tmpdir/release.json"
status="$(curl -sS -o "$release_json" -w '%{http_code}' \
  -H "Accept: application/vnd.github+json" \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  "$api/releases/tags/$tag")"

if [ "$status" = "404" ]; then
  create_json="$tmpdir/create.json"
  python3 - "$tag" "$title" "$notes_text" >"$create_json" <<'PY'
import json
import sys
payload = {
    "tag_name": sys.argv[1],
    "name": sys.argv[2],
    "body": sys.argv[3],
    "draft": False,
    "prerelease": False,
}
print(json.dumps(payload))
PY
  api_call POST "$api/releases" "$release_json" --data @"$create_json" >/dev/null || {
    cat <<'EOF' >&2
[!] Common causes for HTTP 422 here:
[!] - the repository has no commits/default branch yet
[!] - the tag or release name is invalid
[!] - the token does not have Contents: read/write on this repo
EOF
    exit 1
  }
elif [ "$status" != "200" ]; then
  fail "Could not resolve release for tag $tag (HTTP $status)"
fi

upload_url="$(python3 - "$release_json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
print(data["upload_url"].split("{", 1)[0])
PY
)"
release_id="$(python3 - "$release_json" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
print(data["id"])
PY
)"

assets_dir="$tmpdir/assets"
mkdir -p "$assets_dir"
cp "$manifest_path" "$assets_dir/artifacts.yaml"
cp "$checksums_path" "$assets_dir/sha256sums.txt"
find "$release_dir" -maxdepth 1 -type f -exec cp {} "$assets_dir/" \;

assets_json="$tmpdir/assets.json"
api_call GET "$api/releases/$release_id/assets" "$assets_json" >/dev/null

for asset_path in "$assets_dir"/*; do
  [ -f "$asset_path" ] || continue
  asset_name="$(basename "$asset_path")"
  existing_id="$(python3 - "$assets_json" "$asset_name" <<'PY'
import json
import sys
assets = json.load(open(sys.argv[1]))
target = sys.argv[2]
for asset in assets:
    if asset["name"] == target:
        print(asset["id"])
        break
PY
)"

  if [ -n "$existing_id" ]; then
    api_call DELETE "$api/releases/assets/$existing_id" "$tmpdir/delete-$existing_id.json" >/dev/null || exit 1
  fi

  upload_response="$tmpdir/upload-$asset_name.json"
  status="$(curl -sS -o "$upload_response" -w '%{http_code}' \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer $GITHUB_TOKEN" \
    -H "Content-Type: application/octet-stream" \
    --data-binary @"$asset_path" \
    "$upload_url?name=$asset_name")"
  if ! [[ "$status" =~ ^2 ]]; then
    printf '[!] Upload failed for %s with HTTP %s\n' "$asset_name" "$status" >&2
    if [ -s "$upload_response" ]; then
      cat "$upload_response" >&2
      printf '\n' >&2
    fi
    exit 1
  fi

  printf '[+] Uploaded %s\n' "$asset_name"
done
