"""Inspect and extract the unsigned artifact; never install a package."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
LTC_SHA = 'f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f'


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def command(*args):
    value = subprocess.check_output(args, stderr=subprocess.STDOUT, timeout=60)
    assert len(value) <= 8 * 1024 * 1024
    return value.decode()


def verify(package, extracted):
    package, extracted = Path(package).resolve(), Path(extracted).resolve()
    assert package.is_file() and package.stat().st_size <= 256 * 1024 * 1024
    assert str(extracted).startswith('/mnt/data/') and not extracted.exists()
    archive_members = command('ar', 't', str(package)).splitlines()
    assert len(archive_members) == 3 and archive_members[0] == 'debian-binary'
    assert archive_members[1].startswith('control.tar.') and archive_members[2].startswith('data.tar.')
    fields = {key: command('dpkg-deb', '--field', str(package), key).strip() for key in ['Package', 'Version', 'Architecture', 'Maintainer', 'Depends', 'Recommends', 'Description', 'Installed-Size']}
    assert 0 < int(fields['Installed-Size']) <= 512 * 1024
    assert fields['Version'] == '0.1.0' and fields['Architecture'] == 'amd64'
    for dependency in ['libc6', 'libasound2', 'libopus0', 'libgtk-3', 'libwebkit2gtk-4.1']:
        assert dependency in fields['Depends'], fields
    assert fields['Maintainer'] and 'Rust' in fields['Description']
    command('dpkg-deb', '--extract', str(package), str(extracted))
    binary = extracted / 'usr/bin/freeshow-desktop'
    resources = extracted / 'usr/lib/FreeShow Rust'
    codec = resources / 'codecs/libltc.so.11'
    assert binary.is_file() and os.access(binary, os.X_OK)
    assert codec.is_file() and sha(codec) == LTC_SHA
    docs = extracted / 'usr/share/doc/freeshow-desktop'
    assert (docs / 'GPL-3.0.txt').read_bytes() == (ROOT / 'LICENSE').read_bytes()
    assert (docs / 'libltc/COPYING.LESSER').is_file()
    notices = json.loads((docs / 'dependency-notices.json').read_text())
    assert notices['ltc']['sha256'] == LTC_SHA and notices['ltc']['correspondingBinarySourceUnresolved']
    assert len(notices['inventory']) > 500 and notices['noticeFiles'] > 500
    # The normal build includes these three original browser clients. Validate
    # every packaged resource against its production build input, not a marker.
    remote_rows = []
    source = ROOT / 'dist-rust/remotes'
    for original in sorted(source.rglob('*')):
        if not original.is_file():
            continue
        relative = original.relative_to(source)
        installed = resources / 'remotes' / relative
        assert installed.is_file() and sha(installed) == sha(original), str(relative)
        remote_rows.append({'path': str(relative), 'bytes': installed.stat().st_size, 'sha256': sha(installed)})
    assert 10 <= len(remote_rows) <= 128
    for kind in ['remote', 'stage', 'controller']:
        assert any(row['path'].lower().startswith(kind + '/') and row['path'].endswith('.html') for row in remote_rows), kind
    desktop = list((extracted / 'usr/share/applications').glob('*.desktop'))
    icons = list((extracted / 'usr/share/icons').rglob('*.png'))
    assert len(desktop) == 1 and icons
    desktop_text = desktop[0].read_text()
    assert 'Exec=freeshow-desktop' in desktop_text and 'Icon=' in desktop_text
    dynamic = command('readelf', '--dynamic', str(binary))
    assert not re.search(r'\((?:RPATH|RUNPATH)\)', dynamic)
    dependencies = command('ldd', str(binary))
    assert 'not found' not in dependencies
    assert '/home/rob-admin/.local' not in dependencies and '/mnt/data/' not in dependencies
    elf_versions = command('readelf', '--version-info', str(binary))
    glibc = sorted(set(re.findall(r'GLIBC_([\d.]+)', elf_versions)), key=lambda text: tuple(map(int, text.split('.'))))
    assert glibc and tuple(map(int, glibc[-1].split('.'))) <= (2, 39)
    manifest = hashlib.sha256()
    count = total = 0
    elf_files = []
    for file in sorted(extracted.rglob('*')):
        relative = str(file.relative_to(extracted))
        assert not file.is_symlink(), relative
        if not file.is_file():
            continue
        assert not any(part in ['node_modules', 'electron'] for part in file.relative_to(extracted).parts), relative
        count += 1
        total += file.stat().st_size
        assert count <= 10000 and total <= 512 * 1024 * 1024
        with file.open('rb') as stream:
            if stream.read(4) == b'\x7fELF':
                elf_files.append(relative)
        manifest.update((relative + '\0' + oct(file.stat().st_mode & 0o7777) + '\0' + sha(file) + '\n').encode())
    assert sorted(elf_files) == ['usr/bin/freeshow-desktop', 'usr/lib/FreeShow Rust/codecs/libltc.so.11'], elf_files
    return {'ok': True, 'package': str(package), 'packageBytes': package.stat().st_size, 'packageSha256': sha(package), 'fields': fields, 'archiveMembers': archive_members, 'elfFiles': elf_files, 'extracted': str(extracted), 'binary': str(binary), 'binarySha256': sha(binary), 'codecSha256': sha(codec), 'desktop': desktop_text, 'iconFiles': [str(value.relative_to(extracted)) for value in icons], 'resources': remote_rows, 'fileCount': count, 'installedBytes': total, 'contentManifestSha256': manifest.hexdigest(), 'maximumGlibcSymbol': glibc[-1], 'systemDependencies': dependencies, 'systemInstallationPerformed': False, 'signed': False, 'licenseCompletenessCertified': False}


if __name__ == '__main__':
    result = verify(sys.argv[1], sys.argv[2])
    output = Path(sys.argv[3])
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({key: result[key] for key in ['ok', 'packageSha256', 'binarySha256', 'fileCount', 'maximumGlibcSymbol']}))
