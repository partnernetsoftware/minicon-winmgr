"""Native diagnostic UI on owned test windows; never replaces the live shell."""
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import settings_probe as p

ROOT = Path(__file__).resolve().parents[1]
ENUM = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)


def find(pid, class_name):
    found = []
    @ENUM
    def visit(hwnd, _):
        if p.pid(hwnd) == pid:
            name = c.create_unicode_buffer(128)
            p.u.GetClassNameW(hwnd, name, len(name))
            if name.value == class_name and (class_name!='#32770' or p.u.IsWindowVisible(hwnd)):
                found.append(hwnd)
        return True
    p.u.EnumWindows(visit, 0)
    return found[0] if found else 0


def main(cases=None, *, release=False):
    build = subprocess.run(['cargo','test','--bin','minicon-winmgr','--no-run','--message-format=json',
         *(['--release','--target-dir','target/release-validation'] if release else [])],
        cwd=ROOT,text=True,encoding='utf-8',stdout=subprocess.PIPE,check=True)
    executable = [item['executable'] for line in build.stdout.splitlines()
        if (item:=json.loads(line)).get('reason')=='compiler-artifact'
        and item.get('executable') and item.get('profile',{}).get('test')]
    assert len(executable)==1
    output = ROOT/'target/test-reports/diagnostics'/str(time.time_ns())
    output.mkdir(parents=True)
    report = {'profile':'release' if release else 'debug','passed':False,'checks':[], 'scope':'Owned diagnostic fixture windows; no live shell replacement',
              'test_binary_sha256':hashlib.sha256(Path(executable[0]).read_bytes()).hexdigest()}
    shell = p.shell_settings()
    original = hashlib.sha256(p.LIVE.read_bytes()).hexdigest()

    def check(name, condition):
        report['checks'].append({'name':name,'passed':bool(condition)})
        assert condition,name

    try:
        for language,theme in (cases or [('en','light'),('en','dark'),('zh','light'),('zh','dark')]):
            case = output/f'{language}-{theme}'
            case.mkdir()
            settings = case/'settings.txt'
            settings.write_text(f'ui.language={language}\nappearance.mode={theme}\n',encoding='utf-8')
            environment = dict(os.environ,MINICON_SETTINGS=str(settings),MINICON_CONFIG=str(case/'empty.conf'),
                               MINICON_LANGUAGE=language,MINICON_THEME_MODE=theme)
            process = subprocess.Popen([executable[0],'--exact','diagnostic_ui::tests::native_window_fixture',
                '--ignored','--nocapture'],cwd=ROOT,env=environment,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,
                text=True,encoding='utf-8',creationflags=subprocess.CREATE_NO_WINDOW)
            lines = []; mailbox = queue.Queue()
            def read():
                for line in process.stdout:
                    lines.append(line);mailbox.put(line)
                mailbox.put(None)
            reader = threading.Thread(target=read,daemon=True);reader.start()
            owner = 0
            try:
                while True:
                    line = mailbox.get(timeout=30)
                    assert line is not None,'fixture exited before creating its window'
                    if line.startswith('DIAGNOSTIC_UI_READY '):
                        ready=json.loads(line.split(' ',1)[1]);owner=ready['owner'];hwnd=ready['window'];info=ready['info'];break
                check(f'{language}-{theme}: owned fixture',p.pid(owner)==process.pid and p.pid(hwnd)==process.pid)
                ctrl = lambda ident:p.u.GetDlgItem(hwnd,ident)
                click = lambda ident:p.u.PostMessageW(ctrl(ident),0xF5,0,0)
                state = lambda:p.u.SendMessageW(owner,0x8000+202,0,0)
                count = lambda:(state()>>8)&0xffff
                active = lambda:bool(state()&2)
                check(f'{language}-{theme}: idle has no recording or export worker',state()==0)
                expected='Start' if language=='en' else '开始记录'
                check(f'{language}-{theme}: localized controls',p.text(ctrl(2))==expected)
                if language=='en' and theme=='light':
                    time.sleep(5.2)
                    idle_state=state();report['idle_after_5s_state']=idle_state
                    if idle_state:p.capture_settings(case/'unexpected-idle-state.png',hwnd)
                    check('default opens without a diagnostic timer',idle_state==0)
                p.capture_settings(case/'idle.png',hwnd)
                click(2);p.wait(active)
                check(f'{language}-{theme}: explicit start arms recording',count()==1 and not p.u.IsWindowEnabled(ctrl(2)))
                if language=='en' and theme=='light':
                    p.wait(lambda:count()>=2,timeout=8)
                    check('real five-second timer produces a sample',(state()>>24)>=1)
                else:
                    p.u.PostMessageW(hwnd,0x113,1,0)
                    p.wait(lambda:count()>=2)
                p.capture_settings(case/'recording.png',hwnd)
                click(3);p.wait(lambda:not active())
                before=count();time.sleep(.1)
                if language=='en' and theme=='light':
                    ticks=state()>>24;time.sleep(5.2)
                    check('stop removes the real sampling timer',state()>>24==ticks)

                p.u.PostMessageW(hwnd,0x113,1,0);time.sleep(.05)
                check(f'{language}-{theme}: stop ignores late timer messages',count()==before)
                # Open and cancel the real Save dialog, using only this test PID.
                click(4)
                dialog=p.wait(lambda:find(process.pid,'#32770'))
                p.u.PostMessageW(dialog,0x111,2,0)
                p.wait(lambda:not find(process.pid,'#32770'))
                check(f'{language}-{theme}: cancel does not create export worker',not (state()&4))
                # Save the reviewed summary through the native file dialog.
                destination=case/'summary.md'
                click(4);dialog=p.wait(lambda:find(process.pid,'#32770'))
                def filename_edit():
                    edits=[]
                    @ENUM
                    def inspect(child,_):
                        name=c.create_unicode_buffer(128);p.u.GetClassNameW(child,name,len(name))
                        if name.value=='Edit' and p.u.IsWindowVisible(child):
                            edits.append((p.rect(child)[1],child))
                        return True
                    p.u.EnumChildWindows(dialog,inspect,0)
                    return max(edits)[1] if edits else 0
                edit=p.wait(filename_edit)
                check(f'{language}-{theme}: file dialog has a filename control',bool(edit))
                value=c.create_unicode_buffer(str(destination))
                check(f'{language}-{theme}: filename text accepted',bool(p.u.SendMessageW(edit,12,0,c.cast(value,c.c_void_p).value)))
                p.u.PostMessageW(p.u.GetDlgItem(dialog,1),0xF5,0,0)
                p.wait(lambda:destination.exists(),timeout=10)
                p.wait(lambda:not (state()&8))
                data=destination.read_text(encoding='utf-8')
                check(f'{language}-{theme}: exported summary contains numeric resource fields',
                    'Private commit (bytes)' in data and 'Samples:' in data)
                check(f'{language}-{theme}: export excludes locations and UI configuration',
                    str(case) not in data and 'USERPROFILE' not in data and 'MINICON_' not in data)
                # Escape is routed through the diagnostic message filter.
                p.u.PostMessageW(ctrl(2),0x100,27,0)
                p.wait(lambda:not find(process.pid,'minicon_winmgr_diagnostics'))
                check(f'{language}-{theme}: close retains stopped data',not active() and count()==before)
                p.u.PostMessageW(p.u.GetDlgItem(info,3),0x100,13,0)
                hwnd=p.wait(lambda:find(process.pid,'minicon_winmgr_diagnostics'))
                check(f'{language}-{theme}: reopening does not resume recording',not active() and count()==before)
                # Replacing data requires explicit consent, with No as default.
                click(2);confirmation=p.wait(lambda:find(process.pid,'#32770'))
                check(f'{language}-{theme}: replacement defaults to No',(p.u.SendMessageW(confirmation,0x400,0,0)&0xffff)==7)
                p.u.PostMessageW(confirmation,0x111,7,0)
                p.wait(lambda:not find(process.pid,'#32770'))
                check(f'{language}-{theme}: declining replacement keeps data',not active() and count()==before)
                p.u.SendMessageW(ctrl(6),0x14E,2,0)
                click(2);confirmation=p.wait(lambda:find(process.pid,'#32770'))
                p.u.PostMessageW(confirmation,0x111,6,0);p.wait(active)
                p.u.PostMessageW(ctrl(3),0x100,27,0)
                p.wait(lambda:not find(process.pid,'minicon_winmgr_diagnostics'))
                closed_count=count()
                check(f'{language}-{theme}: Escape stops an active recording',not active() and closed_count>=1)
                p.u.PostMessageW(owner,0x8000+201,0,0)
                hwnd=p.wait(lambda:find(process.pid,'minicon_winmgr_diagnostics'))
                check(f'{language}-{theme}: scenario and data survive reopening',
                    not active() and count()==closed_count and p.u.SendMessageW(ctrl(6),0x147,0,0)==2)
                # Buttons remain within the client at narrow and wide sizes.
                for width,height in [(440,440),(700,620)]:
                    p.u.MoveWindow(hwnd,60,60,width,height,True)
                    time.sleep(.03)
                    client=w.RECT();p.u.GetClientRect(hwnd,c.byref(client))
                    origin=w.POINT();p.u.ClientToScreen(hwnd,c.byref(origin))
                    rectangles=[p.rect(ctrl(ident)) for ident in [1,2,3,4,5,8,9,11]]
                    check(f'{language}-{theme}: controls fit {width}x{height}',
                          all(origin.x<=r[0]<r[2]<=origin.x+client.right and origin.y<=r[1]<r[3]<=origin.y+client.bottom for r in rectangles) and
                          all(rectangles[i][2]<=rectangles[i+1][0] for i in range(1,4)))
            finally:
                if owner:p.u.PostMessageW(owner,16,0,0)
                try:process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill();process.wait();raise
                reader.join(timeout=2)
                (case/'fixture-local.log').write_text(''.join(lines),encoding='utf-8')
            check(f'{language}-{theme}: fixture exits cleanly',process.returncode==0)
        report['passed']=True
    finally:
        report['normal_binary_unchanged']=hashlib.sha256(p.LIVE.read_bytes()).hexdigest()==original
        report['shell_registration_unchanged']=p.shell_settings()==shell
        report['passed']=report['passed'] and report['normal_binary_unchanged'] and report['shell_registration_unchanged']
        (output/'summary.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
        print(json.dumps({'passed':report['passed'],'checks':len(report['checks'])}),flush=True)
    return report


if __name__=='__main__':
    import argparse
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--case',choices=['en-light','en-dark','zh-light','zh-dark'])
    parser.add_argument('--release',action='store_true')
    args=parser.parse_args()
    main([tuple(args.case.split('-'))] if args.case else None,release=args.release)
