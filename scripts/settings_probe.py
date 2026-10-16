"""0.1.6 desktop acceptance on the current session, with isolated configuration.

Build first, gracefully replace only this session's instance, run native input
and persistence checks, then restore the normal live instance in finally.
Never changes the Windows shell registration or reads document contents.
"""
from __future__ import annotations
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import tomllib
import winreg

ROOT = Path(__file__).resolve().parents[1]
LIVE = ROOT / 'target/debug/live/minicon-winmgr.exe'
SOURCE = ROOT / 'target/debug/minicon-winmgr.exe'
TEST = ROOT / 'target/settings-acceptance'
CONFIG = TEST / 'settings.txt'
REPORT = ROOT / 'target/test-reports/settings/settings.json'
u = c.WinDLL('user32', use_last_error=True)
k = c.WinDLL('kernel32', use_last_error=True)
u.SetProcessDPIAware()
for name, args, result in [
    ('FindWindowW', [w.LPCWSTR, w.LPCWSTR], w.HWND),
    ('FindWindowExW', [w.HWND, w.HWND, w.LPCWSTR, w.LPCWSTR], w.HWND),
    ('GetForegroundWindow', [], w.HWND), ('GetDlgCtrlID', [w.HWND], c.c_int),
    ('SetForegroundWindow', [w.HWND], w.BOOL),
    ('SendMessageW', [w.HWND, w.UINT, w.WPARAM, w.LPARAM], c.c_ssize_t),
    ('PostMessageW', [w.HWND, w.UINT, w.WPARAM, w.LPARAM], w.BOOL),
    ('GetWindowTextW', [w.HWND, w.LPWSTR, c.c_int], c.c_int),
    ('GetWindowTextLengthW', [w.HWND], c.c_int),
    ('GetDlgItem', [w.HWND, c.c_int], w.HWND),
    ('IsWindowVisible', [w.HWND], w.BOOL), ('IsWindowEnabled', [w.HWND], w.BOOL),
    ('IsWindow', [w.HWND], w.BOOL),
    ('GetWindowRect', [w.HWND, c.POINTER(w.RECT)], w.BOOL),
    ('GetClientRect', [w.HWND, c.POINTER(w.RECT)], w.BOOL),
    ('ClientToScreen', [w.HWND, c.POINTER(w.POINT)], w.BOOL),
    ('MoveWindow', [w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.BOOL], w.BOOL),
    ('GetWindowThreadProcessId', [w.HWND, c.POINTER(w.DWORD)], w.DWORD),
    ('GetGUIThreadInfo', [w.DWORD, c.c_void_p], w.BOOL),
    ('GetGuiResources', [w.HANDLE, w.DWORD], w.DWORD),
]:
    fn = getattr(u, name); fn.argtypes = args; fn.restype = result
k.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]; k.OpenProcess.restype = w.HANDLE
k.CloseHandle.argtypes = [w.HANDLE]
k.GetProcessHandleCount.argtypes = [w.HANDLE, c.POINTER(w.DWORD)]
k.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
class KEY(c.Structure):
    _fields_ = [('vk', w.WORD), ('scan', w.WORD), ('flags', w.DWORD), ('time', w.DWORD), ('extra', c.c_size_t)]
class MOUSE(c.Structure):
    _fields_ = [('dx', w.LONG), ('dy', w.LONG), ('data', w.DWORD), ('flags', w.DWORD), ('time', w.DWORD), ('extra', c.c_size_t)]
class UNION(c.Union): _fields_ = [('key', KEY), ('mouse', MOUSE)]
class INPUT(c.Structure): _fields_ = [('kind', w.DWORD), ('value', UNION)]
class GTI(c.Structure):
    _fields_ = [('size', w.DWORD), ('flags', w.DWORD), ('active', w.HWND), ('focus', w.HWND), ('capture', w.HWND), ('menu', w.HWND), ('move', w.HWND), ('caret', w.HWND), ('rect', w.RECT)]
u.SendInput.argtypes = [w.UINT, c.POINTER(INPUT), c.c_int]
VK = {'ALT': 18, 'SHIFT': 16, 'CTRL': 17, 'WIN': 91, 'TAB': 9, 'ENTER': 13, 'ESC': 27, 'SPACE': 32, 'DOWN': 40, 'UP': 38, 'BACK': 8}

def key(name):
    codes = [VK.get(n, ord(n) if len(n) == 1 else 0) for n in name.split('+')]
    values = [INPUT(1, UNION(key=KEY(v, 0, 0, 0, 0))) for v in codes]
    values += [INPUT(1, UNION(key=KEY(v, 0, 2, 0, 0))) for v in reversed(codes)]
    inputs = (INPUT * len(values))(*values)
    assert u.SendInput(len(values), inputs, c.sizeof(INPUT)) == len(values)
    time.sleep(.06)  # SendInput queues input; wait for the target message pump.

def typing(value):
    encoded = value.encode('utf-16-le')
    for i in range(0, len(encoded), 2):
        unit = int.from_bytes(encoded[i:i + 2], 'little')
        inputs = (INPUT * 2)(INPUT(1, UNION(key=KEY(0, unit, 4, 0, 0))), INPUT(1, UNION(key=KEY(0, unit, 6, 0, 0))))
        assert u.SendInput(2, inputs, c.sizeof(INPUT)) == 2

def wait(predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result: return result
        time.sleep(.025)
    raise AssertionError('condition timed out')

def window(name): return u.FindWindowW(name, None)
def message(): return u.FindWindowExW(w.HWND(-3), None, 'minicon_winmgr_message_window', None)
def dialog():
    found = []
    callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)(lambda hwnd, _: (found.append(hwnd) or True) if pid(hwnd) == pid(message()) else True)
    u.EnumWindows.argtypes = [c.c_void_p, w.LPARAM]; u.EnumWindows(callback, 0)
    for hwnd in found:
        name = c.create_unicode_buffer(128); u.GetClassNameW(w.HWND(hwnd), name, 128)
        if name.value == '#32770' and u.IsWindowVisible(hwnd): return hwnd
    return None
def panel(): return window('minicon_winmgr_settings')
def pid(hwnd):
    value = w.DWORD(); u.GetWindowThreadProcessId(hwnd, c.byref(value)); return value.value
def text(hwnd):
    buffer = c.create_unicode_buffer(u.SendMessageW(hwnd, 14, 0, 0) + 1)
    u.SendMessageW(hwnd, 13, len(buffer), c.addressof(buffer)); return buffer.value
def rect(hwnd):
    r = w.RECT(); assert u.GetWindowRect(hwnd, c.byref(r)); return [r.left, r.top, r.right, r.bottom]
def controls():
    result = {}
    callback = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)(lambda hwnd, _: (result.__setitem__(u.GetDlgCtrlID(hwnd), hwnd) or True))
    u.EnumChildWindows.argtypes = [w.HWND, c.c_void_p, w.LPARAM]
    u.EnumChildWindows(panel(), callback, 0); return result
def ctrl(ident): return controls()[ident]
def click(ident): u.SendMessageW(ctrl(ident), 0xF5, 0, 0)
def checked(ident): return bool(u.SendMessageW(ctrl(ident), 0xF0, 0, 0))
def focus():
    g = GTI(); g.size = c.sizeof(g); assert u.GetGUIThreadInfo(u.GetWindowThreadProcessId(panel(), None), c.byref(g)); return g.focus
def activate():
    if u.GetForegroundWindow() != panel():
        key('ALT'); assert u.SetForegroundWindow(panel())
    time.sleep(.08)
    g = GTI(); g.size = c.sizeof(g)
    u.GetGUIThreadInfo(u.GetWindowThreadProcessId(panel(), None), c.byref(g))
    if g.flags & 4: key('ALT')  # Foreground activation can enter native menu mode.
def focus_id(ident):
    for _ in range(20):
        if u.GetForegroundWindow() != panel(): activate()
        if u.GetDlgCtrlID(focus()) == ident: return
        key('TAB'); time.sleep(.035)
    raise AssertionError(f'cannot focus {ident}')
def show():
    u.SendMessageW(message(), 0x8000 + 141, 0, 0)
    wait(lambda: u.IsWindowVisible(panel())); activate()
def saved():
    time.sleep(.12)
    wait(lambda: not any(word in text(ctrl(6112)) for word in ['正在保存','Saving']) and not u.IsWindowEnabled(ctrl(6113)))
    assert not any(word in text(ctrl(6112)) for word in ['失败','could not']), text(ctrl(6112))
def metrics():
    process = k.OpenProcess(0x1000, False, pid(message())); assert process
    try:
        count = w.DWORD(); assert k.GetProcessHandleCount(process, c.byref(count))
        class MEMORY(c.Structure):
            _fields_ = [('cb', w.DWORD), ('faults', w.DWORD)] + [(name, c.c_size_t) for name in ['peak_working', 'working', 'peak_paged', 'paged', 'peak_nonpaged', 'nonpaged', 'pagefile', 'peak_pagefile', 'private']]
        memory = MEMORY(); memory.cb = c.sizeof(memory)
        psapi = c.WinDLL('psapi'); psapi.GetProcessMemoryInfo.argtypes = [w.HANDLE, c.c_void_p, w.DWORD]
        assert psapi.GetProcessMemoryInfo(process, c.byref(memory), memory.cb)
        return {'handles': count.value, 'gdi': u.GetGuiResources(process, 0), 'user': u.GetGuiResources(process, 1), 'working_bytes': memory.working, 'private_bytes': memory.private}
    finally: k.CloseHandle(process)

def capture_settings(output, hwnd=None):
    hwnd = hwnd or panel()
    # Render only our window: desktop crops can accidentally include another
    # application if the user changes foreground while acceptance is running.
    from PIL import Image
    g = c.WinDLL('gdi32', use_last_error=True)
    u.GetDC.argtypes = [w.HWND]; u.GetDC.restype = w.HDC
    u.ReleaseDC.argtypes = [w.HWND, w.HDC]
    u.PrintWindow.argtypes = [w.HWND, w.HDC, w.UINT]
    g.CreateCompatibleDC.argtypes = [w.HDC]; g.CreateCompatibleDC.restype = w.HDC
    g.CreateCompatibleBitmap.argtypes = [w.HDC, c.c_int, c.c_int]; g.CreateCompatibleBitmap.restype = w.HBITMAP
    g.SelectObject.argtypes = [w.HDC, w.HGDIOBJ]; g.SelectObject.restype = w.HGDIOBJ
    g.DeleteObject.argtypes = [w.HGDIOBJ]; g.DeleteDC.argtypes = [w.HDC]
    g.GetDIBits.argtypes = [w.HDC, w.HBITMAP, w.UINT, w.UINT, c.c_void_p, c.c_void_p, w.UINT]
    class HEADER(c.Structure):
        _fields_ = [('size', w.DWORD), ('width', w.LONG), ('height', w.LONG), ('planes', w.WORD), ('bits', w.WORD), ('compression', w.DWORD), ('image_size', w.DWORD), ('x', w.LONG), ('y', w.LONG), ('used', w.DWORD), ('important', w.DWORD)]
    rect = w.RECT(); assert u.GetClientRect(hwnd, c.byref(rect))
    width, height = rect.right, rect.bottom
    dc = u.GetDC(hwnd); assert dc
    memory = g.CreateCompatibleDC(dc); bitmap = g.CreateCompatibleBitmap(dc, width, height)
    assert memory and bitmap
    old = g.SelectObject(memory, bitmap)
    try:
        assert u.PrintWindow(hwnd, memory, 3), 'PrintWindow settings failed'
        g.SelectObject(memory, old)
        header = HEADER(c.sizeof(HEADER), width, -height, 1, 32)
        buffer = c.create_string_buffer(width * height * 4)
        assert g.GetDIBits(memory, bitmap, 0, height, buffer, c.byref(header), 0) == height
        Image.frombytes('RGB', (width, height), buffer.raw, 'raw', 'BGRX').save(output)
    finally:
        g.SelectObject(memory, old); g.DeleteObject(bitmap); g.DeleteDC(memory); u.ReleaseDC(hwnd, dc)

def shell_settings():
    result = {}
    for name, hive in [('user', winreg.HKEY_CURRENT_USER), ('machine', winreg.HKEY_LOCAL_MACHINE)]:
        try:
            with winreg.OpenKey(hive, r'SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon') as reg:
                result[name] = winreg.QueryValueEx(reg, 'Shell')
        except FileNotFoundError: result[name] = None
    return result

def stop():
    hwnd = message()
    if hwnd:
        process = k.OpenProcess(0x100000, False, pid(hwnd))
        assert process
        try:
            u.PostMessageW(hwnd, 16, 0, 0)
            assert k.WaitForSingleObject(process, 8000) == 0, 'instance refused graceful exit'
        finally: k.CloseHandle(process)

def start(environment, args=()):
    flags = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP | 0x01000000
    subprocess.Popen([str(LIVE), *args], cwd=LIVE.parent, env=environment, creationflags=flags, close_fds=True)
    wait(message); wait(panel); time.sleep(.4)

def main(*, include_run_regression=True, build=True):
    u.OpenInputDesktop.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    u.OpenInputDesktop.restype = w.HANDLE
    u.CloseDesktop.argtypes = [w.HANDLE]
    desktop = u.OpenInputDesktop(0, False, 1)
    if not desktop:
        raise RuntimeError('Input desktop unavailable; unlock/reconnect this session before interactive acceptance. Existing report preserved.')
    u.CloseDesktop(desktop)
    report = {'version': tomllib.loads((ROOT/'Cargo.toml').read_text(encoding='utf-8'))['package']['version'], 'tests': [], 'limits': [
        'Physical mixed-DPI monitors and monitor unplugging are not available in this session.',
        'Screen-reader speech and system high-contrast appearance require separate environmental coverage.',
    ]}
    def check(name, condition, detail=None):
        report['tests'].append({'name': name, 'passed': bool(condition), 'detail': detail})
        if not condition: raise AssertionError(name)
    original_environment = os.environ.copy()
    environment = original_environment.copy()
    for name in ['MINICON_NO_TASKBAR', 'MINICON_TASKBAR', 'MINICON_TRAY', 'MINICON_GROUP_TASKS', 'MINICON_GROUP_BY', 'MINICON_HOVER_PREVIEW', 'MINICON_THEME_MODE', 'MINICON_TASKBAR_POSITION', 'MINICON_LANGUAGE']:
        environment.pop(name, None)
    TEST.mkdir(parents=True, exist_ok=True)
    environment['MINICON_LANGUAGE']='zh'
    environment['MINICON_SETTINGS'] = str(CONFIG)
    environment['MINICON_CONFIG'] = str(TEST / 'empty.conf')
    environment['LOCALAPPDATA'] = str(TEST / 'profile')
    environment['RUST_LOG'] = 'info'
    CONFIG.write_text('# acceptance only\nunknown = keep\ntaskbar.group = never\nbehavior.hover_preview = false\n', encoding='utf-8')
    original_shell = shell_settings()
    if build: subprocess.run(['cargo', 'build'], cwd=ROOT, check=True)
    try:
        stop()
        deadline = time.monotonic() + 8
        while True:
            try:
                shutil.copy2(SOURCE, LIVE)
                break
            except PermissionError:
                if time.monotonic() >= deadline: raise
                time.sleep(.2)
        start(environment, ['--settings']); show()
        first_pid = pid(message())
        result = subprocess.run([str(LIVE), '--settings'], env=environment, capture_output=True, timeout=8)
        check('settings_cli_reuses_same_instance', result.returncode == 0 and pid(message()) == first_pid)
        activate(); focus_id(6101)
        sequence = []
        for _ in range(10):
            key('TAB'); time.sleep(.035); sequence.append(u.GetDlgCtrlID(focus()))
        check('native_tab_sequence_skips_disabled_radio_basis', sequence == [6102, 6103, 6108, 6401, 6120, 6205, 6114, 6132, 6141, 6101], sequence)
        key('SHIFT+TAB'); time.sleep(.05)
        check('reverse_tab', u.GetDlgCtrlID(focus()) == 6141)
        focus_id(6120); key('ENTER')
        info = wait(lambda: window('minicon_winmgr_settings_info'))
        wait(lambda: u.IsWindowVisible(info))
        edit = u.GetDlgItem(info, 1)
        check('configuration_details_show_values_sources_and_save_path', '配置来源' in text(edit) and '下次启动' in text(edit) and str(CONFIG) in text(edit))
        key('CTRL+A')
        check('configuration_details_support_native_text_selection', u.SendMessageW(edit, 0xB0, 0, 0) != 0)
        key('ESC'); wait(lambda: not u.IsWindowVisible(info))
        wait(lambda: u.GetDlgCtrlID(focus()) == 6120)
        check('configuration_details_close_restores_entry_focus', True)
        key('ENTER'); wait(lambda: u.IsWindowVisible(info))
        check('configuration_details_reuse_window', window('minicon_winmgr_settings_info') == info)
        key('ESC'); wait(lambda: not u.IsWindowVisible(info))
        focus_id(6103); key('DOWN'); time.sleep(.15)
        check('radio_arrow_applies_and_enables_basis', checked(6104) and u.IsWindowEnabled(ctrl(6106)))
        saved()
        focus_id(6101); key('SPACE'); saved()
        check('pending_taskbar_hide_keeps_current_bar_and_dependencies', not checked(6101) and window('minicon_winmgr_taskbar') and u.IsWindowEnabled(ctrl(6102)), text(ctrl(6301)))
        key('SPACE'); saved()
        check('restore_next_start_taskbar', checked(6101))
        focus_id(6108); key('SPACE'); saved()
        check('preview_initially_disabled_can_initialize_on_enable', checked(6108) and window('minicon_winmgr_hover_preview'))
        key('SPACE'); saved()
        # Native Tab reaches controls outside a small viewport and auto-scrolls.
        check('advanced_actions_initially_collapsed', not u.IsWindowVisible(ctrl(6110)))
        focus_id(6205); key('ENTER')
        wait(lambda:u.IsWindowVisible(ctrl(6110)))
        check('advanced_actions_expand_from_keyboard', u.IsWindowVisible(ctrl(6110)))
        u.MoveWindow(panel(), 120, 100, 640, 480, True); time.sleep(.15)
        focus_id(6110); time.sleep(.15)
        viewport = controls()[0]; cr = rect(viewport); br = rect(ctrl(6110))
        check('small_viewport_scrolls_focused_action_into_view', br[1] >= cr[1] and br[3] <= cr[3], {'viewport': cr, 'focused': br})
        footer = rect(ctrl(6114)); outer = rect(panel())
        check('small_viewport_footer_remains_reachable', outer[1] <= footer[1] < footer[3] <= outer[3])
        key('ENTER'); confirmation = wait(dialog)
        if u.GetForegroundWindow() != confirmation: u.SetForegroundWindow(confirmation)
        key('ESC'); wait(lambda: not dialog())
        wait(lambda: u.GetDlgCtrlID(focus()) == 6110)
        check('cancel_returns_focus_to_original_settings_button', True)
        check('desktop_action_confirmation_cancel_preserves_shell', shell_settings() == original_shell)
        show(); key('ESC'); time.sleep(.1)
        check('escape_hides_reused_settings', not u.IsWindowVisible(panel()))
        show(); check('reopen_same_window', pid(panel()) == first_pid)
        # Failure injection is confined to the isolated settings path.
        saved(); previous = CONFIG.read_bytes(); held = CONFIG.with_suffix('.held')
        CONFIG.replace(held); CONFIG.mkdir()
        click(6102)
        wait(lambda: u.IsWindowEnabled(ctrl(6113)))
        check('save_failure_is_visible_and_session_value_applied', not checked(6102) and '失败' in text(ctrl(6112)), text(ctrl(6112)))
        u.SendMessageW(panel(), 16, 0, 0); show()
        check('save_failure_survives_hide_reopen', u.IsWindowEnabled(ctrl(6113)))
        # Failed shutdown returns to settings when the default No is chosen.
        u.PostMessageW(message(), 16, 0, 0); wait(lambda: dialog())
        activate_dialog = dialog()
        if u.GetForegroundWindow() != activate_dialog: u.SetForegroundWindow(activate_dialog)
        key('ENTER'); time.sleep(.15)
        check('dirty_shutdown_default_returns_to_settings', bool(message()) and u.IsWindowVisible(panel()))
        CONFIG.rmdir(); held.replace(CONFIG)
        check('failed_write_kept_original_bytes', CONFIG.read_bytes() == previous)
        click(6113); saved()
        check('retry_same_choice_persists_without_retoggling', 'taskbar.tray = false' in CONFIG.read_text(encoding='utf-8') and 'unknown = keep' in CONFIG.read_text(encoding='utf-8'))
        # Warm up caches, then check repeated reusable-window and preview operations.
        for _ in range(10):
            u.SendMessageW(panel(), 16, 0, 0); u.SendMessageW(message(), 0x8000 + 141, 0, 0); click(6108); click(6108)
        saved(); time.sleep(.5); samples = [metrics()]
        latencies = []
        for i in range(100):
            started = time.perf_counter()
            u.SendMessageW(panel(), 16, 0, 0); u.SendMessageW(message(), 0x8000 + 141, 0, 0); click(6108); click(6108)
            latencies.append((time.perf_counter() - started) * 1000)
            if i == 49: samples.append(metrics())
        saved(); time.sleep(.8); samples.append(metrics())
        check('100_settings_reopens_and_preview_cycles_no_resource_growth', all(samples[-1][k] <= samples[0][k] + (8 if k == 'handles' else 2 if k in ['gdi', 'user'] else 8 * 1024 * 1024) for k in samples[0]), samples)
        ordered = sorted(latencies)
        report['synchronous_reopen_and_two_toggles_ms'] = {'median': ordered[50], 'p95': ordered[94], 'max': ordered[-1], 'method': 'SendMessage hide/show plus two native button clicks; excludes asynchronous disk completion'}
        # Next-start setting is exercised with an actual process restart.
        click(6101); saved(); check('saved_restart_preference', not checked(6101))
        stop(); start(environment, ['--settings']); show()
        check('disabled_taskbar_start_still_has_settings_cli', not window('minicon_winmgr_taskbar') and not u.IsWindowEnabled(ctrl(6102)) and u.IsWindowEnabled(ctrl(6101)))
        result = subprocess.run([str(LIVE), '--settings'], env=environment, capture_output=True, timeout=8)
        check('settings_cli_without_taskbar_reuses_instance', result.returncode == 0)
        click(6101); saved(); stop()
        overridden = environment | {'MINICON_TASKBAR': 'true', 'MINICON_NO_TASKBAR': '1', 'MINICON_GROUP_TASKS': 'false', 'MINICON_TRAY': 'not-a-bool'}
        start(overridden, ['--settings']); show()
        check('valid_env_override_is_read_only_and_invalid_env_is_not', not u.IsWindowEnabled(ctrl(6101)) and checked(6101) and '固定' in text(ctrl(6301)) and all('MINICON_' not in text(ctrl(cid)) and '环境变量' not in text(ctrl(cid)) for cid in [6301, 6303, 6312]) and not u.IsWindowEnabled(ctrl(6103)) and u.IsWindowEnabled(ctrl(6102)))
        check('environment_did_not_rewrite_file_preference', 'taskbar.show = true' in CONFIG.read_text(encoding='utf-8'))
        stop(); start(environment, ['--settings']); show()
        # Keep the established Run acceptance in its isolated LOCALAPPDATA.
        existing_regression = ROOT / '.workbuddy/acceptance.py'
        if include_run_regression and existing_regression.exists():
            u.SendMessageW(panel(), 16, 0, 0)
            result = subprocess.run([sys.executable, str(existing_regression)], cwd=ROOT, env=environment, capture_output=True, timeout=60)
            details = json.loads((ROOT / '.workbuddy/acceptance-results.json').read_text(encoding='utf-8'))
            report['run_regression'] = [{'name': entry.get('test'), 'passed': entry.get('passed', False)} for entry in details]
            check('established_run_dialog_regression', result.returncode == 0 and all(entry['passed'] for entry in report['run_regression']))
        # Actual Run execution expands argument variables, retaining quoted spacing.
        profile = TEST / 'profile'; profile.mkdir(exist_ok=True)
        source = TEST / 'run_probe.rs'
        source.write_text('fn main(){let a:Vec<String>=std::env::args().collect();std::fs::write(&a[1],format!("{:?}",a)).unwrap();}', encoding='utf-8')
        executable = TEST / 'Run Probe.exe'
        subprocess.run(['rustc', str(source), '-o', str(executable)], cwd=ROOT, check=True, capture_output=True)
        output = profile / 'args.txt'
        if output.exists(): output.unlink()
        key('WIN+R'); run = wait(lambda: window('minicon_winmgr_run_dialog'))
        wait(lambda: u.IsWindowVisible(run))
        key('CTRL+A'); key('BACK')
        typing(f'"{executable}" "%LOCALAPPDATA%\\args.txt" "%USERPROFILE%" %UNKNOWN%')
        time.sleep(.2); key('ENTER'); wait(output.exists)
        arguments = output.read_text(encoding='utf-8')
        check('run_expands_known_argument_variables_and_preserves_unknown', '%LOCALAPPDATA%' not in arguments and '%USERPROFILE%' not in arguments and '%UNKNOWN%' in arguments)
        # A missing associated editor must not be misreported as a missing document.
        document = profile / 'association.json'; document.write_text('{}', encoding='utf-8')
        key('WIN+R'); wait(lambda: u.IsWindowVisible(run)); key('CTRL+A'); key('BACK')
        typing(r'%LOCALAPPDATA%\association.json'); time.sleep(.2); key('ENTER'); time.sleep(.8)
        check('document_path_expands_or_explains_broken_association', not u.IsWindowVisible(run) or '默认打开程序不可用' in text(u.GetDlgItem(run, 2008)), text(u.GetDlgItem(run, 2008)))
        if u.IsWindowVisible(run): key('ESC')
        key('WIN+B'); notifications = wait(lambda: window('minicon_winmgr_notification_panel'))
        wait(lambda: u.IsWindowVisible(notifications))
        wait(lambda:text(u.GetDlgItem(notifications, 6)) == '时间设置')
        check('notification_panel_retains_time_settings_entry', text(u.GetDlgItem(notifications, 6)) == '时间设置', text(u.GetDlgItem(notifications, 6)))
        u.SendMessageW(notifications, 16, 0, 0)
        show()
        # Accessible names/roles are provided by genuine native controls.
        check('native_control_names_and_states', text(ctrl(6101)).startswith('显示任务栏') and text(ctrl(6114)) == '关闭')
        automation = '''[void][Reflection.Assembly]::LoadWithPartialName('UIAutomationClient')
$condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NativeWindowHandleProperty,[int]HANDLE)
$root = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children,$condition)
$items = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition)
$result = foreach ($item in $items) { $v = $item.Current; if ($v.AutomationId -in @('6101','6103','6106','6114')) { $patterns = @($item.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName }); [pscustomobject]@{ id=$v.AutomationId; name=$v.Name; role=$v.ControlType.ProgrammaticName; enabled=$v.IsEnabled; patterns=$patterns } } }
ConvertTo-Json -InputObject @($result) -Depth 4 -Compress
'''.replace('HANDLE', str(panel()))
        # Prefer PowerShell 7: the legacy Windows PowerShell 5 UIA bridge
        # reports generic Pane roles on this machine. Keep provider wait bounded.
        accessible_ok = False
        for _ in range(5):
            result = subprocess.run([shutil.which('pwsh') or 'powershell', '-NoProfile', '-Command', '[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; ' + automation], capture_output=True, timeout=20)
            report['uia_controls'] = json.loads(result.stdout.decode('utf-8-sig'))
            accessible = {item['id']: item for item in report['uia_controls']}
            accessible_ok = result.returncode == 0 and accessible.get('6101', {}).get('role') == 'ControlType.CheckBox' and 'TogglePatternIdentifiers.Pattern' in accessible['6101']['patterns'] and accessible.get('6103', {}).get('role') == 'ControlType.RadioButton' and accessible.get('6114', {}).get('name') == '关闭'
            if accessible_ok: break
            time.sleep(.3)
        check('uia_exposes_native_roles_names_and_patterns', accessible_ok, report['uia_controls'])
        try:
            output = REPORT.parent / 'images/settings.png'; output.parent.mkdir(parents=True, exist_ok=True)
            capture_settings(output)
            report['screenshot'] = 'images/settings.png'
        except ImportError: report['screenshot'] = 'Pillow unavailable'
        report['passed'] = True
    except Exception as error:
        report['passed'] = False; report['error'] = repr(error)
        report['failure_ui']={'settings_title':text(panel()),'notification_time':text(u.GetDlgItem(window('minicon_winmgr_notification_panel'),6))}
        raise
    finally:
        # Recover an interrupted fault-injection before asking the test instance to exit.
        held = CONFIG.with_suffix('.held')
        if held.exists():
            if CONFIG.is_dir(): CONFIG.rmdir()
            held.replace(CONFIG)
        if panel():
            click(6113)
            try: saved()
            except AssertionError: pass
        try:
            stop(); start(original_environment)
            report['normal_session_restored'] = bool(window('minicon_winmgr_taskbar'))
        finally:
            report['shell_registration_unchanged'] = shell_settings() == original_shell
            REPORT.parent.mkdir(parents=True, exist_ok=True)
            REPORT.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
            print(json.dumps(report, ensure_ascii=True, indent=2))

if __name__ == "__main__":
    raise SystemExit("Use scripts/test.py --layer settings --restore-exe <normal-running-exe> for isolated desktop checks.")
