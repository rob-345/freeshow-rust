"""Freeze a just-built normal Linux Tauri binary before test feature unification.

The caller must run build -> freeze sequentially, before any other Cargo writes.
This records observed artifacts/features; it does not attest compiler provenance.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def freeze(build_log, binary, resources, destination, receipt):
    binary = binary.resolve(strict=True)
    resources = resources.resolve(strict=True)
    assert binary.is_file() and resources.is_dir(), 'Missing normal binary/resources'
    assert build_log.stat().st_size <= 64 * 1024 * 1024, 'Oversized compiler record'
    artifacts = []
    completed = []
    for line in build_log.read_text().splitlines():
        row = json.loads(line)
        if row.get('reason') == 'build-finished':
            completed.append(row['success'])
        if row.get('reason') == 'compiler-artifact':
            artifacts.append(row)
    assert completed == [True], 'Build did not successfully finish'
    desktop = [row for row in artifacts if row.get('target', {}).get('name') == 'freeshow-desktop' and row['target']['kind'] == ['bin']]
    assert len(desktop) == 1 and desktop[0]['features'] == ['custom-protocol'] and not desktop[0]['profile']['test'], 'Expected normal desktop build'
    assert Path(desktop[0]['executable']).resolve(strict=True) == binary, 'Compiler executable differs'
    browse = [row for row in artifacts if row.get('target', {}).get('name') == 'freeshow_browse']
    assert len(browse) == 1 and browse[0]['features'] == [], 'Browse test harness is enabled'
    local = [row for row in artifacts if row.get('target', {}).get('name', '').startswith('freeshow')]
    assert all('test-harness' not in row['features'] for row in local), 'Native test harness is enabled'
    source_files = sorted(resources.rglob('*'))
    assert source_files and len(source_files) <= 4096 and all(not path.is_symlink() for path in source_files), 'Unsupported resource tree'
    before = digest(binary)
    resource_records = [{'path': str(path.relative_to(resources)), 'bytes': path.stat().st_size, 'sha256': digest(path)} for path in source_files if path.is_file()]
    assert resource_records, 'No remote resources'
    # Keep Tauri's native Cargo target/debug resolver and adjacent resource tree.
    assert destination.name == 'debug' and destination.parent.name == 'target', 'Expected isolated target/debug layout'
    assert not destination.exists(), 'Refusing to replace a frozen normal binary'
    destination.mkdir(parents=True)
    try:
        frozen = destination / binary.name
        shutil.copy2(binary, frozen)
        shutil.copytree(resources, destination / 'remotes')
        (destination / '.cargo-lock').touch()
        assert digest(binary) == digest(frozen) == before, 'Binary changed during freeze'
        expected_paths = [row['path'] for row in resource_records]
        for tree in [resources, destination / 'remotes']:
            observed = sorted(tree.rglob('*'))
            assert all(not path.is_symlink() for path in observed), 'Resource link appeared during freeze'
            assert [str(path.relative_to(tree)) for path in observed if path.is_file()] == expected_paths, 'Resource set changed during freeze'
        for row in resource_records:
            source = resources / row['path']
            copied = destination / 'remotes' / row['path']
            assert source.stat().st_size == copied.stat().st_size == row['bytes'] and digest(source) == digest(copied) == row['sha256'], 'Resource changed during freeze'
        result = {'schemaVersion': 1, 'binary': {'source': str(binary), 'path': str(frozen.resolve()), 'bytes': frozen.stat().st_size, 'sha256': before}, 'desktopFeatures': desktop[0]['features'], 'browseFeatures': browse[0]['features'], 'localFeatures': {row['target']['name']: row['features'] for row in local}, 'resources': resource_records, 'isolatedFromSubsequentCargoWrites': True, 'compilerProvenanceAttested': False}
        receipt.parent.mkdir(parents=True, exist_ok=True)
        receipt.write_text(json.dumps(result, indent=2) + '\n')
    except BaseException:
        shutil.rmtree(destination)
        raise
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ['build-log', 'binary', 'resources', 'destination', 'receipt']:
        parser.add_argument('--' + option, type=Path, required=True)
    args = parser.parse_args()
    result = freeze(args.build_log, args.binary, args.resources, args.destination, args.receipt)
    print(json.dumps({'frozen': result['binary']['path'], 'sha256': result['binary']['sha256'], 'resourceFiles': len(result['resources'])}))
