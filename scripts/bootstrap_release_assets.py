#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from pathlib import Path


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download(url: str, destination: Path) -> None:
    request = urllib.request.Request(
        url,
        headers={
            "User-Agent": "artifact-catalog-bootstrap/1.0",
        },
    )
    with urllib.request.urlopen(request) as response, destination.open("wb") as handle:
        shutil.copyfileobj(response, handle)


def copy_if_checksum_matches(candidate: Path, destination: Path, expected_sha: str) -> bool:
    if sha256_file(candidate) != expected_sha:
        return False
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(candidate, destination)
    return True


def extract_from_zip(archive: Path, wanted_name: str, destination: Path, expected_sha: str) -> bool:
    with zipfile.ZipFile(archive) as zf:
        matches = [name for name in zf.namelist() if Path(name).name == wanted_name]
        for match in matches:
            with zf.open(match) as src, tempfile.NamedTemporaryFile(delete=False) as tmp:
                shutil.copyfileobj(src, tmp)
                tmp_path = Path(tmp.name)
            try:
                if copy_if_checksum_matches(tmp_path, destination, expected_sha):
                    return True
            finally:
                tmp_path.unlink(missing_ok=True)
    return False


def extract_from_tar(archive: Path, wanted_name: str, destination: Path, expected_sha: str) -> bool:
    with tarfile.open(archive) as tf:
        matches = [member for member in tf.getmembers() if member.isfile() and Path(member.name).name == wanted_name]
        for match in matches:
            extracted = tf.extractfile(match)
            if extracted is None:
                continue
            with extracted, tempfile.NamedTemporaryFile(delete=False) as tmp:
                shutil.copyfileobj(extracted, tmp)
                tmp_path = Path(tmp.name)
            try:
                if copy_if_checksum_matches(tmp_path, destination, expected_sha):
                    return True
            finally:
                tmp_path.unlink(missing_ok=True)
    return False


def restore_artifact(artifact: dict[str, object], release_dir: Path) -> tuple[bool, str]:
    filename = str(artifact["filename"])
    source_ref = str(artifact["source_ref"])
    release_asset_name = str(artifact["release_asset_name"])
    expected_sha = str(artifact["sha256"])
    destination = release_dir / release_asset_name

    if not source_ref.startswith(("http://", "https://")):
        return False, f"{filename}: unsupported source_ref for bootstrap: {source_ref}"

    with tempfile.TemporaryDirectory(prefix="artifact-bootstrap-") as tmpdir:
        downloaded = Path(tmpdir) / "downloaded"
        try:
            download(source_ref, downloaded)
        except Exception as exc:  # noqa: BLE001
            return False, f"{filename}: download failed from {source_ref}: {exc}"

        if copy_if_checksum_matches(downloaded, destination, expected_sha):
            return True, f"{filename}: restored from direct download"

        if zipfile.is_zipfile(downloaded):
            if extract_from_zip(downloaded, filename, destination, expected_sha):
                return True, f"{filename}: restored by extracting matching file from zip source"

        if tarfile.is_tarfile(downloaded):
            if extract_from_tar(downloaded, filename, destination, expected_sha):
                return True, f"{filename}: restored by extracting matching file from tar source"

        actual_sha = sha256_file(downloaded)
        return (
            False,
            f"{filename}: checksum mismatch after download ({actual_sha}) and no matching archive member verified",
        )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Rebuild staged release assets from manifest source_ref entries."
    )
    parser.add_argument("--manifest", default="manifests/artifacts.yaml")
    parser.add_argument("--release-dir", default="staging/release-assets")
    parser.add_argument("--active-only", action="store_true", default=True)
    args = parser.parse_args()

    manifest_path = Path(args.manifest)
    release_dir = Path(args.release_dir)
    release_dir.mkdir(parents=True, exist_ok=True)

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    failures: list[str] = []

    for artifact in manifest.get("artifacts", []):
        if args.active_only and not artifact.get("active", False):
            continue
        ok, message = restore_artifact(artifact, release_dir)
        prefix = "[+]" if ok else "[!]"
        print(f"{prefix} {message}")
        if not ok:
            failures.append(message)

    if failures:
        print("\nBootstrap failed for one or more artifacts:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
