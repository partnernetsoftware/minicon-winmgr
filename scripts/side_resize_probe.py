"""Own-taskbar side-resize acceptance; restore the original live binary/config."""
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
OUT = ROOT / 'target/test-reports/side-resize' / str(time.time_ns())


def main():
    OUT.mkdir(parents=True)
    original = os.environ.copy()
    environment = original.copy()
    for name in ['MINICON_NO_TASKBAR', 'MINICON_TASKBAR', 'MINICON_TASKBAR_POSITION',
                 'MINICON_LANGUAGE', 'MINICON_HOVER_PREVIEW', 'MINICON_THEME_MODE', 'MINICON_SETTINGS']:
        environment.pop(name, None)
    profile = OUT / 'profile'
    profile.mkdir()
    config = profile / 'winmgr.cnf'
    config.write_text('ui.language=en\ntaskbar.position=left\nbehavior.hover_preview=false\n', encoding='utf-8')
    environment.update(USERPROFILE=str(profile), MINICON_CONFIG=str(OUT/'empty.conf'))
    backup = OUT / 'original-live.exe'
    shutil.copy2(p.LIVE, backup)
    with backup.open('rb') as source:
        original_hash = hashlib.file_digest(source, 'sha256').hexdigest()
    shell = p.shell_settings()
    cursor = w.POINT()
    p.u.GetCursorPos(c.byref(cursor))
    with p.SOURCE.open('rb') as source:
        binary_hash = hashlib.file_digest(source, 'sha256').hexdigest()
    report = {'checks': [], 'passed': False, 'limits': ['Current physical display and DPI only.'],
              'binary_sha256': binary_hash}
    p.u.GetDpiForWindow.argtypes = [w.HWND]; p.u.GetDpiForWindow.restype = w.UINT
    p.u.SetCursorPos.argtypes = [c.c_int, c.c_int]; p.u.SetCursorPos.restype = w.BOOL

    def check(name, condition, detail=None):
        report['checks'].append({'name': name, 'passed': bool(condition), 'detail': detail})
        assert condition, name

    def bar():
        return p.window('minicon_winmgr_taskbar')

    def width():
        rect = p.rect(bar()); return rect[2]-rect[0]

    def preference():
        for line in config.read_text(encoding='utf-8').splitlines():
            if line.split('=',1)[0].strip() == 'taskbar.side_width':
                return int(line.split('=',1)[1].strip())
        return None

    def work_area():
        rect = w.RECT()
        assert p.u.SystemParametersInfoW(0x0030, 0, c.byref(rect), 0)
        return [rect.left, rect.top, rect.right, rect.bottom]

    def mouse(flags):
        value = p.INPUT(0, p.UNION(mouse=p.MOUSE(0, 0, 0, flags, 0, 0)))
        assert p.u.SendInput(1, c.byref(value), c.sizeof(value)) == 1

    def drag_start(edge, target_pixels):
        rect = p.rect(bar())
        x = rect[2]-2 if edge == 'left' else rect[0]+2
        y = rect[1]+min(120, (rect[3]-rect[1])//2)
        assert p.u.SetCursorPos(x,y)
        time.sleep(.05)
        mouse(2); time.sleep(.06)
        target_x = rect[0]+target_pixels if edge == 'left' else rect[2]-target_pixels
        assert p.u.SetCursorPos(target_x,y)
        time.sleep(.15)

    try:
        p.stop()
        baseline_work = work_area()
        report['baseline_work_area'] = baseline_work
        shutil.copy2(p.SOURCE, p.LIVE)
        p.start(environment, ['--settings']); p.show()
        report['initial_work_area'] = work_area()
        p.wait(lambda: bar() and width() > 0)
        dpi = p.u.GetDpiForWindow(bar()) or 96
        scaled = lambda value: (value*dpi+48)//96
        check('default_side_width_180_dip', abs(width()-scaled(180)) <= 1, width())
        drag_start('left', scaled(260))
        check('left_drag_previews_width', abs(width()-scaled(260)) <= 2, width())
        check('drag_does_not_persist_each_pixel', preference() is None)
        mouse(4)
        p.wait(lambda: preference() == 260)
        p.wait(lambda: work_area()[0] == baseline_work[0]+width())
        check('left_width_saved_once_on_release', abs(width()-scaled(260)) <= 2)
        check('left_resize_updates_reserved_work_area', work_area()[0] == baseline_work[0]+width(),
              {'baseline': baseline_work, 'actual': work_area(), 'width': width()})
        drag_start('left', scaled(340))
        p.u.PostMessageW(bar(), 0x001F, 0, 0)  # WM_CANCELMODE, own taskbar only.
        p.wait(lambda: abs(width()-scaled(260)) <= 2)
        # Release over the owned bar after cancelling capture.
        rect = p.rect(bar()); p.u.SetCursorPos(rect[2]-2,rect[1]+120); mouse(4)
        check('cancel_restores_width_without_saving', preference() == 260)
        check('cancel_preserves_work_area', work_area()[0] == baseline_work[0]+width())
        drag_start('left', scaled(340))
        escape = p.INPUT(1, p.UNION(key=p.KEY(27,0,0,0,0)))
        assert p.u.SendInput(1,c.byref(escape),c.sizeof(escape)) == 1
        time.sleep(.08)
        escape = p.INPUT(1, p.UNION(key=p.KEY(27,0,2,0,0)))
        assert p.u.SendInput(1,c.byref(escape),c.sizeof(escape)) == 1
        p.wait(lambda: abs(width()-scaled(260)) <= 2)
        rect = p.rect(bar()); p.u.SetCursorPos(rect[2]-2,rect[1]+120); mouse(4)
        check('escape_cancels_drag_and_preserves_settings_window',
              preference() == 260 and p.u.IsWindowVisible(p.panel()))
        p.click(6143); p.saved()
        p.wait(lambda: p.checked(6143) and abs(width()-scaled(260)) <= 2)
        drag_start('right', scaled(320)); mouse(4)
        p.wait(lambda: preference() == 320)
        p.wait(lambda: work_area()[0] == baseline_work[0] and work_area()[2] == baseline_work[2]-width())
        check('right_inner_edge_drags_in_reverse_direction', abs(width()-scaled(320)) <= 2, width())
        check('right_resize_restores_old_edge_and_reserves_new_width',
              work_area()[0] == baseline_work[0] and work_area()[2] == baseline_work[2]-width())
        drag_start('right', scaled(80))
        check('minimum_width_is_enforced_while_dragging', abs(width()-scaled(160)) <= 2)
        mouse(4); p.wait(lambda: preference() == 160)
        drag_start('right', scaled(10000))
        expected_max = min(scaled(480), (baseline_work[2]-baseline_work[0])//3)
        check('maximum_width_leaves_desktop_space', abs(width()-expected_max) <= 2, width())
        mouse(4); time.sleep(.1)
        drag_start('right', scaled(320)); mouse(4)
        p.wait(lambda: preference() == 320)
        p.stop(); p.start(environment)
        p.wait(lambda: bar() and abs(width()-scaled(320)) <= 2)
        check('no_argument_start_restores_home_preferences', preference() == 320)
        p.show()
        p.wait(lambda: p.checked(6143) and abs(width()-scaled(320)) <= 2)
        check('restart_preserves_side_and_width', preference() == 320)
        p.key('ESC')
        p.wait(lambda: not p.u.IsWindowVisible(p.panel()))
        check('resize_does_not_leave_escape_registered', preference() == 320)
        p.show()
        check('shell_registration_unchanged', p.shell_settings() == shell)
        report['passed'] = True
    finally:
        mouse(4)
        p.stop()
        report['exit_restores_work_area'] = 'baseline_work' in locals() and work_area() == baseline_work
        shutil.copy2(backup,p.LIVE)
        p.start(original)
        p.u.SetCursorPos(cursor.x,cursor.y)
        report['normal_session_restored'] = bool(p.wait(bar))
        with p.LIVE.open('rb') as live:
            report['original_binary_restored'] = hashlib.file_digest(live,'sha256').hexdigest() == original_hash
        report['shell_registration_unchanged'] = p.shell_settings() == shell
        report['passed'] = report['passed'] and all(report[key] for key in
            ['normal_session_restored', 'original_binary_restored',
             'shell_registration_unchanged', 'exit_restores_work_area'])
        (OUT/'summary.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
        print(json.dumps({'passed': report['passed'], 'checks': len(report['checks']),
                          'normal_session_restored': report['normal_session_restored']}),flush=True)
        if not report['passed']:
            raise RuntimeError('Side-resize acceptance or restoration failed; inspect the local report')
    return report


if __name__ == '__main__':
    main()
