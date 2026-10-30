"""Own-taskbar fullscreen-yield acceptance; restore the original live binary.

A fullscreen foreground window on the bar's monitor must make the bar leave
the topmost band (WS_EX_TOPMOST cleared, the fullscreen surface wins hit
testing over the bar area); destroying the window must restore topmost within
one clock tick. A work-area-sized (maximized shape) window must never yield.
"""
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import os
from pathlib import Path
import shutil
import time

import settings_probe as p

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'target/test-reports/fullscreen-yield' / str(time.time_ns())

GWL_EXSTYLE = -20
WS_EX_TOPMOST = 0x00000008
WS_POPUP_VISIBLE = 0x90000000  # WS_POPUP | WS_VISIBLE
PROBE_CLASS = 'minicon_fullscreen_probe'
WNDPROC = c.WINFUNCTYPE(c.c_ssize_t, w.HWND, c.c_uint, w.WPARAM, w.LPARAM)


class MONITORINFO(c.Structure):
    _fields_ = [('cbSize', c.c_uint), ('rcMonitor', w.RECT), ('rcWork', w.RECT),
                ('dwFlags', c.c_uint)]


class WNDCLASSW(c.Structure):
    _fields_ = [('style', c.c_uint), ('lpfnWndProc', WNDPROC), ('cbClsExtra', c.c_int),
                ('cbWndExtra', c.c_int), ('hInstance', w.HINSTANCE), ('hIcon', w.HICON),
                ('hCursor', c.c_void_p), ('hbrBackground', c.c_void_p),
                ('lpszMenuName', w.LPCWSTR), ('lpszClassName', w.LPCWSTR)]


def bind():
    p.u.GetWindowLongW.argtypes = [w.HWND, c.c_int]
    p.u.GetWindowLongW.restype = w.LONG
    p.u.WindowFromPoint.argtypes = [w.POINT]
    p.u.WindowFromPoint.restype = w.HWND
    p.u.GetAncestor.argtypes = [w.HWND, c.c_uint]
    p.u.GetAncestor.restype = w.HWND
    p.u.MonitorFromWindow.argtypes = [w.HWND, w.DWORD]
    p.u.MonitorFromWindow.restype = w.HANDLE
    p.u.GetMonitorInfoW.argtypes = [w.HANDLE, c.POINTER(MONITORINFO)]
    p.u.GetMonitorInfoW.restype = w.BOOL
    p.u.RegisterClassW.argtypes = [c.POINTER(WNDCLASSW)]
    p.u.RegisterClassW.restype = w.WORD
    p.u.CreateWindowExW.argtypes = [w.DWORD, w.LPCWSTR, w.LPCWSTR, w.DWORD, c.c_int, c.c_int,
                                    c.c_int, c.c_int, w.HWND, w.HMENU, w.HINSTANCE, w.LPVOID]
    p.u.CreateWindowExW.restype = w.HWND
    p.u.DestroyWindow.argtypes = [w.HWND]
    p.u.DestroyWindow.restype = w.BOOL
    p.k.GetModuleHandleW.argtypes = [w.LPCWSTR]
    p.k.GetModuleHandleW.restype = w.HMODULE
    # Without explicit types the 64-bit lparam is truncated inside the window
    # procedure and WM_NCCREATE returns garbage, aborting CreateWindowExW.
    p.u.DefWindowProcW.argtypes = [w.HWND, c.c_uint, w.WPARAM, w.LPARAM]
    p.u.DefWindowProcW.restype = c.c_ssize_t


def main():
    OUT.mkdir(parents=True)
    bind()
    original = os.environ.copy()
    environment = original.copy()
    for name in ['MINICON_NO_TASKBAR', 'MINICON_TASKBAR', 'MINICON_TASKBAR_POSITION',
                 'MINICON_LANGUAGE', 'MINICON_HOVER_PREVIEW', 'MINICON_THEME_MODE',
                 'MINICON_SETTINGS', 'MINICON_TRAY']:
        environment.pop(name, None)
    profile = OUT / 'profile'
    profile.mkdir()
    environment.update(USERPROFILE=str(profile), MINICON_CONFIG=str(OUT / 'empty.conf'),
                       MINICON_LANGUAGE='en', MINICON_HOVER_PREVIEW='0')
    backup = OUT / 'original-live.exe'
    shutil.copy2(p.LIVE, backup)
    with backup.open('rb') as source:
        original_hash = hashlib.file_digest(source, 'sha256').hexdigest()
    shell = p.shell_settings()
    with p.SOURCE.open('rb') as source:
        binary_hash = hashlib.file_digest(source, 'sha256').hexdigest()
    report = {'checks': [], 'passed': False, 'binary_sha256': binary_hash,
              'limits': ['Current physical display only; multi-monitor yield is unit-tested.']}
    # The fixture window never pumps messages while we poll; without this the
    # system could swap in a DWM ghost window with a different pid.
    p.u.DisableProcessWindowsGhosting()

    def check(name, condition, detail=None):
        report['checks'].append({'name': name, 'passed': bool(condition), 'detail': detail})
        assert condition, name

    def bar():
        return p.window('minicon_winmgr_taskbar')

    def bar_topmost():
        return bool(p.u.GetWindowLongW(bar(), GWL_EXSTYLE) & WS_EX_TOPMOST)

    def hit_at_bar_center():
        r = p.rect(bar())
        hwnd = p.u.WindowFromPoint(w.POINT((r[0] + r[2]) // 2, (r[1] + r[3]) // 2))
        root = p.u.GetAncestor(hwnd, 2)  # GA_ROOT
        return root if root else hwnd

    def wait_until(condition, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if condition():
                return True
            time.sleep(.2)
        return bool(condition())

    wndproc = WNDPROC(lambda hwnd, msg, wp, lp: p.u.DefWindowProcW(hwnd, msg, wp, lp))
    registered = []

    def make_window(x, y, width, height):
        if not registered:
            cls = WNDCLASSW()
            cls.lpfnWndProc = wndproc
            cls.lpszClassName = PROBE_CLASS
            cls.hInstance = p.k.GetModuleHandleW(None)
            assert p.u.RegisterClassW(c.byref(cls))
            registered.append(cls)  # keep the class struct and wndproc alive
        hwnd = p.u.CreateWindowExW(0, PROBE_CLASS, 'fullscreen probe fixture',
                                   WS_POPUP_VISIBLE, x, y, width, height,
                                   None, None, p.k.GetModuleHandleW(None), None)
        assert hwnd, 'fixture window creation failed'
        p.key('ALT')  # makes SetForegroundWindow reliable for this process
        assert p.u.SetForegroundWindow(hwnd)
        p.wait(lambda: p.u.GetForegroundWindow() == hwnd)
        return hwnd

    fixture = None
    try:
        p.stop()
        shutil.copy2(p.SOURCE, p.LIVE)
        p.start(environment)
        p.wait(bar)
        check('bar_starts_topmost', bar_topmost())

        mi = MONITORINFO()
        mi.cbSize = c.sizeof(MONITORINFO)
        assert p.u.GetMonitorInfoW(p.u.MonitorFromWindow(bar(), 2), c.byref(mi))
        monitor, work = mi.rcMonitor, mi.rcWork

        # 1. A real fullscreen foreground window makes the bar yield.
        fixture = make_window(monitor.left, monitor.top,
                              monitor.right - monitor.left, monitor.bottom - monitor.top)
        check('fullscreen_foreground_drops_bar_topmost',
              wait_until(lambda: not bar_topmost(), 8))
        check('fullscreen_surface_wins_bar_hit_test', hit_at_bar_center() == fixture,
              {'hit': hit_at_bar_center(), 'fixture': fixture})

        # 2. Destroying the fullscreen window restores topmost within a tick.
        assert p.u.DestroyWindow(fixture)
        fixture = None
        check('bar_restores_topmost_after_fullscreen_exits', wait_until(bar_topmost, 8))

        # 3. A window matching the work area (maximized shape) never yields.
        fixture = make_window(work.left, work.top,
                              work.right - work.left, work.bottom - work.top)
        yielded = wait_until(lambda: not bar_topmost(), 7)  # beyond one clock tick
        check('work_area_sized_window_keeps_bar_topmost', not yielded and bar_topmost())
        assert p.u.DestroyWindow(fixture)
        fixture = None

        check('shell_registration_unchanged', p.shell_settings() == shell)
        report['passed'] = True
    except Exception as error:
        report['error'] = type(error).__name__
        raise
    finally:
        if fixture:
            p.u.DestroyWindow(fixture)
        p.stop()
        shutil.copy2(backup, p.LIVE)
        p.start(original)
        report['normal_session_restored'] = bool(p.wait(bar))
        with p.LIVE.open('rb') as live:
            report['original_binary_restored'] = hashlib.file_digest(live, 'sha256').hexdigest() == original_hash
        report['shell_registration_unchanged'] = p.shell_settings() == shell
        report['passed'] = report['passed'] and all(report[key] for key in
            ['normal_session_restored', 'original_binary_restored', 'shell_registration_unchanged'])
        (OUT / 'summary.json').write_text(json.dumps(report, indent=2) + '\n', encoding='utf-8')
        print(json.dumps({'passed': report['passed'], 'checks': len(report['checks']),
                          'normal_session_restored': report['normal_session_restored']}), flush=True)
        if not report['passed']:
            raise RuntimeError('Fullscreen-yield acceptance or restoration failed; inspect the local report')
    return report


if __name__ == "__main__":
    raise SystemExit("Use scripts/test.py --layer fullscreen --restore-exe <normal-running-exe> for isolated desktop checks.")
