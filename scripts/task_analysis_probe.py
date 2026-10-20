"""Focused native human/agent analysis path, with an owned run and private store."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import subprocess
import time
import settings_probe as p
import scenario_probe as s


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate', type=Path, required=True)
    args = parser.parse_args()
    exe = args.candidate.resolve(strict=True)
    folder = p.ROOT/'target/test-reports/task-analysis'/str(time.time_ns())
    folder.mkdir(parents=True)
    store = folder/'notes'
    report = dict(passed=False, binary_sha256=s.digest(exe))
    p.u.EnumWindows.argtypes = [c.c_void_p, w.LPARAM]
    p.k.GetProcessTimes.argtypes = [w.HANDLE] + [c.POINTER(w.FILETIME)]*4
    handle = p.k.OpenProcess(0x1000, False, os.getpid())
    times = [w.FILETIME() for _ in range(4)]
    assert p.k.GetProcessTimes(handle, *[c.byref(t) for t in times])
    p.k.CloseHandle(handle)
    target = dict(pid=os.getpid(), created_100ns=times[0].dwLowDateTime+(times[0].dwHighDateTime << 32))
    original = p.u.GetForegroundWindow()
    shell = p.pid(p.message())
    tool = None
    hwnd = None
    editor = None

    def window(pid, title):
        found = []
        cb = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)(lambda h, _:
            (found.append(h) if p.pid(h) == pid and p.u.IsWindowVisible(h)
             and title in p.text(h) else None) or True)
        p.u.EnumWindows(cb, 0)
        return next(iter(found), None)

    def ctl(h, id): return p.u.GetDlgItem(h, id)
    def txt(h, id): return p.text(ctl(h, id))
    def set_text(h, id, value):
        value = c.create_unicode_buffer(value)
        p.u.SendMessageW(ctl(h, id), 12, 0, c.addressof(value))
    def click(h, id):
        p.wait(lambda: p.u.IsWindowEnabled(ctl(h, id)))
        p.u.PostMessageW(ctl(h, id), 0xf5, 0, 0)
    def ready(): return p.u.IsWindowEnabled(ctl(editor, 2105))
    def rpc(request, ok=True):
        child = subprocess.run([str(exe), '--notes-json', '--store', str(store)],
            input=json.dumps(request, ensure_ascii=False).encode('utf-8'),
            capture_output=True, timeout=15, creationflags=subprocess.CREATE_NO_WINDOW)
        response = json.loads(child.stdout)
        assert response['ok'] == ok, response
        assert child.returncode == (0 if ok else 1)
        return response.get('data') if ok else response['error']
    def launch():
        child = subprocess.Popen([str(exe), '--language', 'en', '--theme', 'dark',
            '--notes-store', str(store), '--focus-pid', str(target['pid']),
            '--focus-created', str(target['created_100ns']), '--search', str(target['pid'])],
            creationflags=subprocess.CREATE_NO_WINDOW)
        h = p.wait(lambda: window(child.pid, 'minicon-tskmgr'), timeout=15)
        p.wait(lambda: str(target['pid']) in txt(h, 1108), timeout=18)
        return child, h

    try:
        tool, hwnd = launch()
        assert p.u.SendMessageW(ctl(hwnd, 1104), 0xf0, 0, 0) == 1
        assert p.u.SendMessageW(ctl(hwnd, 1107), 0x1004, 0, 0) >= 2, 'Search lost ancestor context'
        click(hwnd, 1119)
        editor = p.wait(lambda: window(tool.pid, 'Task analysis'))
        p.wait(ready)
        assert p.u.SendMessageW(ctl(editor, 2110), 0x147, 0, 0) == 0
        set_text(editor, 2102, 'Human observation — 测试记录\r\nInvestigate owned run.')
        set_text(editor, 2103, 'Owned fixture; no external process inspection')
        p.u.SendMessageW(ctl(editor, 2104), 0x14e, 2, 0)
        click(editor, 2105)
        p.wait(lambda: ready() and 'Saved.' in txt(editor, 2108))
        first = rpc(dict(op='read', target=target))
        assert first['flag'] == 'watching' and first['entries'][0]['author']['kind'] == 'human'
        second = rpc(dict(op='append', target=target, expected_revision=first['revision'],
            author='fixture-agent', text='Hypothesis: needs investigation.',
            evidence='Fixture evidence only', flag='suspicious'))
        assert second['flag'] == 'suspicious'
        set_text(editor, 2102, 'Human review draft remains after conflict.')
        click(editor, 2105)
        p.wait(lambda: ready() and 'conflict' in txt(editor, 2108))
        assert 'review draft' in txt(editor, 2102)
        click(editor, 2106)
        p.wait(lambda: ready() and 'History loaded' in txt(editor, 2108))
        assert 'fixture-agent' in txt(editor, 2101) and 'review draft' in txt(editor, 2102)
        p.u.SendMessageW(ctl(editor, 2104), 0x14e, 4, 0)
        click(editor, 2105)
        p.wait(lambda: ready() and 'Saved.' in txt(editor, 2108))
        final = rpc(dict(op='read', target=target))
        assert final['flag'] == 'resolved' and len(final['entries']) == 3
        assert rpc(dict(op='append', target=target, expected_revision=first['revision'],
            author='stale-agent', text='Must fail'), ok=False)['code'] == 'conflict'
        p.capture_settings(folder/'analysis.png', editor)
        click(editor, 2107)
        p.wait(lambda: not p.u.IsWindow(editor))
        editor = None
        set_text(hwnd, 1101, 'status:resolved')
        p.wait(lambda: p.u.SendMessageW(ctl(hwnd, 1107), 0x1004, 0, 0) >= 2)
        assert str(target['pid']) in txt(hwnd, 1108)
        p.capture_settings(folder/'tree-resolved.png', hwnd)
        p.u.PostMessageW(hwnd, 16, 0, 0)
        assert tool.wait(timeout=8) == 0
        tool, hwnd = launch()
        click(hwnd, 1119)
        editor = p.wait(lambda: window(tool.pid, 'Task analysis'))
        p.wait(lambda: ready() and 'fixture-agent' in txt(editor, 2101))
        assert 'resolved' in txt(editor, 2101)
        assert rpc(dict(op='read', subject=final['subject']))['revision'] == final['revision']
        # Application notes are a distinct thread; changing scope must not erase run history.
        assert p.u.SendMessageW(ctl(editor, 2110), 0x146, 0, 0) == 2
        p.u.SendMessageW(ctl(editor, 2110), 0x14e, 1, 0)
        p.u.SendMessageW(editor, 0x111, 2110 | (1 << 16), ctl(editor, 2110))
        p.wait(lambda: ready() and 'Current state: unreviewed' in txt(editor, 2101))
        set_text(editor, 2102, 'Application purpose, retained across runs.')
        click(editor, 2105)
        p.wait(lambda: ready() and 'Saved.' in txt(editor, 2108))
        application = rpc(dict(op='read', target=target, scope='application'))
        assert application['subject']['image'] and application['subject']['run'] is None
        assert len(application['entries']) == 1
        assert rpc(dict(op='read', target=target))['revision'] == final['revision']
        p.u.SendMessageW(ctl(editor, 2110), 0x14e, 0, 0)
        p.u.SendMessageW(editor, 0x111, 2110 | (1 << 16), ctl(editor, 2110))
        p.wait(lambda: ready() and 'fixture-agent' in txt(editor, 2101))
        click(editor, 2107)
        p.wait(lambda: not p.u.IsWindow(editor))
        editor = None
        report.update(passed=True, default_tree_search_context=True,
            human_agent_append_and_states=True, stale_revision_preserves_draft=True,
            native_restart_history=True, agent_history_read=True, separate_application_scope=True)
    except BaseException as error:
        report['error'] = repr(error)
        if editor and p.u.IsWindow(editor):
            report['editor_status'] = txt(editor, 2108)
        raise
    finally:
        if editor and p.u.IsWindow(editor):
            # Only our draft, store and process are involved in failure cleanup.
            set_text(editor, 2102, '')
        if tool and tool.poll() is None:
            if hwnd: p.u.PostMessageW(hwnd, 16, 0, 0)
            try: tool.wait(timeout=3)
            except subprocess.TimeoutExpired:
                tool.kill(); tool.wait()
                report['forced_owned_tool_cleanup'] = True
                report['passed'] = False
        if original and p.u.IsWindow(original): p.u.SetForegroundWindow(original)
        report['original_winmgr_unchanged'] = p.pid(p.message()) == shell
        (folder/'summary.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(json.dumps(report), flush=True)


if __name__ == '__main__':
    main()
