"""Normal production binary, saved Audio subtab, one real first-mount click.
No bundle injection, module/store/API replacement, navigation retry, or playback.
"""
import ctypes
import hashlib
import itertools
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import wave

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


def wait(predicate, label, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            print('PASS', label, flush=True)
            return value
        time.sleep(0.15)
    raise AssertionError('Startup observation deadline: ' + label)


def one_case(binary, artifacts, selected):
    result = {'ok': False, 'savedSubtab': selected, 'observations': []}
    driver = shutil.which('tauri-driver')
    webkit = os.environ.get('WEBKIT_WEBDRIVER') or shutil.which('WebKitWebDriver')
    assert driver and webkit
    observer = (HERE / 'probe.js').read_text()
    process = pulse = None
    session = None
    with tempfile.TemporaryDirectory(prefix='freeshow-browse-startup-') as name:
        temporary = Path(name)
        app, data, runtime = temporary / 'app', temporary / 'library', temporary / 'runtime'
        for directory in [app, data, runtime]:
            directory.mkdir(mode=0o700)
        folders = {}
        expected = {}
        for index, count in enumerate([2, 3, 1]):
            key = f'startup-{index}'
            folder = data / ('Audio fixture ' + str(index) + ' 音楽')
            folder.mkdir()
            folders[key] = {'name': f'Startup audio {index} 敬拜', 'path': str(folder)}
            expected[key] = []
            for number in range(count):
                file = folder / f'{index}-{number} retained 音楽.wav'
                with wave.open(str(file), 'wb') as stream:
                    stream.setnchannels(1)
                    stream.setsampwidth(2)
                    stream.setframerate(8000)
                    stream.writeframes(b'\0\0' * 8000)
                expected[key].append({'path': str(file), 'name': file.stem, 'duration': '00:01'})
            (folder / 'excluded text.txt').write_text('Real source audio-extension filtering')
        media_folders = {}
        for index in range(2):
            folder = data / f'Media fixture {index}'
            folder.mkdir()
            (folder / 'excluded text.txt').write_text('Seeded real media folder, not an Audio row')
            media_folders[f'media-{index}'] = {'name': f'Startup media {index}', 'path': str(folder)}
        settings = {'initialized': True, 'language': 'en', 'audioFolders': folders, 'mediaFolders': media_folders, 'drawerTabsData': {'audio': {'enabled': True, 'activeSubTab': selected}, 'media': {'enabled': True, 'activeSubTab': 'all'}}, 'drawer': {'height': 420, 'stored': None}, 'special': {'showClosePopup': False, 'icecast': {'enabled': False}}}
        settings_path = app / 'settings.json'
        settings_path.write_text(json.dumps(settings, ensure_ascii=False))
        result['settingsBefore'] = settings
        result['settingsBeforeSha256'] = sha(settings_path)
        result['fixtures'] = [{'path': row['path'], 'bytes': Path(row['path']).stat().st_size, 'sha256': sha(row['path'])} for rows in expected.values() for row in rows]
        wanted = sorted([row for rows in expected.values() for row in rows] if selected == 'all' else expected[selected], key=lambda row: row['path'])
        result['expectedRows'] = wanted
        result['expectedCounts'] = {value['name']: len(expected[key]) for key, value in folders.items()} | {'All': 6}
        pulse_socket = temporary / 'pulse.sock'
        environment = dict(os.environ, FREESHOW_RUST_APP_DATA=str(app), FREESHOW_RUST_DATA_DIR=str(data), XDG_DATA_HOME=str(temporary / 'xdg-data'), XDG_CACHE_HOME=str(temporary / 'xdg-cache'), XDG_CONFIG_HOME=str(temporary / 'xdg-config'), XDG_RUNTIME_DIR=str(runtime), PULSE_SERVER='unix:' + str(pulse_socket), PULSE_SINK='freeshow_browse_startup', GSETTINGS_BACKEND='memory', TAURI_WEBVIEW_AUTOMATION='true', WEBKIT_DISABLE_COMPOSITING_MODE='1', LANG='C.UTF-8', LC_ALL='C.UTF-8', FREESHOW_RUST_API_BIND_IP='127.0.0.1', FREESHOW_RUST_REMOTES_BIND_IP='127.0.0.1')
        log = (artifacts / (selected + '-driver.log')).open('w')
        pulse_log = (artifacts / (selected + '-pulse.log')).open('w')
        try:
            pulse = subprocess.Popen(['pulseaudio', '-n', '--daemonize=no', '--use-pid-file=no', '--exit-idle-time=-1', '--log-target=stderr', '--load=module-native-protocol-unix socket=' + str(pulse_socket) + ' auth-anonymous=1', '--load=module-null-sink sink_name=freeshow_browse_startup rate=48000 channels=2'], env=environment, stdout=pulse_log, stderr=subprocess.STDOUT, start_new_session=True)
            wait(lambda: pulse_socket.exists(), 'private virtual sink', 5)
            driver_port, native_port = port(), port()
            process = subprocess.Popen([driver, '--port', str(driver_port), '--native-port', str(native_port), '--native-driver', webkit], env=environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            requests = 0
            deadline = time.monotonic() + 90

            def http(method, route, body=None):
                nonlocal requests
                requests += 1
                assert requests <= 512 and time.monotonic() < deadline
                endpoint = driver_port if method == 'POST' and route == '/session' else native_port
                request = urllib.request.Request(f'http://127.0.0.1:{endpoint}' + route, method=method, data=None if body is None else json.dumps(body).encode(), headers={'Content-Type': 'application/json'})
                try:
                    with urllib.request.urlopen(request, timeout=20) as response:
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

            def observe(mode='snapshot'):
                value = script(observer, mode)
                assert value['origin'] == 'tauri://localhost' and value['diagnosticGlobalsAbsent']
                assert len(result['observations']) < 128
                result['observations'].append(value)
                for item in value['errors']:
                    assert item.get('detail', {}).get('error', {}).get('code') != 'browse_busy', item
                return value

            def click(selector):
                element = http('POST', f'/session/{session}/element', {'using': 'css selector', 'value': selector})['element-6066-11e4-a52e-4f735466cecf']
                http('POST', f'/session/{session}/element/{element}/click', {})

            def driver_ready():
                try:
                    return http('GET', '/status').get('ready')
                except urllib.error.URLError:
                    return False

            wait(driver_ready, 'private WebDriver')
            session = http('POST', '/session', {'capabilities': {'alwaysMatch': {'tauri:options': {'application': str(binary)}}}})['sessionId']
            observe('install')
            wait(lambda: script("return !!window.api?.getRuntimeContext() && !!document.querySelector('main .left')"), 'normal retained editor')
            pids = []
            for entry in Path('/proc').glob('[0-9]*'):
                try:
                    if (entry / 'exe').resolve() == binary:
                        pids.append(int(entry.name))
                except (OSError, RuntimeError):
                    pass
            assert len(pids) == 1, pids
            result['appPid'] = pids[0]
            result['guideDismissed'] = bool(script("return !!document.querySelector('#guideButtons')"))
            if result['guideDismissed']:
                click('#guideButtons button')
            before = observe()
            assert before['topTab'] == 'shows' and before['rows'] == []
            result['context'] = before['context']
            assert result['context']['dataPath'] == str(data)
            result['beforeFirstAudioMount'] = before
            click('section.drawer .top button#audio')

            def ready():
                value = observe()
                assert value['context'] == result['context']
                counts = {tab['label']: tab['count'] for tab in value['tabs']}
                return value if value['topTab'] == 'audio' and counts == result['expectedCounts'] and value['rows'] == wanted else None

            result['firstAudioMount'] = wait(ready, 'first Audio mount exact file rows and every folder count')
            # Observe stability after ordinary asynchronous callbacks settle;
            # polling is read-only and never reopens a tab or requests files.
            for _ in range(3):
                time.sleep(0.15)
                value = ready()
                assert value is not None, 'first Audio mount must remain correct without navigation'
            final = observe('finish')
            result['final'] = final
            assert final['clicks'] == [{'group': 'top', 'id': 'audio', 'label': 'Audio', 'trusted': True}], final['clicks']
            expected_active = 'All' if selected == 'all' else folders[selected]['name']
            assert [tab['label'] for tab in final['tabs'] if tab['active']] == [expected_active]
            known = []
            for item in final['errors']:
                detail = item.get('detail', {})
                request = detail.get('request', {})
                channel = request.get('data', {}).get('channel')
                error = detail.get('error', {})
                if item.get('kind') == 'freeshow:runtime-error':
                    assert request.get('context') == result['context'], item
                if item.get('kind') == 'freeshow:runtime-error' and request.get('channel') == 'MAIN' and channel in ['DEVICE_ID', 'PROVIDER_CONNECTIONS', 'SPOTIFY_GET_STATE'] and error.get('code') == 'unsupported_command' and error.get('message') == 'unsupported command: MAIN.' + channel:
                    known.append(item)
                else:
                    raise AssertionError('Unexpected startup/browser error: ' + json.dumps(item))
            result['knownIndependentStartupGaps'] = known
            script("window.api.send('MAIN',{channel:'CLOSE'})")
            result['normalCloseAttempted'] = True
            wait(lambda: not Path('/proc').joinpath(str(result['appPid'])).exists(), 'normal native CLOSE', 15)
            result['normalCloseExitedBeforeFixtureTermination'] = True
            saved = json.loads(settings_path.read_text())
            assert saved['audioFolders'] == folders and saved['mediaFolders'] == media_folders
            assert saved['drawerTabsData']['audio']['activeSubTab'] == selected
            result['savedProfileVerified'] = {'audioFolders': saved['audioFolders'], 'mediaFolders': saved['mediaFolders'], 'audioSubtab': saved['drawerTabsData']['audio']['activeSubTab']}
            result['webdriverRequests'] = requests
            result['ok'] = True
        except Exception as error:
            result['error'] = str(error)
            if session:
                try:
                    result['failureObservation'] = script(observer, 'snapshot')
                except Exception as observation:
                    result['failureObservationError'] = str(observation)
                try:
                    script("window.api.send('MAIN',{channel:'CLOSE'})")
                    result['normalCloseAttempted'] = True
                    if 'appPid' in result:
                        wait(lambda: not Path('/proc').joinpath(str(result['appPid'])).exists(), 'failure normal native CLOSE', 15)
                        result['normalCloseExitedBeforeFixtureTermination'] = True
                except Exception as cleanup:
                    result['failureCloseError'] = str(cleanup)
            raise
        finally:
            terminate(process)
            terminate(pulse)
            log.close()
            pulse_log.close()
            until = time.monotonic() + 5
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if not pid:
                    assert time.monotonic() < until
                    time.sleep(0.05)
            result['actualFixtureCleanup'] = {'driverJoined': process is None or process.poll() is not None, 'pulseJoined': pulse is None or pulse.poll() is not None, 'ownedChildren': Path(f'/proc/{os.getpid()}/task/{os.getpid()}/children').read_text().strip()}
            (artifacts / (selected + '-receipt.json')).write_text(json.dumps(result, ensure_ascii=False, indent=2))
    return result


def main():
    assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0
    binary = Path(os.environ.get('FREESHOW_DESKTOP_BIN', '/mnt/data/cache/freeshow-rust-build/browse-admission/target/debug/freeshow-desktop')).resolve()
    assert binary.is_file()
    artifacts = ROOT / 'test-output-rust/browse-startup'
    artifacts.mkdir(parents=True, exist_ok=True)
    receipt = {'ok': False, 'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT).decode().strip(), 'binary': str(binary), 'binarySha256': sha(binary), 'normalProductionBuild': True, 'diagnosticBundle': False, 'performanceSample': False, 'physicalAudioVerified': False, 'cases': []}
    try:
        for selected in ['startup-0', 'all']:
            receipt['cases'].append(one_case(binary, artifacts, selected))
        assert sha(binary) == receipt['binarySha256']
        receipt['ok'] = True
    finally:
        (artifacts / 'receipt.json').write_text(json.dumps(receipt, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
