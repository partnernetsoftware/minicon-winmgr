"""Interactive exact-binary acceptance. Requires three explicit UAC decisions."""
import argparse
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import desktop_session
import settings_probe as p
import scenario_probe as s
import memory_probe as m
import run_probe_input as gui


def token_status(process):
    desktop=s.Desktop()
    elevation=int.from_bytes(desktop.token(process.handle,20).raw[:4],'little')
    kind=int.from_bytes(desktop.token(process.handle,18).raw[:4],'little')
    label=desktop.token(process.handle,25)
    sid=c.cast(label,c.POINTER(c.c_void_p))[0]
    a=desktop.a
    a.GetSidSubAuthorityCount.argtypes=[c.c_void_p];a.GetSidSubAuthorityCount.restype=c.POINTER(c.c_ubyte)
    a.GetSidSubAuthority.argtypes=[c.c_void_p,w.DWORD];a.GetSidSubAuthority.restype=c.POINTER(w.DWORD)
    count=a.GetSidSubAuthorityCount(sid)[0]
    integrity=a.GetSidSubAuthority(sid,count-1)[0]
    return dict(elevated=bool(elevation),elevation_type=kind,integrity=integrity)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate',type=Path,required=True)
    parser.add_argument('--restore-exe',type=Path,required=True)
    parser.add_argument('--interactive-uac',action='store_true',required=True)
    args=parser.parse_args()
    candidate=args.candidate.resolve(strict=True);restore=args.restore_exe.resolve(strict=True)
    folder=p.ROOT/'target/test-reports/run-uac'/str(time.time_ns());folder.mkdir(parents=True)
    gui.OUTPUT=folder
    env={k:v for k,v in os.environ.items() if not k.startswith('MINICON_')}
    env.update(MINICON_SETTINGS=str(folder/'settings.conf'),MINICON_CONFIG=str(folder/'empty.conf'),MINICON_LOG_DIR=str(folder/'logs'),LOCALAPPDATA=str(folder/'profile'))
    (folder/'settings.conf').write_text('ui.language=en\nappearance.mode=dark\n',encoding='utf-8')
    subprocess.run(['rustc',str(p.ROOT/'scripts/fixtures/run_probe.rs'),'--out-dir',str(folder)],check=True)
    target=folder/'Program Files'/'Run Permission Probe.exe';target.parent.mkdir();shutil.copy2(folder/'run_probe.exe',target)
    report=dict(passed=False,candidate_sha256=s.digest(candidate),checks=[])
    isolation=None
    def check(name,value):
        report['checks'].append(dict(name=name,passed=bool(value)))
        if not value:raise AssertionError(name)
    def phase(name,decision):
        (folder/'phase.json').write_text(json.dumps(dict(phase=name,required_user_decision=decision)),encoding='utf-8')
        print('UAC phase: '+name+'; choose '+decision,flush=True)
    def ordinary_owned(expected):
        process=m.Process(p.pid(p.message()))
        try:
            state=token_status(process)
            check('shell_image_and_same_user_session',s.Desktop().image(process)==expected.resolve() and s.Desktop().same_user_session(process))
            check('shell_remains_standard',not state['elevated'] and state['elevation_type']!=2 and state['integrity']<=8192)
            return state
        finally:process.close()
    def summon():
        p.show();p.u.SendMessageW(p.message(),0x312,3,0)
        p.wait(lambda:gui.popup() and p.u.IsWindowVisible(gui.popup()))
    def command(output):
        return f'"{target}" "{output}" --name "\u5f20 \u4e09"'
    def prepare(output):
        summon();check('reopened_default_unchecked',p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0)==0)
        gui.key('ALT+O');gui.key('CTRL+A');gui.typing(command(output));time.sleep(.2)
        gui.key('TAB');gui.key('SPACE');time.sleep(.08)
        check('explicit_checkbox_checked',p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0)==1)
        gui.key('ALT+O')
    try:
        with desktop_session.isolated(candidate,restore) as isolation:
            phase('1 startup of exact Winmgr as administrator','Yes')
            shell=c.WinDLL('shell32',use_last_error=True)
            shell.ShellExecuteW.argtypes=[w.HWND,w.LPCWSTR,w.LPCWSTR,w.LPCWSTR,w.LPCWSTR,c.c_int];shell.ShellExecuteW.restype=c.c_void_p
            result=shell.ShellExecuteW(None,'runas',str(candidate),'--settings',str(folder),1)
            check('administrator_entry_accepted',bool(result) and int(result)>32)
            p.wait(lambda:p.message() and p.panel(),timeout=30)
            p.wait(lambda:p.text(p.panel()).endswith('0.1.14'))
            report['startup_child']=ordinary_owned(candidate)
            process=m.Process(p.pid(p.message()))
            try:s.Desktop().close(process)
            finally:process.close()
            shutil.copy2(candidate,p.LIVE);p.start(env,['--settings'])
            ordinary_owned(p.LIVE)
            accepted=folder/'accepted.txt';prepare(accepted)
            phase('2 Run administrator target','Yes')
            gui.key('ENTER')
            p.wait(lambda:accepted.exists(),timeout=180)
            p.wait(lambda:not p.u.IsWindowVisible(gui.popup()),timeout=15)
            text=accepted.read_text(encoding='utf-8')
            check('authorized_target_is_elevated','elevated=true' in text)
            check('authorized_unicode_arguments_preserved','\u5f20 \u4e09' in text)
            check('authorized_working_directory',f'cwd={env["USERPROFILE"]}' in text)
            ordinary_owned(p.LIVE)
            history=Path(env['LOCALAPPDATA'])/'minicon-winmgr/run-history.json'
            p.wait(lambda:history.exists() and command(accepted) in json.loads(history.read_text(encoding='utf-8')),timeout=10)
            before=history.read_bytes()
            cancelled=folder/'cancelled.txt';prepare(cancelled)
            edit=p.u.GetDlgItem(gui.popup(),2001)
            p.u.SendMessageW(edit,0xb1,3,8)
            phase('3 Run administrator target','No')
            gui.key('ENTER')
            status=p.u.GetDlgItem(gui.popup(),2008)
            p.wait(lambda:'Authorization cancelled' in p.text(status),timeout=180)
            check('cancelled_request_keeps_dialog_and_input',bool(p.u.IsWindowVisible(gui.popup())) and p.text(edit)==command(cancelled))
            packed=p.u.SendMessageW(edit,0xb0,0,0)
            check('cancelled_request_keeps_selection',(packed&65535,(packed>>16)&65535)==(3,8))
            check('cancelled_target_never_started',not cancelled.exists())
            check('cancelled_request_does_not_record_history',history.read_bytes()==before)
            ordinary_owned(p.LIVE)
            gui.capture('cancelled-uac.png')
            p.u.SendMessageW(gui.popup(),0x10,0,0);summon()
            check('after_cancel_reopen_resets_unchecked',p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0)==0)
            p.u.SendMessageW(gui.popup(),0x10,0,0)
            check('accepted_binary_unchanged',s.digest(candidate)==report['candidate_sha256'])
            report['passed']=True
    except Exception as error:
        report['error']=repr(error);raise
    finally:
        report['original_shell_restored']=bool(isolation and isolation.get('restored',False))
        (folder/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
        print(json.dumps(report,indent=2),flush=True)


if __name__=='__main__':main()
