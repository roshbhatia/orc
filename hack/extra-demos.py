#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
import hashlib
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]

def fingerprint(directory):
    metadata = json.loads((directory / "demo.json").read_text())
    visible = {key: metadata[key] for key in ("name", "summary", "replay", "core", "binary")}
    digest = hashlib.sha256(json.dumps(visible, sort_keys=True).encode())
    sources = {directory / "demo.tape", directory / "demo.sh", ROOT / "hack/extra-demos.py"}
    for name in ("go.mod", "go.sum", "Cargo.toml", "Cargo.lock", "flake.lock", "extras/demo.py"):
        path = ROOT / name
        if path.is_file():
            sources.add(path)
    roots = [directory, ROOT / "cmd", ROOT / "internal", ROOT / "src", ROOT / "extras/internal", ROOT / "extras/lib"]
    for root in roots:
        if not root.exists():
            continue
        for path in root.rglob("*"):
            if any(part in {"node_modules", "__pycache__", ".git", "target", "dist"} for part in path.parts):
                continue
            if path.is_file() and path.suffix in {".go", ".rs", ".py", ".sh", ".lua", ".json", ".yaml", ".nix"} and path.name != "demo.json":
                sources.add(path)
    sources.update(ROOT.glob("*.go"))
    for source in sorted(sources):
        digest.update(str(source.relative_to(ROOT)).encode() + b"\0")
        digest.update(source.read_bytes())
    return digest.hexdigest() + "\n"

check = "--check" in sys.argv
selected = [arg for arg in sys.argv[1:] if not arg.startswith("--")]
for directory in sorted((ROOT / "extras").iterdir()):
    if not (directory / "demo.json").is_file() or selected and directory.name not in selected:
        continue
    stamp = directory / ".demo.sha256"
    metadata = json.loads((directory / "demo.json").read_text())
    if metadata.get("status") == "pending":
        if not check and selected:
            raise SystemExit("live recording pending: " + directory.name)
        continue
    if check:
        if not (directory / "demo.gif").is_file() or (directory / "demo.gif").stat().st_size == 0:
            raise SystemExit("missing demo: " + directory.name)
        if not stamp.exists() or stamp.read_text() != fingerprint(directory):
            raise SystemExit("stale demo: " + directory.name)
    else:
        subprocess.run(["vhs", str(directory.relative_to(ROOT) / "demo.tape")], cwd=ROOT, check=True)
        subprocess.run(["ffprobe", "-v", "error", str(directory / "demo.gif")], check=True)
        stamp.write_text(fingerprint(directory))
