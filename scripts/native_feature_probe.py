"""Actual application diagnostics and DWM hover; owned fixture, isolated shell."""
import ctypes as c
from ctypes import wintypes as w
import json
import os
import shutil
import time
import settings_probe as p
import scenario_probe as s


def main():
    desktop=s.Desktop()
    folder=s.ROOT/'target/test-reports/native-features'/str(time.time_ns())
    folder.mkdir(parents=True)
    env={key:value for key,value in os.environ.items() if not key.startswith('MINICON_')}
    env.update(MINICON_SETTINGS=str(folder/'settings.conf'),MINICON_CONFIG=str(folder/'empty.conf'),
               MINICON_QUICKLAUNCH_FILE=str(folder/'pins.txt'),MINICON_LOG_DIR=str(folder/'logs'))
    (folder/'settings.conf').write_text('ui.language=en\nappearance.mode=dark\ntaskbar.position=bottom\ntaskbar.menu=false\nbehavior.hover_preview=true\n',encoding='utf-8')
    tests=[];fixture=None;has_record=False;failure=None
    cursor=w.POINT();p.u.GetCursorPos(c.byref(cursor))
    p.u.SetCursorPos.argtypes=[c.c_int,c.c_int]
    p.u.ShowWindow.argtypes=[w.HWND,c.c_int]
    p.u.SetWindowPos.argtypes=[w.HWND,w.HWND,c.c_int,c.c_int,c.c_int,c.c_int,w.UINT]
    p.u.RedrawWindow.argtypes=[w.HWND,c.c_void_p,w.HANDLE,w.UINT]
    def check(name,value):
        tests.append(dict(name=name,passed=bool(value)))
        if not value:raise AssertionError(name)
    def wait(seconds):
        end=time.monotonic()+seconds
        while time.monotonic()<end:desktop.pump();time.sleep(.01)
    def until(predicate,timeout=8):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            desktop.pump()
            value=predicate()
            if value:return value
            time.sleep(.02)
        raise AssertionError('Native feature condition timed out')
    def click(window,ident):desktop.send(p.u.GetDlgItem(window,ident),0xF5,pid=p.pid(p.message()))
    def capture(path,window):
        nonlocal failure
        # Like the settings/theme probes, capture after the new text/theme has
        # been painted, rather than racing the final control update.
        if not p.u.RedrawWindow(window,None,None,0x185):raise RuntimeError('Native repaint failed')
        wait(.15)
        try:p.capture_settings(path,window)
        except BaseException as error:
            failure=dict(error=str(error),window_valid=bool(p.u.IsWindow(window)),
                visible=bool(p.u.IsWindowVisible(window)),window_pid=p.pid(window),
                app_pid=p.pid(p.message()),last_error=c.get_last_error(),resources=p.metrics())
            raise
    def hover():
        bar=p.window('minicon_winmgr_taskbar');rect=p.rect(bar)
        # No pins or menu. The topmost fixture was present before enumeration,
        # so the first stable task button is the owned STATIC fixture.
        moved=bool(p.u.SetCursorPos(rect[0]+70,rect[1]+18))
        try:
            if not moved:raise RuntimeError("Input desktop refused SetCursorPos; physical hover cannot be exercised")
            return until(lambda:(h:=p.window('minicon_winmgr_hover_preview')) and p.u.IsWindowVisible(h) and h)
        except BaseException:
            position=w.POINT();p.u.GetCursorPos(c.byref(position))
            context=dict(cursor_move_succeeded=moved,initial_cursor=[cursor.x,cursor.y],cursor=[position.x,position.y],bar_rect=list(rect),
                         fixture_visible=bool(p.u.IsWindowVisible(fixture)),fixture_rect=list(p.rect(fixture)),
                         preview_exists=bool(p.window('minicon_winmgr_hover_preview')),metrics=p.metrics())
            (folder/'hover-context.json').write_text(json.dumps(context,indent=2),encoding='utf-8')
            capture(folder/'hover-bar.png',bar)
            raise
    try:
        p.stop()
        fixture=desktop.u.CreateWindowExW(0,'STATIC','WINMGR PREVIEW FIXTURE',
            0x10CF0000,300,150,500,350,None,None,None,None)
        if not fixture:raise RuntimeError('Could not create preview fixture')
        p.u.SetWindowPos(fixture,w.HWND(-1),0,0,0,0,0x13)
        wait(.15)
        shutil.copy2(p.SOURCE,p.LIVE);p.start(env)
        for language,language_id in [('en',6150),('zh',6151)]:
            p.show();p.click(language_id);p.saved()
            for theme,theme_id in [('light',6130),('dark',6131)]:
                p.click(theme_id);p.saved();p.click(6120)
                info=until(lambda:p.window('minicon_winmgr_settings_info'))
                click(info,3)
                diagnostic=until(lambda:(h:=p.window('minicon_winmgr_diagnostics')) and p.u.IsWindowVisible(h) and h)
                check(language+'_'+theme+'_real_diagnostic_entry',p.pid(diagnostic)==p.pid(p.message()))
                check(language+'_'+theme+'_initially_idle',p.u.IsWindowEnabled(p.u.GetDlgItem(diagnostic,2)) and not p.u.IsWindowEnabled(p.u.GetDlgItem(diagnostic,3)))
                p.u.PostMessageW(p.u.GetDlgItem(diagnostic,2),0xF5,0,0)
                if has_record:
                    from diagnostics_probe import find
                    confirmation=until(lambda:find(p.pid(p.message()),'#32770'))
                    check(language+'_'+theme+'_replacement_defaults_to_no',
                          (p.u.SendMessageW(confirmation,0x400,0,0)&0xffff)==7)
                    p.u.PostMessageW(confirmation,0x111,6,0)
                until(lambda:p.u.IsWindowEnabled(p.u.GetDlgItem(diagnostic,3)))
                check(language+'_'+theme+'_recording_controls',p.u.IsWindowEnabled(p.u.GetDlgItem(diagnostic,3)) and not p.u.IsWindowEnabled(p.u.GetDlgItem(diagnostic,2)))
                wait(5.3)
                click(diagnostic,3)
                has_record=True
                check(language+'_'+theme+'_stopped_summary_exportable',p.u.IsWindowEnabled(p.u.GetDlgItem(diagnostic,4)))
                capture(folder/(language+'-'+theme+'-diagnostics.png'),diagnostic)
                click(diagnostic,5)
                check(language+'_'+theme+'_close_hides_diagnostics',not p.u.IsWindowVisible(diagnostic))
                desktop.send(info,16,pid=p.pid(p.message()))
            desktop.send(p.panel(),16,pid=p.pid(p.message()))
        # Keep real pointer injection on the owned test desktop surface after
        # closing diagnostic windows, rather than an unrelated remote client.
        p.key('ALT')
        p.u.SetForegroundWindow(fixture)
        until(lambda:p.u.GetForegroundWindow()==fixture)
        preview=hover();wait(.25)
        capture(folder/'preview.png',preview)
        from PIL import Image
        pixels=list(Image.open(folder/'preview.png').convert('RGB').getdata())
        check('actual_composed_fixture_surface',sum(min(pixel)>200 for pixel in pixels)>len(pixels)*.35)
        p.u.SetCursorPos(20,100);until(lambda:not p.u.IsWindowVisible(preview))
        check('leaving_button_hides_preview',True)
        p.u.ShowWindow(fixture,6);wait(.25)
        hover();check('minimized_source_keeps_ui_responsive',bool(desktop.send(p.message(),s.READY,pid=p.pid(p.message()))))
        p.u.ShowWindow(fixture,9);wait(.25)
        p.u.SetCursorPos(20,100);wait(.1);hover()
        desktop.u.DestroyWindow(fixture);fixture=None
        until(lambda:not p.u.IsWindowVisible(preview))
        check('destroyed_source_clears_preview',True)
    except BaseException as error:
        if failure is None:failure=dict(error=repr(error))
        raise
    finally:
        p.u.SetCursorPos(cursor.x,cursor.y)
        if fixture:desktop.u.DestroyWindow(fixture)
        desktop.pump()
        p.stop();p.start(os.environ.copy())
        (folder/'summary.json').write_text(json.dumps(dict(tests=tests,failure=failure,binary_sha256=s.digest(p.SOURCE)),indent=2),encoding='utf-8')
    print(f'Actual diagnostic/preview features: {len(tests)} checks passed.',flush=True)
