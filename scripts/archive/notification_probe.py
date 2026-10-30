"""Interactive Windows notification-area acceptance client (Python stdlib only).

Run while minicon-winmgr hosts the notification area, for example:
  python scripts/archive/notification_probe.py --count 8 --hidden 2 --duration 120
  python scripts/archive/notification_probe.py --cycles 100 --output .workbuddy/probe.json

Click the probe icons; callbacks and actual Shell_NotifyIcon return values are
recorded. An optional --command-file accepts a JSON command or array of commands:
  {"op":"tip", "id":41, "tip":"Updated tooltip"}
Operations: add, tip, icon (icon=32512..32516), hide (hidden=true/false), version, delete, stop.
The client re-registers after TaskbarCreated and removes its icons on normal exit.
Killing this process deliberately tests the host's stale-owner cleanup.
"""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import time
import uuid


class GUID(c.Structure):
    _fields_ = [('a', w.DWORD), ('b', w.WORD), ('cc', w.WORD), ('d', c.c_ubyte * 8)]


class NotifyIcon(c.Structure):
    _fields_ = [
        ('size', w.DWORD), ('hwnd', w.HWND), ('id', w.UINT), ('flags', w.UINT),
        ('callback', w.UINT), ('icon', w.HICON), ('tip', w.WCHAR * 128),
        ('state', w.DWORD), ('mask', w.DWORD), ('info', w.WCHAR * 256),
        ('version', w.UINT), ('title', w.WCHAR * 64), ('infoflags', w.DWORD),
        ('guid', GUID), ('balloon', w.HICON),
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--count', type=int, default=8)
    parser.add_argument('--hidden', type=int, default=2)
    parser.add_argument('--version', type=int, choices=[0, 3, 4], default=4)
    parser.add_argument('--duration', type=float, default=120)
    parser.add_argument('--cycles', type=int, default=0)
    parser.add_argument('--guid', action='store_true', help='Use NIF_GUID identities')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--command-file', type=Path)
    args = parser.parse_args()
    if not 0 <= args.count <= 256 or not 0 <= args.hidden <= args.count:
        parser.error('require 0 <= hidden <= count <= 256')
    if args.duration <= 0 or args.cycles < 0:
        parser.error('duration must be positive and cycles nonnegative')
    user = c.WinDLL('user32', use_last_error=True)
    shell = c.WinDLL('shell32', use_last_error=True)
    callback_type = c.WINFUNCTYPE(c.c_ssize_t, w.HWND, w.UINT, w.WPARAM, w.LPARAM)

    class WindowClass(c.Structure):
        _fields_ = [('style', w.UINT), ('proc', callback_type), ('cls', c.c_int),
                    ('wnd', c.c_int), ('instance', w.HINSTANCE), ('icon', w.HICON),
                    ('cursor', w.HANDLE), ('brush', w.HBRUSH),
                    ('menu', w.LPCWSTR), ('name', w.LPCWSTR)]

    user.DefWindowProcW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM]
    user.DefWindowProcW.restype = c.c_ssize_t
    user.CreateWindowExW.argtypes = [w.DWORD, w.LPCWSTR, w.LPCWSTR, w.DWORD,
                                    c.c_int, c.c_int, c.c_int, c.c_int,
                                    w.HWND, w.HMENU, w.HINSTANCE, c.c_void_p]
    user.CreateWindowExW.restype = w.HWND
    user.LoadIconW.argtypes = [w.HINSTANCE, c.c_void_p]
    user.LoadIconW.restype = w.HICON
    user.DestroyWindow.argtypes = [w.HWND]
    user.CopyIcon.argtypes = [w.HICON]
    user.CopyIcon.restype = w.HICON
    user.DestroyIcon.argtypes = [w.HICON]
    shell.Shell_NotifyIconW.argtypes = [w.DWORD, c.POINTER(NotifyIcon)]
    shell.Shell_NotifyIconW.restype = w.BOOL
    report = {'pid': os.getpid(), 'operations': [], 'events': [], 'restarts': 0}
    desired = {}
    hwnd = None
    running = True
    replay = False
    created = user.RegisterWindowMessageW('TaskbarCreated')

    def save():
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            temporary = args.output.with_suffix('.tmp')
            temporary.write_text(json.dumps(report, indent=2), encoding='utf-8')
            # Windows readers can briefly hold the destination without delete
            # sharing while polling this report during desktop acceptance.
            for attempt in range(50):
                try:
                    temporary.replace(args.output)
                    break
                except PermissionError:
                    if attempt == 49:
                        raise
                    time.sleep(.01)

    @callback_type
    def proc(window, message, wp, lp):
        nonlocal replay
        if message == 0x8001:
            report['events'].append({'wparam': wp, 'lparam': lp,
                                     'event': lp & 65535, 'id': (lp >> 16) & 65535 if lp >> 16 else wp})
            save()
            return 0
        if message == created:
            replay = True
            return 0
        return user.DefWindowProcW(window, message, wp, lp)

    wc = WindowClass()
    wc.proc = proc
    wc.name = 'minicon_notification_acceptance_probe'
    if not user.RegisterClassW(c.byref(wc)):
        raise c.WinError(c.get_last_error())
    hwnd = user.CreateWindowExW(0, wc.name, 'Notification acceptance probe', 0,
                                0, 0, 0, 0, None, None, None, None)
    if not hwnd:
        raise c.WinError(c.get_last_error())
    report['hwnd'] = hwnd

    def action(command, remember=True):
        nonlocal running
        op = command['op']
        if op == 'stop':
            running = False
            return
        ident = int(command.get('id', 41))
        data = NotifyIcon()
        data.size = c.sizeof(data)
        data.hwnd = hwnd
        data.id = ident
        owned_icon = None
        if op == 'add':
            data.flags = 7
            data.callback = 0x8001
            data.icon = user.LoadIconW(None, c.c_void_p(32512 + ident % 5))
            data.tip = command.get('tip', f'Probe {ident}')[:127]
            code = 0
        elif op == 'tip':
            data.flags = 4
            data.tip = command['tip'][:127]
            code = 1
        elif op == 'icon':
            resource = int(command.get('icon', 32515))
            if resource not in range(32512, 32517):
                raise ValueError('icon must be a standard system icon ID 32512..32516')
            owned_icon = user.CopyIcon(user.LoadIconW(None, c.c_void_p(resource)))
            data.flags = 2
            data.icon = owned_icon
            code = 1
        elif op == 'hide':
            data.flags = 8
            data.mask = 1
            data.state = int(command.get('hidden', True))
            code = 1
        elif op == 'version':
            data.version = command.get('version', args.version)
            code = 4
        elif op == 'delete':
            code = 2
        else:
            raise ValueError(f'Unknown operation: {op}')
        if args.guid:
            identity = uuid.uuid5(uuid.NAMESPACE_URL, f'minicon-probe/{os.getpid()}/{ident}')
            c.memmove(c.addressof(data.guid), identity.bytes_le, 16)
            data.flags |= 0x20
        ok = bool(shell.Shell_NotifyIconW(code, c.byref(data)))
        if owned_icon:
            user.DestroyIcon(owned_icon)
        report['operations'].append(dict(command, ok=ok))
        if ok and remember:
            if op == 'add':
                desired[ident] = [dict(command)]
            elif op == 'delete':
                desired.pop(ident, None)
            elif ident in desired:
                desired[ident].append(dict(command))
        save()

    try:
        for ident in range(41, 41 + args.count):
            action({'op': 'add', 'id': ident})
            action({'op': 'version', 'id': ident, 'version': args.version})
            if ident < 41 + args.hidden:
                action({'op': 'hide', 'id': ident})
        message = w.MSG()
        end = time.monotonic() + args.duration
        phase = 0
        next_cycle = time.monotonic()
        while running and time.monotonic() < end:
            while user.PeekMessageW(c.byref(message), None, 0, 0, 1):
                user.TranslateMessage(c.byref(message))
                user.DispatchMessageW(c.byref(message))
            if replay:
                replay = False
                report['restarts'] += 1
                for commands in list(desired.values()):
                    for command in commands:
                        action(command, remember=False)
            if phase < args.cycles * 4 and time.monotonic() >= next_cycle:
                commands = [{'op': 'add', 'id': 9001},
                            {'op': 'tip', 'id': 9001, 'tip': f'Cycle {phase // 4}'},
                            {'op': 'hide', 'id': 9001}, {'op': 'delete', 'id': 9001}]
                action(commands[phase % 4])
                phase += 1
                report['completed_cycles'] = phase // 4
                next_cycle = time.monotonic() + .06
            if args.command_file and args.command_file.exists():
                try:
                    command = json.loads(args.command_file.read_text(encoding='utf-8'))
                    args.command_file.unlink()
                except (OSError, ValueError):
                    continue
                for entry in command if isinstance(command, list) else [command]:
                    action(entry)
            time.sleep(.01)
    finally:
        for ident in list(desired):
            action({'op': 'delete', 'id': ident})
        user.DestroyWindow(hwnd)
        report['stopped'] = True
        save()
        print(json.dumps({'pid': report['pid'], 'stopped': True,
                          'operations': len(report['operations']),
                          'failed': [op for op in report['operations'] if not op['ok']],
                          'restarts': report['restarts'],
                          'completed_cycles': report.get('completed_cycles', 0),
                          'events': report['events']}, indent=2))


if __name__ == '__main__':
    main()
