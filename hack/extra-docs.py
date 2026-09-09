#!/usr/bin/env python3
import hashlib
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
CHECK = "--check" in sys.argv


def write(path, text):
    if CHECK:
        if not path.exists() or path.read_text() != text:
            raise SystemExit("stale generated file: " + str(path.relative_to(ROOT)))
    else:
        path.write_text(text)


def main():
    entries = []
    packages = []
    for directory in sorted((ROOT / "extras").iterdir()):
        if not directory.is_dir() or directory.name in {"internal", "lib", "manifests"}:
            continue
        if not any((directory / name).exists() for name in ("default.nix", "package.nix", "provider.yaml", "provider.json")):
            continue
        metadata = directory / "demo.json"
        data = json.loads(metadata.read_text())
        for name in ("demo.tape", "demo.sh"):
            if not (directory / name).is_file():
                raise SystemExit("missing demo source: " + str(directory / name))
        extra = data["name"]
        text = "# " + extra + "\n\n" + data["summary"] + ".\n\n"
        if data["replay"]:
            text += "The demo replays an offline response fixture. It does not contact a model or claim a new agent run.\n\n"
        text += "## Install\n\n```sh\n" + "brew install roshbhatia/tap/" + data["brew"] + "\nnix profile add '" + data["nix"] + "'\n```\n\n"
        text += "Install the core utility separately, or select its all-provider bundle. Runtime tools still need their own credentials.\n\n"
        if data.get("runtime_note"):
            text += data["runtime_note"] + "\n\n"
        text += "## Demo\n\n![" + data["summary"] + "](demo.gif)\n\n[Tape source](demo.tape) · [Task script](demo.sh)\n\n"
        text += "Run `nix develop -c bash extras/" + extra + "/demo.sh` to run the task without recording.\n"
        text += "Run `nix develop -c python3 hack/extra-demos.py " + extra + "` to record it.\n"
        write(directory / "README.md", text)
        entries.append(data)
        manifest = next((directory / name for name in ('provider.yaml', 'provider.json') if (directory / name).is_file()), None)
        kind = 'provider' if manifest else 'tool'
        shared = [data['manifest_path']] if manifest else []
        packages.append({
            'name': data['brew'], 'kind': kind, 'binary': data['binary'],
            'description': data['summary'],
            'archive': data['core'] + '_' + kind + '_' + extra + '_%{version}_%{os}_%{arch}.tar.gz',
            'share': shared,
            'python_script': (directory / 'provider.py').is_file(),
            'checksum': 'sidecar',
            **{key: data[key] for key in ('dependencies', 'npm_runtime', 'libexec_assets', 'script_environment', 'python_resources') if key in data},
        })
    write(ROOT / "package-index.json", json.dumps({"version": 1, "providers": entries, "packages": packages}, indent=2) + "\n")
    index = "| Extra | Task | Demo |\n|---|---|---|\n"
    for entry in entries:
        name = entry["name"]
        index += f"| [{name}]({name}/README.md) | {entry['summary']} | [Tape]({name}/demo.tape) |\n"
    path = ROOT / "extras/README.md"
    start = "<!-- BEGIN GENERATED CATALOG -->"
    end = "<!-- END GENERATED CATALOG -->"
    authored = path.read_text() if path.exists() else "# Extras\n"
    if authored.count(start) != authored.count(end) or authored.count(start) > 1:
        raise SystemExit("invalid extras catalog markers")
    block = start + "\n\n" + index + "\n" + end
    if start in authored:
        before, rest = authored.split(start)
        _, after = rest.split(end)
        authored = before + block + after
    elif CHECK:
        raise SystemExit("missing extras catalog markers")
    else:
        authored = authored.rstrip() + "\n\n" + block + "\n"
    write(path, authored)


if __name__ == "__main__":
    main()
