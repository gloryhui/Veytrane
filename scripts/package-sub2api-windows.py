#!/usr/bin/env python3
"""Package only the fixed Windows executable and its recipient-facing files."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import zipfile


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    raw = args.binary.read_bytes()
    if len(raw) < 64 or raw[:2] != b"MZ":
        raise SystemExit("Expected a Windows PE executable")
    offset = struct.unpack_from("<I", raw, 0x3C)[0]
    if raw[offset:offset + 4] != b"PE\0\0" or struct.unpack_from("<H", raw, offset + 4)[0] != 0x8664:
        raise SystemExit("Expected a Windows x64 executable")
    if args.output.exists():
        raise SystemExit("Output already exists; choose a new directory")
    repo = Path(__file__).resolve().parents[1]
    package = args.output / "Sub2API-Windows-x64"
    package.mkdir(parents=True)
    (package / "Sub2API.exe").write_bytes(raw)
    templates = repo / "packaging/sub2api-windows"
    bat = (templates / "Start-Sub2API.bat").read_text().replace("\r\n", "\n")
    (package / "Start-Sub2API.bat").write_bytes(bat.replace("\n", "\r\n").encode("utf-8"))
    (package / "使用说明.txt").write_bytes(b"\xef\xbb\xbf" + (templates / "使用说明.txt").read_text().replace("\n", "\r\n").encode("utf-8"))
    (package / "LICENSE.txt").write_bytes((repo / "LICENSE").read_bytes())
    archive = args.output / "Sub2API-Windows-x64.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zipped:
        for path in sorted(package.iterdir()):
            zipped.write(path, f"{package.name}/{path.name}")
    def git(*arguments):
        return subprocess.check_output(["git", *arguments], cwd=repo, text=True).strip()
    sources = [repo / "Cargo.toml", repo / "Cargo.lock", repo / "build.rs", *sorted((repo / "src").rglob("*.rs"))]
    manifest = {
        "parent_commit": git("rev-parse", "HEAD"),
        "working_tree": "dirty" if git("status", "--porcelain") else "clean",
        "architecture": "Windows x64", "authenticode": "unsigned",
        "local_url": "http://127.0.0.1:18081/",
        "identity": "generated independently on each device; none included",
        "files": {p.name: sha256(p) for p in sorted(package.iterdir())},
        "zip_sha256": sha256(archive),
        "sources_sha256": {str(p.relative_to(repo)): sha256(p) for p in sources},
    }
    (args.output / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2))
    (args.output / "Sub2API-Windows-x64.zip.sha256").write_text(f"{sha256(archive)}  {archive.name}\n")
    print(f"ZIP: {archive.resolve()}")
    print(f"SHA256: {sha256(archive)}")
    print(f"Size: {archive.stat().st_size / 1024 / 1024:.2f} MiB")


if __name__ == "__main__":
    main()
