#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Build portable adapter archives from the generated package index."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tarfile
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--version', required=True)
parser.add_argument('--os', choices=['darwin', 'linux'])
parser.add_argument('--arch', choices=['amd64', 'arm64'])
parser.add_argument('--output', default='dist')
parser.add_argument('--all', action='store_true')
parser.add_argument('--include-darwin-amd64', action='store_true')
args = parser.parse_args()
if args.all:
    targets = [('darwin', 'arm64'), ('linux', 'amd64'), ('linux', 'arm64')]
    if args.include_darwin_amd64:
        targets.append(('darwin', 'amd64'))
    for operating_system, architecture in targets:
        subprocess.run([sys.executable, __file__, '--version', args.version, '--os', operating_system, '--arch', architecture, '--output', args.output], check=True)
    raise SystemExit(0)
if not args.os or not args.arch:
    parser.error('supply --all or both --os and --arch')
output = Path(args.output)
output.mkdir(parents=True, exist_ok=True)
index = json.loads((ROOT / 'package-index.json').read_text())
for entry, metadata in zip(index['packages'], index['providers'], strict=True):
    directory = ROOT / 'extras' / metadata['name']
    files = {}
    executable = directory / 'provider.py'
    if executable.is_file():
        content = executable.read_bytes()
        if not content.startswith(b'#!/usr/bin/env -S uv run --script\n'):
            raise SystemExit('Python adapters must declare their uv script runtime: ' + str(executable))
        files[entry['binary']] = (content, 0o755)
    else:
        files[entry['binary']] = ((directory / 'provider.sh').read_bytes(), 0o755)
        files['lib/provider.sh'] = ((ROOT / 'extras/lib/provider.sh').read_bytes(), 0o644)
        if (directory / 'process_tree.py').is_file():
            files['lib/process_tree.py'] = ((directory / 'process_tree.py').read_bytes(), 0o644)
    files['README.md'] = ((directory / 'README.md').read_bytes(), 0o644)
    if (ROOT / 'LICENSE').is_file():
        files['LICENSE'] = ((ROOT / 'LICENSE').read_bytes(), 0o644)
    for name in entry['share']:
        files[name] = ((directory / ('provider' + Path(name).suffix)).read_bytes(), 0o644)
    filename = entry['archive']
    for key, value in [('version', args.version), ('os', args.os), ('arch', args.arch)]:
        filename = filename.replace('%{' + key + '}', value)
    archive = output / filename
    with tarfile.open(archive, 'w:gz') as tar:
        for name, (content, mode) in sorted(files.items()):
            info = tarfile.TarInfo(name)
            info.size, info.mode = len(content), mode
            tar.addfile(info, io.BytesIO(content))
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / (filename + '.sha256')).write_text(checksum + '  ' + filename + '\n')
(output / 'package-index.json').write_bytes((ROOT / 'package-index.json').read_bytes())
