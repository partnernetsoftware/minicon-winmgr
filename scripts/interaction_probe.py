"""0.1.8 own-window acceptance: on-demand startup, Run adoption, focus return."""
import ctypes as c
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
import settings_probe as p

OUT=p.ROOT/'target/interaction-acceptance'
FIXTURE_TITLE='minicon-winmgr acceptance fixture'

def fixture():
    p.u.CreateWindowExW.argtypes=[p.w.DWORD,p.w.LPCWSTR,p.w.LPCWSTR,p.w.DWORD,c.c_int,c.c_int,c.c_int,c.c_int,p.w.HWND,p.w.HMENU,p.w.HINSTANCE,p.w.LPVOID]
    p.u.CreateWindowExW.restype=p.w.HWND
    window=p.u.CreateWindowExW(0x40000,'STATIC',FIXTURE_TITLE,0x10CF0000,80,80,420,160,None,None,None,None)
    assert window
    message=p.w.MSG()
    p.u.GetMessageW.argtypes=[c.POINTER(p.w.MSG),p.w.HWND,p.w.UINT,p.w.UINT]
    p.u.DispatchMessageW.argtypes=[c.POINTER(p.w.MSG)];p.u.DispatchMessageW.restype=c.c_ssize_t
    p.u.TranslateMessage.argtypes=[c.POINTER(p.w.MSG)]
    while p.u.IsWindow(window) and p.u.GetMessageW(c.byref(message),None,0,0)>0:
        p.u.TranslateMessage(c.byref(message));p.u.DispatchMessageW(c.byref(message))


def threads():
    class ENTRY(c.Structure):
        _fields_=[('size',p.w.DWORD),('usage',p.w.DWORD),('tid',p.w.DWORD),('pid',p.w.DWORD),('base',p.w.LONG),('delta',p.w.LONG),('flags',p.w.DWORD)]
    p.k.CreateToolhelp32Snapshot.argtypes=[p.w.DWORD,p.w.DWORD];p.k.CreateToolhelp32Snapshot.restype=p.w.HANDLE
    for name in ['Thread32First','Thread32Next']:
        getattr(p.k,name).argtypes=[p.w.HANDLE,c.POINTER(ENTRY)]
    snap=p.k.CreateToolhelp32Snapshot(4,0)
    if snap==p.w.HANDLE(-1).value:raise OSError('Thread snapshot failed')
    try:
        entry=ENTRY();entry.size=c.sizeof(entry);count=0
        more=p.k.Thread32First(snap,c.byref(entry))
        while more:
            if entry.pid==p.pid(p.message()):count+=1
            more=p.k.Thread32Next(snap,c.byref(entry))
        return count
    finally:p.k.CloseHandle(snap)

def main():
    OUT.mkdir(parents=True,exist_ok=True)
    original=os.environ.copy();env=original.copy()
    for key in ['MINICON_NO_TASKBAR','MINICON_TASKBAR','MINICON_START_MENU','MINICON_HOVER_PREVIEW','MINICON_LANGUAGE','MINICON_THEME_MODE','MINICON_TASKBAR_POSITION','MINICON_TRAY']:
        env.pop(key,None)
    env.update(MINICON_SETTINGS=str(OUT/'settings.conf'),MINICON_CONFIG=str(OUT/'empty.conf'),MINICON_ACCEPTANCE_PATH=str(OUT/'local-paths'),LOCALAPPDATA=str(OUT/'profile'),APPDATA=str(OUT/'appdata'),PROGRAMDATA=str(OUT/'programdata'))
    (OUT/'settings.conf').write_text('ui.language=en\nappearance.mode=light\ntaskbar.menu=false\nbehavior.hover_preview=false\n',encoding='utf-8')
    apps=OUT/'appdata/Microsoft/Windows/Start Menu/Programs';apps.mkdir(parents=True,exist_ok=True)
    (apps/'Minicon Test Editor.lnk').write_bytes(b'acceptance-only; never launched')
    paths=OUT/'local-paths';paths.mkdir(exist_ok=True);(paths/'Alpha document.txt').write_text('acceptance',encoding='utf-8')
    history=OUT/'profile/minicon-winmgr/run-history.json';history.parent.mkdir(parents=True,exist_ok=True)
    candidate='minicon-winmgr --settings'
    history.write_text(json.dumps([candidate]),encoding='utf-8')
    report={'binary_sha256':hashlib.sha256(p.SOURCE.read_bytes()).hexdigest(),'checks':[]}
    shell=p.shell_settings()
    fixture_process=None
    fixture_window=None
    def check(name,condition,detail=None):
        report['checks'].append(dict(name=name,passed=bool(condition),detail=detail))
        assert condition,name
    def resource():return dict(**p.metrics(),threads=threads())
    def run_control(ident):return p.u.GetDlgItem(p.window('minicon_winmgr_run_dialog'),ident)
    # Deliver native edit keyboard messages to our window, never global keys to
    # an application that happened to become foreground during automation.
    report['run_input_method']='native edit WM_KEYDOWN/WM_CHAR; global hotkey coverage remains in full Run regression'
    def run_key(code):
        p.u.SendMessageW(run_control(2001),0x100,code,0)
        p.u.SendMessageW(run_control(2001),0x101,code,0)
    def run_text(value):
        buffer=c.create_unicode_buffer(value)
        p.u.SendMessageW(run_control(2001),0xC,0,c.addressof(buffer))

    try:
        p.stop();shutil.copy2(p.SOURCE,p.LIVE);p.start(env)
        check('cold_start_defers_optional_and_run_windows',not p.window('minicon_winmgr_start_menu') and not p.window('minicon_winmgr_run_dialog') and not p.window('minicon_winmgr_hover_preview'))
        import tomllib
        version=tomllib.loads((p.ROOT/'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
        check('settings_title_ends_with_build_version',p.text(p.panel()).endswith(version))
        report['cold_start']=resource()
        started=time.monotonic();p.u.SendMessageW(p.message(),0x312,3,0)
        run=p.wait(lambda:p.window('minicon_winmgr_run_dialog'));p.wait(lambda:p.u.IsWindowVisible(run))
        report['first_run_ms']=round((time.monotonic()-started)*1000,2)
        p.wait(lambda:p.text(run_control(2001))==candidate)
        check('first_use_loads_isolated_history',p.text(run_control(2001))==candidate)
        report['after_first_run']=resource()
        for language,language_id,use,run_label in [('en',6150,'Use','Run'),('zh',6151,'采用','运行')]:
            p.u.SendMessageW(run,16,0,0);p.show();p.click(language_id);p.saved()
            p.u.SendMessageW(p.message(),0x312,3,0);p.wait(lambda:p.u.IsWindowVisible(run))
            check(language+'_same_window_reused',p.window('minicon_winmgr_run_dialog')==run)
            run_text('');time.sleep(.25)
            p.wait(lambda:p.u.IsWindowVisible(run_control(2006)));run_key(40)
            p.wait(lambda:p.text(run_control(2002))==use)
            check(language+'_empty_input_previews_candidate',p.u.IsWindowEnabled(run_control(2002)) and p.text(run_control(2001))==candidate and p.u.IsWindowVisible(run_control(2006)))
            check(language+'_candidate_hint_visible','Enter' in p.text(run_control(2007)))
            p.capture_settings(OUT/('run_'+language+'_use.png'),run)
            run_key(13);p.wait(lambda:p.text(run_control(2001))==candidate and p.text(run_control(2002))==run_label and not p.text(run_control(2008)))
            check(language+'_empty_status_area_hidden',not p.u.IsWindowVisible(run_control(2008)))
            check(language+'_adoption_keeps_dialog_and_command',p.u.IsWindowVisible(run) and p.text(run_control(2001))==candidate and p.text(run_control(2002))==run_label,dict(visible=bool(p.u.IsWindowVisible(run)),input=p.text(run_control(2001)),button=p.text(run_control(2002)),status=p.text(run_control(2008)),foreground=p.u.GetForegroundWindow()))
            p.capture_settings(OUT/('run_'+language+'_ready.png'),run)
            run_text('')
            for char in 'mini':p.u.SendMessageW(run_control(2001),0x102,ord(char),0)
            p.wait(lambda:p.u.IsWindowVisible(run_control(2006)) and p.u.SendMessageW(run_control(2006),0x18B,0,0)>=2)
            def suggestions():
                result=[]
                for index in range(p.u.SendMessageW(run_control(2006),0x18B,0,0)):
                    buffer=c.create_unicode_buffer(32768);p.u.SendMessageW(run_control(2006),0x189,index,c.addressof(buffer));result.append(buffer.value)
                return result
            items=suggestions()
            check(language+'_typing_shows_history_and_apps',any('Minicon Test Editor' in item for item in items) and any(candidate in item for item in items))
            check(language+'_suggestions_do_not_change_typed_input',p.text(run_control(2001))=='mini')
            run_key(40);run_key(40);p.wait(lambda:p.text(run_control(2002))==use)
            p.capture_settings(OUT/('run_'+language+'_apps.png'),run)
            run_key(13);p.wait(lambda:'Minicon Test Editor.lnk' in p.text(run_control(2001)))
            check(language+'_app_adoption_preserves_quoted_full_path',p.text(run_control(2001)).startswith('"') and p.text(run_control(2001)).endswith('"') and p.u.IsWindowVisible(run))
            p.capture_settings(OUT/('run_'+language+'_app_ready.png'),run)
            p.u.SendMessageW(run,0x312,3,0)
            value=str(paths)+'\\';buffer=c.create_unicode_buffer(value);p.u.SendMessageW(run_control(2001),0xC,0,c.addressof(buffer))
            p.wait(lambda:p.u.IsWindowVisible(run_control(2006)) and any('Alpha document.txt' in item for item in suggestions()))
            check(language+'_typing_shows_local_path',any('Alpha document.txt' in item for item in suggestions()))
            p.capture_settings(OUT/('run_'+language+'_paths.png'),run)
            p.u.SendMessageW(run_control(2006),0x184,0,0)
            value='%MINICON_ACCEPTANCE_PATH%\\';buffer=c.create_unicode_buffer(value);p.u.SendMessageW(run_control(2001),0xC,0,c.addressof(buffer))
            p.wait(lambda:any('Alpha document.txt' in item for item in suggestions()))
            check(language+'_environment_path_suggestions_preserve_input',p.text(run_control(2001))==value)

        p.u.SendMessageW(run,16,0,0);p.show()
        for edge,ident in [('top',6140),('bottom',6141),('left',6142),('right',6143)]:
            p.click(ident);p.saved();p.activate()
            bar=p.window('minicon_winmgr_taskbar')
            check(edge+'_settings_foreground',p.u.GetForegroundWindow()==p.panel())
            p.u.SendMessageW(bar,0x312,1,0)
            check(edge+'_keyboard_summons_bar',p.u.GetForegroundWindow()==bar)
            p.u.SendMessageW(bar,0x100,0x23,0);p.u.SendMessageW(bar,0x100,0x24,0)
            check(edge+'_home_end_keep_navigation_focus',p.u.GetForegroundWindow()==bar)
            p.u.SendMessageW(bar,0x100,27,0)
            p.wait(lambda:p.u.GetForegroundWindow()==p.panel())
            check(edge+'_escape_returns_previous_window',True)
        p.click(6101);p.saved()
        check('saved_visibility_explicitly_reports_restart','重启' in p.text(p.ctrl(6112)))
        p.click(6101);p.saved()
        p.stop();env['MINICON_START_MENU']='1';p.start(env)
        check('explicit_legacy_menu_opt_in_initializes',bool(p.window('minicon_winmgr_start_menu')))
        check('run_stays_deferred_with_menu_opt_in',not p.window('minicon_winmgr_run_dialog'))
        p.stop();env.pop('MINICON_START_MENU',None)
        fixture_process=subprocess.Popen([sys.executable,str(Path(__file__).resolve()),'--fixture'],creationflags=subprocess.CREATE_NO_WINDOW)
        fixture_window=p.wait(lambda:p.u.FindWindowW('STATIC',FIXTURE_TITLE))
        p.key('ALT');p.u.SetForegroundWindow(fixture_window);p.wait(lambda:p.u.GetForegroundWindow()==fixture_window)
        p.start(env)
        p.show();p.click(6141);p.saved()
        bar=p.window('minicon_winmgr_taskbar')
        p.u.SendMessageW(bar,0x312,1,0);p.u.SendMessageW(bar,0x100,0x24,0);p.u.SendMessageW(bar,0x100,32,0)
        p.wait(lambda:p.u.GetForegroundWindow()==fixture_window)
        check('home_space_activates_fixture_without_minimizing',True)
        p.u.SendMessageW(bar,0x312,1,0);p.u.SendMessageW(bar,0x100,27,0)
        p.wait(lambda:p.u.GetForegroundWindow()==fixture_window)
        check('escape_restores_external_fixture',True)
        report['passed']=True
    except Exception as error:
        report['passed']=False;report['error']=repr(error)
        if p.window('minicon_winmgr_run_dialog'):
            report['run_failure_state']={'input':p.text(run_control(2001)),'visible':bool(p.u.IsWindowVisible(p.window('minicon_winmgr_run_dialog'))),'status':p.text(run_control(2008)),'count':p.u.SendMessageW(run_control(2006),0x18B,0,0),'foreground':p.u.GetForegroundWindow()}
            p.capture_settings(OUT/'failure.png',p.window('minicon_winmgr_run_dialog'))
        raise
    finally:
        if fixture_window:p.u.PostMessageW(fixture_window,16,0,0)
        if fixture_process:
            try:fixture_process.wait(timeout=5)
            except subprocess.TimeoutExpired:fixture_process.terminate();fixture_process.wait(timeout=5)
        p.stop();p.start(original)
        report['normal_session_restored']=bool(p.window('minicon_winmgr_taskbar'))
        report['shell_registration_unchanged']=p.shell_settings()==shell
        (OUT/'summary.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps(report,ensure_ascii=True,indent=2))

if __name__=='__main__':
    if '--fixture' in sys.argv:fixture()
    else:raise SystemExit("Use scripts/test.py --layer interaction --restore-exe <normal-running-exe> for isolated desktop checks.")
