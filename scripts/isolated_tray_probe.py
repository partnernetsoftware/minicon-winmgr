"""Run the native notification fixture on a private, never-activated desktop."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import msvcrt
import os
from pathlib import Path
import subprocess
import time


class Startup(c.Structure):
    _fields_ = [('cb', w.DWORD), ('reserved', w.LPWSTR), ('desktop', w.LPWSTR),
                ('title', w.LPWSTR), ('x', w.DWORD), ('y', w.DWORD),
                ('width', w.DWORD), ('height', w.DWORD), ('chars_x', w.DWORD),
                ('chars_y', w.DWORD), ('fill', w.DWORD), ('flags', w.DWORD),
                ('show', w.WORD), ('reserved_size', w.WORD), ('reserved_bytes', c.c_void_p),
                ('stdin', w.HANDLE), ('stdout', w.HANDLE), ('stderr', w.HANDLE)]


class Process(c.Structure):
    _fields_ = [('process', w.HANDLE), ('thread', w.HANDLE), ('pid', w.DWORD), ('tid', w.DWORD)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test-exe', type=Path, required=True)
    parser.add_argument('--test', choices=['tray', 'startup', 'takeover', 'collision', 'companion-collision', 'show-desktop'], default='tray')
    args = parser.parse_args()
    executable = args.test_exe.resolve(strict=True)
    root = Path(__file__).resolve().parents[1]
    folder = root / 'target/test-reports/private-tray' / str(time.time_ns())
    folder.mkdir(parents=True)
    user = c.WinDLL('user32', use_last_error=True)
    kernel = c.WinDLL('kernel32', use_last_error=True)
    user.CreateDesktopW.argtypes = [w.LPCWSTR, w.LPCWSTR, c.c_void_p, w.DWORD, w.DWORD, c.c_void_p]
    user.CreateDesktopW.restype = w.HANDLE
    user.CloseDesktop.argtypes = [w.HANDLE]
    kernel.CreateProcessW.argtypes = [w.LPCWSTR, w.LPWSTR, c.c_void_p, c.c_void_p,
                                     w.BOOL, w.DWORD, c.c_void_p, w.LPCWSTR,
                                     c.POINTER(Startup), c.POINTER(Process)]
    kernel.CreateProcessW.restype = w.BOOL
    kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
    kernel.GetExitCodeProcess.argtypes = [w.HANDLE, c.POINTER(w.DWORD)]
    kernel.TerminateProcess.argtypes = [w.HANDLE, w.UINT]
    kernel.CloseHandle.argtypes = [w.HANDLE]
    name = 'minicon-fixture-' + str(time.time_ns())
    desktop = user.CreateDesktopW(name, None, None, 0, 0x01ff, None)
    if not desktop:
        raise c.WinError(c.get_last_error())
    process = Process()
    passed = False
    try:
        environment = {key: value for key, value in os.environ.items() if not key.startswith('MINICON_')}
        environment.update(MINICON_PRIVATE_DESKTOP='1', MINICON_TRAY_HOST='1', MINICON_NO_TASKBAR='1',
                           MINICON_CONFIG=str(folder/'empty.conf'),
                           MINICON_SETTINGS=str(folder/'settings.conf'),
                           MINICON_LOG_DIR=str(folder/'logs'))
        block = c.create_unicode_buffer('\0'.join(f'{k}={v}' for k, v in sorted(environment.items()))+'\0\0')
        environment['LOCALAPPDATA'] = str(folder/'appdata')
        if args.test == 'companion-collision':
            environment['MINICON_TEST_COMPANION']='1'
        if args.test == 'takeover':
            environment.pop('MINICON_TRAY_HOST', None)
        block = c.create_unicode_buffer('\0'.join(f'{k}={v}' for k, v in sorted(environment.items()))+'\0\0')
        test = {'show-desktop': 'taskbar::tests::native_show_desktop_minimizes_apps_without_hiding_shell',
                'tray': 'tray_host::tests::native_public_host_lifecycle',
                'startup': 'window::readiness_tests::native_startup_and_worker_host_transition',
                'takeover': 'shell_swap::tests::native_private_desktop_takeover',
                'collision': 'window::readiness_tests::native_rejects_all_launch_hotkeys_taken',
                'companion-collision': 'window::readiness_tests::native_rejects_all_launch_hotkeys_taken'}[args.test]
        command = c.create_unicode_buffer(subprocess.list2cmdline([
            str(executable), '--ignored', '--exact',
            test, '--nocapture']))
        with open(os.devnull, 'rb') as source, (folder/'fixture.log').open('wb') as output:
            handles = [msvcrt.get_osfhandle(source.fileno()), msvcrt.get_osfhandle(output.fileno())]
            for handle in handles:
                os.set_handle_inheritable(handle, True)
            startup = Startup(cb=c.sizeof(Startup), desktop=name, flags=0x100,
                              stdin=handles[0], stdout=handles[1], stderr=handles[1])
            if not kernel.CreateProcessW(str(executable), command, None, None, True,
                                         0x08000400, block, str(root), c.byref(startup), c.byref(process)):
                raise c.WinError(c.get_last_error())
            if kernel.WaitForSingleObject(process.process, 30000) != 0:
                kernel.TerminateProcess(process.process, 1)
                kernel.WaitForSingleObject(process.process, 5000)
                raise RuntimeError('Private desktop fixture exceeded its deadline')
            code = w.DWORD()
            if not kernel.GetExitCodeProcess(process.process, c.byref(code)) or code.value:
                raise RuntimeError(f'Private desktop fixture failed (exit {code.value}); inspect {folder / "fixture.log"}')
            passed = True
    finally:
        for handle in (process.thread, process.process):
            if handle:
                kernel.CloseHandle(handle)
        user.CloseDesktop(desktop)
        (folder/'summary.json').write_text(json.dumps(dict(passed=passed, private_desktop=True,
                                                          interactive_desktop_activated=False)), encoding='utf-8')
    print(f'Private-desktop {args.test} fixture passed.')


if __name__ == '__main__':
    main()
