"""Exercise a published updater against real Latest, restoring the original shell.

Only owned copies are updated. No API override, UAC, registry writes or user
installation replacement. Raw evidence remains in ignored target directories.
"""
import argparse
import ctypes as c
import json
import os
from pathlib import Path
import shutil
import time
import tomllib

import desktop_session as d
import memory_probe as m
import scenario_probe as s
import settings_probe as p


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('candidate', 'legacy-exe', 'restore-exe'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    candidate = args.candidate.resolve(strict=True)
    legacy = args.legacy_exe.resolve(strict=True)
    restore = args.restore_exe.resolve(strict=True)
    version = tomllib.loads((p.ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    folder = p.ROOT / 'target' / ('public-update-' + version) / str(time.time_ns())
    folder.mkdir(parents=True)
    exe = folder / legacy.name
    destination = folder / f'minicon-winmgr-v{version}-x86_64-windows.exe'
    if exe == destination:
        raise ValueError('Published baseline must have an older official filename')
    shutil.copy2(legacy, exe)
    expected = s.digest(candidate)
    old = s.digest(legacy)
    report = dict(candidate_sha256=expected, historical_sha256=old, checks=[], passed=False)
    isolation = None

    def check(name, condition):
        report['checks'].append(dict(name=name, passed=bool(condition)))
        if not condition:
            raise AssertionError(name)

    def enabled(ident):
        return bool(p.u.IsWindowEnabled(p.controls().get(ident, 0)))

    def status():
        return p.text(p.controls().get(6400, 0))

    env = {key: value for key, value in os.environ.items() if not key.startswith('MINICON_')}
    env.update(MINICON_SETTINGS=str(folder / 'settings.txt'),
               MINICON_CONFIG=str(folder / 'empty.conf'),
               MINICON_LOG_DIR=str(folder / 'logs'),
               LOCALAPPDATA=str(folder / 'profile'), MINICON_LANGUAGE='en')
    (folder / 'settings.txt').write_text('ui.language=en\nappearance.mode=light\ntaskbar.menu=false\n', encoding='utf-8')
    (folder / 'empty.conf').write_text('', encoding='utf-8')
    try:
        with d.isolated(candidate, restore) as isolation:
            owned_live = p.LIVE
            try:
                p.LIVE = exe
                p.start(env, ['--settings'])
                p.wait(lambda: 6401 in p.controls() and status())
                check('published_baseline_starts_without_staged_update', not enabled(6402) and not enabled(6403))
                p.click(6401)
                p.wait(lambda: enabled(6402) or 'failed' in status().lower(), timeout=120)
                check('real_public_latest_offers_target_version', enabled(6402) and version in status())
                p.click(6402)
                p.wait(lambda: enabled(6403) or 'failed' in status().lower(), timeout=180)
                check('native_public_download_verified', enabled(6403))
                stage = next(folder.glob('.winmgr-update-*'))
                check('staged_payload_is_accepted_candidate', s.digest(stage / 'payload.exe') == expected)
                before = p.pid(p.message())
                p.click(6403)
                p.wait(lambda: p.message() and p.pid(p.message()) != before and p.panel()
                       and p.text(p.panel()).endswith(version) and (stage / 'started').exists(), timeout=40)
                check('public_restart_acknowledges_actual_target', destination.exists() and s.digest(destination) == expected)
                check('old_name_removed', not exe.exists())
                check('previous_public_binary_preserved', s.digest(stage / 'previous.exe') == old)
                check('new_version_identity_matches', p.text(p.panel()).endswith(version))
                running = m.Process(p.pid(p.message()))
                try:
                    desktop = s.Desktop()
                    check('actual_process_uses_new_filename', desktop.image(running) == destination.resolve())
                    check('actual_process_is_same_user_session_and_standard', desktop.same_user_session(running)
                          and not int.from_bytes(desktop.token(running.handle, 20).raw[:4], 'little'))
                finally:
                    running.close()
                check('settings_preserved', 'appearance.mode=light' in (folder / 'settings.txt').read_text(encoding='utf-8'))
                report['passed'] = True
            finally:
                for stage in folder.glob('.winmgr-update-*'):
                    if not (stage / 'ready').exists() or (stage / 'started').exists():
                        continue
                    (stage / 'abort').write_text('public acceptance cleanup', encoding='utf-8')
                    helper = p.k.OpenProcess(0x101000, False, int((stage / 'ready').read_text()))
                    if helper:
                        try:
                            image = c.create_unicode_buffer(32768)
                            size = p.w.DWORD(len(image))
                            if (p.k.QueryFullProcessImageNameW(helper, 0, image, c.byref(size))
                                    and Path(image.value).resolve() == (stage / 'helper.exe').resolve()):
                                p.k.WaitForSingleObject(helper, 25000)
                        finally:
                            p.k.CloseHandle(helper)
                p.stop()
                p.LIVE = owned_live
    except Exception as error:
        report['error'] = repr(error)
        raise
    finally:
        report['original_shell_restored'] = bool(isolation and isolation.get('restored'))
        report['passed'] = report['passed'] and report['original_shell_restored']
        (folder / 'summary.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(json.dumps(report, indent=2), flush=True)
    if not report['passed']:
        raise RuntimeError('Public update acceptance did not restore the original shell')


if __name__ == '__main__':
    main()
