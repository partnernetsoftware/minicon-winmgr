"""Native, opt-in legacy instance handoff; artifacts and reports stay in target."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import desktop_session as d
import scenario_probe as s
import settings_probe as p


def main(candidate, legacy, restore, language="en"):
    candidate=Path(candidate).resolve(strict=True)
    legacy=Path(legacy).resolve(strict=True)
    folder=s.ROOT/'target/test-reports/instance-switch'/str(time.time_ns())
    folder.mkdir(parents=True)
    old=folder/'legacy'/'minicon-winmgr.exe'
    new=folder/'candidate'/'minicon-winmgr.exe'
    for source,target in [(legacy,old),(candidate,new)]:
        target.parent.mkdir();shutil.copy2(source,target)
    env={k:v for k,v in os.environ.items() if not k.startswith('MINICON_')}
    env.update(MINICON_SETTINGS=str(folder/'settings.txt'),MINICON_CONFIG=str(folder/'empty.conf'),
               MINICON_LOG_DIR=str(folder/'logs'),MINICON_LANGUAGE=language)
    (folder/'empty.conf').write_text('',encoding='utf-8')
    tests=[]
    def check(name,condition):
        tests.append(dict(name=name,passed=bool(condition)))
        if not condition:raise AssertionError(name)
    def spawn(image):
        return subprocess.Popen([str(image)],cwd=image.parent,env=env,close_fds=True,
            creationflags=subprocess.DETACHED_PROCESS|subprocess.CREATE_NEW_PROCESS_GROUP|0x01000000)
    def prompt(pid):
        found=[]
        @c.WINFUNCTYPE(w.BOOL,w.HWND,w.LPARAM)
        def visit(hwnd,_):
            kind=c.create_unicode_buffer(128)
            p.u.GetClassNameW(w.HWND(hwnd),kind,128)
            if p.pid(hwnd)==pid and kind.value=='#32770' and p.u.IsWindowVisible(hwnd):found.append(hwnd)
            return True
        p.u.EnumWindows(visit,0)
        return found[0] if found else None
    children=[]
    try:
        with d.isolated(candidate,restore):
            try:
                previous=spawn(old);children.append(previous)
                p.wait(lambda:p.message() and p.pid(p.message())==previous.pid)
                check('official legacy executable runs',previous.poll() is None)
                for mode in ['cancel','escape']:
                    proposed=spawn(new);children.append(proposed)
                    hwnd=p.wait(lambda:prompt(proposed.pid))
                    title=c.create_unicode_buffer(256)
                    p.u.GetWindowTextW(hwnd,title,256)
                    check(mode+' localized prompt',title.value==('切换运行中的 winmgr？' if language=='zh' else 'Switch running winmgr?'))
                    check(mode+' prompt keeps original alive',previous.poll() is None)
                    check(mode+' defaults to cancel',(p.u.SendMessageW(hwnd,0x400,0,0)&0xffff)==2)
                    if mode=='escape':p.u.PostMessageW(hwnd,0x100,27,0)
                    else:p.u.PostMessageW(hwnd,0x111,2,0)
                    proposed.wait(timeout=8)
                    check(mode+' leaves original instance unchanged',previous.poll() is None and p.pid(p.message())==previous.pid)
                proposed=spawn(new);children.append(proposed)
                hwnd=p.wait(lambda:prompt(proposed.pid))
                p.u.PostMessageW(hwnd,0x111,1,0)
                previous.wait(timeout=20)
                check('confirmed old process exits normally',previous.returncode==0)
                desktop=s.Desktop()
                p.wait(lambda:p.message() and p.pid(p.message())==proposed.pid and desktop.send(p.message(),s.READY,pid=proposed.pid))
                check('replacement reaches startup readiness',proposed.poll() is None)
                duplicate=spawn(new);children.append(duplicate);duplicate.wait(timeout=8)
                check('identical build reuses current instance',duplicate.returncode==0 and proposed.poll() is None and p.pid(p.message())==proposed.pid)
            finally:
                # D.isolated closes the surviving owned shell; close unaccepted prompts first.
                for child in children:
                    hwnd=prompt(child.pid) if child.poll() is None else None
                    if hwnd:p.u.PostMessageW(hwnd,0x111,2,0)
                for child in children:
                    if child.poll() is None and (not p.message() or p.pid(p.message())!=child.pid):child.wait(timeout=20)
        check('original desktop restored by outer guard',True)
    finally:
        (folder/'summary.json').write_text(json.dumps(dict(tests=tests,candidate_sha256=s.digest(candidate),legacy_sha256=s.digest(legacy)),indent=2),encoding='utf-8')
    print(f'Instance handoff: {len(tests)} checks passed; original desktop restored.',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    for name in ['candidate','legacy','restore']:parser.add_argument('--'+name,required=True)
    parser.add_argument('--language',choices=['en','zh'],default='en')
    args=parser.parse_args();main(args.candidate,args.legacy,args.restore,args.language)
