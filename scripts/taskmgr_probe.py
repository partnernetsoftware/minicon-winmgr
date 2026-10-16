"""Existing current-desktop Task Manager only: no launch, elevation or global process stop."""
import ctypes as c
import hashlib
import json
import os
import shutil
import time
import settings_probe as p

OUT=p.ROOT/'target/taskmgr-acceptance'

def elevated(pid):
    api=c.WinDLL('advapi32',use_last_error=True)
    api.OpenProcessToken.argtypes=[p.w.HANDLE,p.w.DWORD,c.POINTER(p.w.HANDLE)]
    api.GetTokenInformation.argtypes=[p.w.HANDLE,c.c_int,c.c_void_p,p.w.DWORD,c.POINTER(p.w.DWORD)]
    process=p.k.OpenProcess(0x1000,False,pid)
    if not process:return None
    token=p.w.HANDLE()
    try:
        if not api.OpenProcessToken(process,8,c.byref(token)):return None
        value=p.w.DWORD();size=p.w.DWORD()
        if not api.GetTokenInformation(token,20,c.byref(value),c.sizeof(value),c.byref(size)):return None
        return bool(value.value)
    finally:
        if token:p.k.CloseHandle(token)
        p.k.CloseHandle(process)

def main():
    OUT.mkdir(exist_ok=True)
    target=p.window('TaskManagerWindow')
    report={'binary_sha256':hashlib.sha256(p.SOURCE.read_bytes()).hexdigest(),'checks':[],'covered':bool(target)}
    if not target:
        report.update(passed=True,skipped='No existing Task Manager on this desktop; never auto-launched.')
        (OUT/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8');return
    p.u.IsIconic.argtypes=[p.w.HWND];p.u.IsIconic.restype=p.w.BOOL
    p.u.SetCursorPos.argtypes=[c.c_int,c.c_int]
    before={'iconic':bool(p.u.IsIconic(target)),'foreground':p.u.GetForegroundWindow()}
    original=os.environ.copy();env=original.copy()
    for key in ['MINICON_NO_TASKBAR','MINICON_TASKBAR','MINICON_START_MENU','MINICON_TASKBAR_POSITION','MINICON_GROUP_TASKS','MINICON_GROUP_BY']:
        env.pop(key,None)
    config=OUT/'settings.conf';config.write_text('taskbar.menu=false\ntaskbar.group=never\ntaskbar.position=bottom\n',encoding='utf-8')
    pins=OUT/'empty-pins.txt';pins.write_text('',encoding='utf-8')
    env.update(MINICON_SETTINGS=str(config),MINICON_CONFIG=str(OUT/'empty.conf'),MINICON_QUICKLAUNCH_FILE=str(pins),MINICON_THEME_MODE='light',RUST_LOG='debug')
    shell=p.shell_settings()
    def check(name,value):
        report['checks'].append(dict(name=name,passed=bool(value)));assert value,name
    def click_task():
        # Deliver exact native button messages; this never clicks another app.
        bar=p.window('minicon_winmgr_taskbar');r=p.rect(bar)
        x=report['task_button_x'];y=(r[3]-r[1])//2
        point=x|(y<<16)
        report['last_click']={'client_point':[x,y],'bar':bar,'foreground':p.u.GetForegroundWindow(),'method':'native taskbar messages; keyboard focus before restore'}
        p.u.SendMessageW(bar,0x200,0,point)
        p.u.SendMessageW(bar,0x201,1,point)
        p.u.SendMessageW(bar,0x202,0,point)
        time.sleep(.08)
    try:
        p.stop();shutil.copy2(p.SOURCE,p.LIVE)
        p.u.PostMessageW(target,0x112,0xF020,0);p.wait(lambda:p.u.IsIconic(target))
        p.u.PostMessageW(target,0x112,0xF120,0);p.wait(lambda:not p.u.IsIconic(target))
        if p.u.GetForegroundWindow()!=target:p.key('ALT');p.u.SetForegroundWindow(target)
        p.wait(lambda:p.u.GetForegroundWindow()==target)
        # First snapshot starts with the foreground Task Manager; no pins in this config.
        p.start(env)
        p.wait(lambda:p.u.GetForegroundWindow()==target)
        p.u.SetCursorPos(5,5);time.sleep(.2)
        p.capture_settings(OUT/'active_bar.png',p.window('minicon_winmgr_taskbar'))
        from PIL import Image
        image=Image.open(OUT/'active_bar.png')
        markers=[x for x in range(40,image.width-130) if image.getpixel((x,7))==(154,95,52) and image.getpixel((x,28))==(154,95,52)]
        assert markers,'Task Manager active marker was not found; do not click an unverified button.'
        report['task_button_x']=markers[0]+20
        report['privilege']={'winmgr_elevated':elevated(p.pid(p.message())),'taskmgr_elevated':elevated(p.pid(target))}
        check('winmgr_remains_standard_user',report['privilege']['winmgr_elevated'] is False)
        p.u.PostMessageW(target,0x112,0xF020,0);p.wait(lambda:p.u.IsIconic(target))
        for iteration in range(3):
            # Synthetic messages do not grant normal mouse foreground rights.
            # Focus our shell first through its existing settings/keyboard path.
            p.show()
            bar=p.window('minicon_winmgr_taskbar')
            p.u.SendMessageW(bar,0x312,1,0)
            p.wait(lambda:p.u.GetForegroundWindow()==bar)
            click_task();p.wait(lambda:not p.u.IsIconic(target) and p.u.GetForegroundWindow()==target)
            check(f'click_{iteration}_restores_and_activates',True)
            click_task();p.wait(lambda:p.u.IsIconic(target))
            check(f'click_{iteration}_minimizes_active_taskmgr',True)
        check('same_task_manager_instance',p.u.IsWindow(target))
        report['passed']=True
    except Exception as error:
        report.update(passed=False,error=repr(error),state={'taskmgr_iconic':bool(p.u.IsIconic(target)),'foreground':p.u.GetForegroundWindow()});raise
    finally:
        p.stop()
        if bool(p.u.IsIconic(target))!=before['iconic']:
            p.u.PostMessageW(target,0x112,0xF020 if before['iconic'] else 0xF120,0)
            p.wait(lambda:bool(p.u.IsIconic(target))==before['iconic'])
        p.u.SetForegroundWindow(before['foreground']);p.start(original)
        report['normal_session_restored']=bool(p.window('minicon_winmgr_taskbar'))
        report['shell_registration_unchanged']=p.shell_settings()==shell
        (OUT/'summary.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps(report,ensure_ascii=True,indent=2))

if __name__=='__main__':main()
