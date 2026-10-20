"""One actual My tasks path with only the paired executables, no receipt/cache."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import shutil
import time
import desktop_session as ds
import memory_probe as m
import scenario_probe as s
import settings_probe as p


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('main', 'tool', 'restore-exe'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    shell, tool, restore = [path.resolve(strict=True) for path in (args.main, args.tool, args.restore_exe)]
    version = tool.name.removeprefix('minicon-tskmgr-v').removesuffix('-x86_64-windows.exe')
    assert tool.name == f'minicon-tskmgr-v{version}-x86_64-windows.exe'
    folder = p.ROOT/'target/test-reports/tool-offline'/str(time.time_ns())
    folder.mkdir(parents=True)
    report = dict(passed=False, main_sha256=s.digest(shell), tool_sha256=s.digest(tool))
    handle = None
    process = None

    def window():
        found = []
        callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)(lambda h, _:
            (found.append(h) if p.u.IsWindowVisible(h)
             and p.text(h).startswith('minicon-tskmgr '+version+' ') else None) or True)
        p.u.EnumWindows(callback, 0)
        return next(iter(found), None)

    p.u.EnumWindows.argtypes = [c.c_void_p, w.LPARAM]
    assert not window(), 'Close existing same-version task windows before this owned probe'
    isolation = None
    try:
        with ds.isolated(shell, restore) as isolation:
            shutil.copy2(shell, p.LIVE)
            sibling = p.LIVE.parent/tool.name
            shutil.copy2(tool, sibling)
            assert not (p.LIVE.parent/'SHA256SUMS.txt').exists()
            settings = folder/'settings.conf'
            settings.write_text('ui.language=en\nappearance.mode=dark\n', encoding='utf-8')
            env = {key:value for key,value in os.environ.items() if not key.startswith('MINICON_')}
            env.update(MINICON_SETTINGS=str(settings), MINICON_CONFIG=str(folder/'empty.conf'),
                       MINICON_LOG_DIR=str(folder/'logs'), LOCALAPPDATA=str(folder/'profile'))
            desktop = s.Desktop()
            child = desktop.start(p.LIVE, env)
            p.show()
            p.click(6410)
            def opened():
                result = window()
                if result:
                    return result
                dialogs = []
                callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)(lambda h, _:
                    (dialogs.append(h) if p.pid(h)==child.pid and p.u.IsWindowVisible(h)
                     and p.text(h)=='Minicon Tskmgr' and p.u.GetDlgItem(h, 7) else None) or True)
                p.u.EnumWindows(callback, 0)
                if dialogs:
                    p.u.PostMessageW(p.u.GetDlgItem(dialogs[0], 7), 0xf5, 0, 0)
                    raise AssertionError('Offline pair unexpectedly requested a download')
                return None
            candidate = p.wait(opened, timeout=12)
            process = m.Process(p.pid(candidate))
            assert desktop.image(process) == sibling.resolve()
            assert process.image_hash == report['tool_sha256']
            handle = candidate
            assert desktop.same_user_session(process)
            assert not int.from_bytes(desktop.token(process.handle, 20)[:4], 'little')
            assert not (p.LIVE.parent/'SHA256SUMS.txt').exists()
            assert not (folder/'profile/minicon-winmgr/tools').exists()
            p.u.PostMessageW(handle, 16, 0, 0)
            assert p.k.WaitForSingleObject(process.handle, 8000) == 0
            assert child.poll() is None
            report['two_executables_no_receipt_or_cache_opened_standard_tool'] = True
        report['passed'] = bool(isolation['restored'])
        report['original_desktop_restored'] = bool(isolation['restored'])
    except BaseException as error:
        report['error'] = repr(error)
        raise
    finally:
        if handle and p.u.IsWindow(handle):
            p.u.PostMessageW(handle, 16, 0, 0)
        if process:
            process.close()
        (folder/'summary.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(json.dumps(report), flush=True)


if __name__ == '__main__':
    main()
