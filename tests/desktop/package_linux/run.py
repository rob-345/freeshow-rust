"""Run the extracted, normal release package without a system installation.

Only ordinary renderer commands and bounded read-only WebDriver observations
are used. No frontend build injection, codec-path override or native IO mock.
"""
import ctypes
import base64
import fcntl
import hashlib
from http.client import HTTPConnection
import itertools
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE.parent))
from script_receipts import wrap_script


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def port():
    with socket.socket() as handle:
        handle.bind(('127.0.0.1', 0))
        return handle.getsockname()[1]


def terminate(process):
    if process is None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def wait(predicate, description, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            print('PASS', description, flush=True)
            return value
        time.sleep(0.1)
    raise AssertionError('Extracted package deadline: ' + description)


def observe_codec(pid, binary):
    """Observe the real loader snapshot while its actual native owner is live."""
    process = Path('/proc') / str(pid)
    executable = process / 'exe'
    assert os.path.samefile(executable, binary)
    environment = (process / 'environ').read_bytes().split(b'\0')
    overrides = [value.split(b'=', 1)[0].decode() for value in environment if value.split(b'=', 1)[0] in [b'FREESHOW_RUST_LTC_LIBRARY', b'APPDIR', b'LD_LIBRARY_PATH', b'LD_PRELOAD']]
    assert overrides == []
    source = (binary.parent.parent / 'lib/FreeShow Rust/codecs/libltc.so.11').resolve(strict=True)
    original = source.stat()
    expected = sha(source)
    snapshots = []
    for index, entry in enumerate((process / 'fd').iterdir()):
        assert index < 512, 'Native descriptor observation bound'
        try:
            target = os.readlink(entry)
        except FileNotFoundError:
            continue
        if 'memfd:freeshow-verified-ltc' not in target:
            continue
        with entry.open('rb') as held:
            metadata = os.fstat(held.fileno())
            assert metadata.st_size == original.st_size <= 1024 * 1024
            seals = fcntl.fcntl(held.fileno(), fcntl.F_GET_SEALS)
            required = fcntl.F_SEAL_SEAL | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_GROW | fcntl.F_SEAL_WRITE
            assert seals & required == required
            actual_hash = hashlib.file_digest(held, 'sha256').hexdigest()
            assert actual_hash == expected
        with (process / 'maps').open('rb') as stream:
            raw = stream.read(2 * 1024 * 1024 + 1)
            assert len(raw) <= 2 * 1024 * 1024
        mappings = []
        for line in raw.decode().splitlines():
            if 'memfd:freeshow-verified-ltc' not in line:
                continue
            fields = line.split(maxsplit=5)
            major, minor = [int(value, 16) for value in fields[3].split(':')]
            assert (major, minor, int(fields[4])) == (os.major(metadata.st_dev), os.minor(metadata.st_dev), metadata.st_ino)
            mappings.append(line)
        assert mappings and any('r-xp' in line for line in mappings)
        snapshots.append({'descriptor': entry.name, 'target': target, 'device': metadata.st_dev, 'inode': metadata.st_ino, 'bytes': metadata.st_size, 'sha256': actual_hash, 'seals': seals, 'maps': mappings})
    assert len(snapshots) == 1
    return {'canonicalPackagedPath': str(source), 'packagedDevice': original.st_dev, 'packagedInode': original.st_ino, 'packagedBytes': original.st_size, 'packagedSha256': expected, 'actualExeSameFile': True, 'actualProcessOverrides': overrides, 'sealedMappedSnapshots': snapshots, 'originContract': {'mainSourceSha256': sha(ROOT / 'src-tauri/src/main.rs'), 'loaderSourceSha256': sha(ROOT / 'crates/freeshow-ltc/src/backend.rs'), 'candidateOrder': ['FREESHOW_RUST_LTC_LIBRARY if present', 'resource_dir()/codecs/libltc.so.11 otherwise'], 'otherCandidateFallbacks': False}}


def main():
    assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0
    verified = json.loads(Path(sys.argv[1]).read_text())
    artifacts = Path(sys.argv[2]).resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    binary = Path(verified['binary']).resolve()
    assert verified['ok'] and sha(binary) == verified['binarySha256']
    assert str(artifacts).startswith('/mnt/data/')
    driver = shutil.which('tauri-driver')
    webkit = os.environ.get('WEBKIT_WEBDRIVER') or shutil.which('WebKitWebDriver')
    assert driver and webkit and shutil.which('Xvfb')
    launcher = Path(f'/proc/{os.getppid()}/cmdline').read_bytes().replace(b'\0', b' ').decode()
    assert 'dbus-run-session' in launcher and os.environ.get('DBUS_SESSION_BUS_ADDRESS'), 'Run under a private dbus-run-session'
    result = {'ok': False, 'sourceHead': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT).decode().strip(), 'packageSha256': verified['packageSha256'], 'binary': str(binary), 'binarySha256': sha(binary), 'normalProductionRelease': True, 'diagnosticBundle': False, 'systemInstallationPerformed': False, 'performanceSample': False, 'physicalMidiOrAudioVerified': False}
    process = pulse = server = discovery_peer = None
    session = None
    result['privateBusLauncher'] = launcher
    # Keep native Unix socket paths short, independently of artifact/cache paths.
    profile_base = Path('/mnt/data/cache/fspkg')
    profile_base.mkdir(mode=0o700, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='fspkg-', dir=profile_base) as name:
        temporary = Path(name)
        app, data, runtime = temporary / 'app', temporary / 'library', temporary / 'runtime'
        for directory in [app, data, runtime]:
            directory.mkdir(mode=0o700)
        settings = {'initialized': True, 'language': 'en', 'ports': {'remote': 0, 'stage': 0, 'controller': 0}, 'disabledServers': {'remote': False, 'stage': False, 'controller': False, 'output_stream': True}, 'serverData': {'companion': {'enabled': False}}, 'special': {'showClosePopup': False, 'icecast': {'enabled': False}}}
        (app / 'settings.json').write_text(json.dumps(settings))
        pulse_socket = temporary / 'pulse.sock'
        environment = dict(os.environ, FREESHOW_RUST_APP_DATA=str(app), FREESHOW_RUST_DATA_DIR=str(data), XDG_DATA_HOME=str(temporary / 'xdg-data'), XDG_CACHE_HOME=str(temporary / 'xdg-cache'), XDG_CONFIG_HOME=str(temporary / 'xdg-config'), XDG_RUNTIME_DIR=str(runtime), PULSE_SERVER='unix:' + str(pulse_socket), PULSE_SINK='freeshow_package_review', GSETTINGS_BACKEND='memory', TAURI_WEBVIEW_AUTOMATION='true', WEBKIT_DISABLE_COMPOSITING_MODE='1', LANG='C.UTF-8', LC_ALL='C.UTF-8', FREESHOW_RUST_API_BIND_IP='127.0.0.1', FREESHOW_RUST_REMOTES_BIND_IP='127.0.0.1')
        for key in ['FREESHOW_RUST_LTC_LIBRARY', 'APPDIR', 'LD_LIBRARY_PATH', 'LD_PRELOAD']:
            environment.pop(key, None)
        result['resourceOverridesAbsent'] = ['FREESHOW_RUST_LTC_LIBRARY', 'APPDIR', 'LD_LIBRARY_PATH', 'LD_PRELOAD']
        log = (artifacts / 'driver.log').open('w')
        pulse_log = (artifacts / 'pulse.log').open('w')
        display_log = (artifacts / 'xvfb.log').open('w')
        try:
            discovery_peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            discovery_peer.bind(('127.0.0.1', 0))
            discovery_target = '127.0.0.1:' + str(discovery_peer.getsockname()[1])
            environment['FREESHOW_RUST_DISCOVERY_LOOPBACK_TARGET'] = discovery_target
            result['discoveryFixture'] = {'explicitTarget': discovery_target, 'peerBoundBeforeApp': True, 'packets': [], 'peerClosed': False}
            read_fd, write_fd = os.pipe()
            try:
                server = subprocess.Popen(['Xvfb', '-displayfd', str(write_fd), '-screen', '0', '1600x1000x24', '-nolisten', 'tcp'], pass_fds=(write_fd,), stdout=display_log, stderr=subprocess.STDOUT, start_new_session=True)
                os.close(write_fd)
                write_fd = None
                assert select.select([read_fd], [], [], 5)[0], 'Private Xvfb display deadline'
                display = os.read(read_fd, 32).decode().strip()
                assert display.isdecimal() and len(display) <= 6
                environment['DISPLAY'] = ':' + display
                result['privateDisplay'] = environment['DISPLAY']
                result['xvfbPid'] = server.pid
            finally:
                os.close(read_fd)
                if write_fd is not None:
                    os.close(write_fd)
            pulse = subprocess.Popen(['pulseaudio', '-n', '--daemonize=no', '--use-pid-file=no', '--exit-idle-time=-1', '--log-target=stderr', '--load=module-native-protocol-unix socket=' + str(pulse_socket) + ' auth-anonymous=1', '--load=module-null-sink sink_name=freeshow_package_review rate=48000 channels=2'], env=environment, stdout=pulse_log, stderr=subprocess.STDOUT, start_new_session=True)
            wait(lambda: pulse_socket.exists(), 'private virtual sink', 5)
            driver_port, native_port = port(), port()
            process = subprocess.Popen([driver, '--port', str(driver_port), '--native-port', str(native_port), '--native-driver', webkit], env=environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            requests = 0
            deadline = time.monotonic() + 120

            def http(method, route, body=None):
                nonlocal requests
                requests += 1
                assert requests <= 256 and time.monotonic() < deadline
                endpoint = driver_port if method == 'POST' and route == '/session' else native_port
                request = urllib.request.Request(f'http://127.0.0.1:{endpoint}' + route, method=method, data=None if body is None else json.dumps(body).encode(), headers={'Content-Type': 'application/json'})
                try:
                    with urllib.request.urlopen(request, timeout=25) as response:
                        raw = response.read(1024 * 1024 + 1)
                        assert len(raw) <= 1024 * 1024
                        value = json.loads(raw)['value']
                except urllib.error.HTTPError as error:
                    raise RuntimeError(error.read(16384).decode()) from error
                assert not (isinstance(value, dict) and isinstance(value.get('error'), str) and 'message' in value), value
                return value

            keys = itertools.count()

            def script(source, *args):
                return http('POST', f'/session/{session}/execute/sync', {'script': wrap_script(next(keys), source), 'args': list(args)})

            def ready():
                try:
                    return http('GET', '/status').get('ready')
                except urllib.error.URLError:
                    return False

            wait(ready, 'private WebDriver')
            session = http('POST', '/session', {'capabilities': {'alwaysMatch': {'tauri:options': {'application': str(binary)}}}})['sessionId']
            script((HERE / 'probe.js').read_text(), 'install')
            wait(lambda: script("return !!window.api?.getRuntimeContext() && !!document.querySelector('main .left')"), 'normal packaged editor')
            pids = []
            for entry in Path('/proc').glob('[0-9]*'):
                try:
                    if (entry / 'exe').resolve() == binary:
                        pids.append(int(entry.name))
                except (OSError, RuntimeError):
                    pass
            assert len(pids) == 1, pids
            result['appPid'] = pids[0]
            if script("return !!document.querySelector('#guideButtons')"):
                element = http('POST', f'/session/{session}/element', {'using': 'css selector', 'value': '#guideButtons button'})['element-6066-11e4-a52e-4f735466cecf']
                http('POST', f'/session/{session}/element/{element}/click', {})
            result['editor'] = script((HERE / 'probe.js').read_text(), 'version')
            assert result['editor']['origin'] == 'tauri://localhost'
            assert result['editor']['version'] == '1.6.7-beta.2-rust.1'
            assert result['editor']['context']['dataPath'] == str(data)
            assert result['editor']['diagnosticGlobalsAbsent']
            screenshot = base64.b64decode(http('GET', f'/session/{session}/screenshot'), validate=True)
            assert screenshot.startswith(b'\x89PNG\r\n\x1a\n') and len(screenshot) <= 768 * 1024
            (artifacts / 'editor.png').write_bytes(screenshot)
            result['editorScreenshotSha256'] = hashlib.sha256(screenshot).hexdigest()
            binding = wait(lambda: script("return window.__TAURI__.core.invoke('remotes_status').then(s=>s.binding?.bound?.endpoints && Object.keys(s.binding.bound.endpoints).length===3 ? s.binding:null)"), 'all packaged remote resources served from actual native bindings')
            assert binding['context'] == result['editor']['context']
            result['remoteBinding'] = binding
            # Observe bounded real discovery traffic, without any multicast
            # transport or ambient target selection in this private fixture.
            assert select.select([discovery_peer], [], [], 5)[0], 'Loopback discovery packet deadline'
            for _ in range(32):
                if not select.select([discovery_peer], [], [], 0)[0]:
                    break
                payload, address = discovery_peer.recvfrom(65507)
                assert address[0] == '127.0.0.1'
                result['discoveryFixture']['packets'].append({'sourceIp': address[0], 'bytes': len(payload), 'sha256': hashlib.sha256(payload).hexdigest()})
            assert result['discoveryFixture']['packets']

            remote_http = []
            for kind in ['REMOTE', 'STAGE', 'CONTROLLER']:
                endpoint = binding['bound']['endpoints'][kind]
                assert endpoint.startswith('127.0.0.1:')
                connection = HTTPConnection(endpoint, timeout=5)
                try:
                    for method in ['GET', 'HEAD']:
                        connection.request(method, '/')
                        response = connection.getresponse()
                        body = response.read(2 * 1024 * 1024 + 1)
                        original = binary.parent.parent / 'lib/FreeShow Rust/remotes' / kind.lower() / 'index.html'
                        assert response.status == 200 and len(body) <= 2 * 1024 * 1024
                        assert 'text/html' in response.getheader('Content-Type')
                        if method == 'GET':
                            assert body == original.read_bytes()
                        else:
                            assert body == b''
                        remote_http.append({'kind': kind, 'method': method, 'status': response.status, 'contentType': response.getheader('Content-Type'), 'contentLength': response.getheader('Content-Length'), 'bytes': len(body), 'sha256': hashlib.sha256(body).hexdigest()})
                finally:
                    connection.close()
            result['remoteHttp'] = remote_http
            result['packagedLtc'] = script((HERE.parent / 'timecode_probe.js').read_text(), {'phase': 'extracted-package', 'fps': 25})
            assert result['packagedLtc']['actualNativeLtc']['loaded']
            assert result['packagedLtc']['actualNativeLtc']['encoderCreated'] == result['packagedLtc']['actualNativeLtc']['encoderFreed']
            assert result['packagedLtc']['actualNativeLtc']['decoderCreated'] == result['packagedLtc']['actualNativeLtc']['decoderFreed']
            result['actualCodecOrigin'] = observe_codec(result['appPid'], binary)
            result['final'] = script((HERE / 'probe.js').read_text(), 'finish')
            assert not result['final']['browserErrors']
            for event in result['final']['runtimeErrors']:
                channel = event.get('request', {}).get('data', {}).get('channel')
                assert channel in ['DEVICE_ID', 'PROVIDER_CONNECTIONS', 'SPOTIFY_GET_STATE'] and event['error']['code'] == 'unsupported_command', event
            result['knownIndependentStartupGaps'] = result['final']['runtimeErrors']
            script("window.api.send('MAIN',{channel:'CLOSE'})")
            result['normalCloseAttempted'] = True
            wait(lambda: not Path('/proc').joinpath(str(result['appPid'])).exists(), 'normal packaged CLOSE', 20)
            result['normalCloseExitedBeforeFixtureTermination'] = True
            result['webdriverRequests'] = requests
            result['ok'] = True
        except Exception as error:
            result['error'] = str(error)
            if session:
                try:
                    result['failureObservation'] = script((HERE / 'probe.js').read_text(), 'snapshot')
                    script("window.api.send('MAIN',{channel:'CLOSE'})")
                    result['normalCloseAttempted'] = True
                    if 'appPid' in result:
                        wait(lambda: not Path('/proc').joinpath(str(result['appPid'])).exists(), 'failure normal CLOSE', 20)
                        result['normalCloseExitedBeforeFixtureTermination'] = True
                except Exception as cleanup:
                    result['failureCloseError'] = str(cleanup)
            raise
        finally:
            terminate(process)
            terminate(pulse)
            terminate(server)
            if discovery_peer is not None:
                discovery_peer.close()
                result['discoveryFixture']['peerClosed'] = discovery_peer.fileno() == -1

            log.close()
            pulse_log.close()
            display_log.close()
            until = time.monotonic() + 5
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if not pid:
                    assert time.monotonic() < until
                    time.sleep(0.05)
            children = Path(f'/proc/{os.getpid()}/task/{os.getpid()}/children').read_text().strip()
            result['actualFixtureCleanup'] = {'driverJoined': process is None or process.poll() is not None, 'pulseJoined': pulse is None or pulse.poll() is not None, 'xvfbJoined': server is None or server.poll() is not None, 'ownedChildren': children}
            assert not children
            assert sha(binary) == result['binarySha256']
            (artifacts / 'gui-receipt.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__':
    main()
