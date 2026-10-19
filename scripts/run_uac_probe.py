"""Interactive exact-binary acceptance; full and single-decision scopes are separate."""
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
    scope=parser.add_mutually_exclusive_group()
    scope.add_argument('--cancel-only',action='store_true',help='Run only the real cancellation gate; does not cover startup or approval')
    scope.add_argument('--approve-only',action='store_true',help='Run only target approval and graceful cleanup; does not cover startup or cancellation')
    args=parser.parse_args()
    candidate=args.candidate.resolve(strict=True);restore=args.restore_exe.resolve(strict=True)
    folder=p.ROOT/'target/test-reports/run-uac'/str(time.time_ns());folder.mkdir(parents=True)
    gui.OUTPUT=folder
    env={k:v for k,v in os.environ.items() if not k.startswith('MINICON_')}
    env.update(MINICON_SETTINGS=str(folder/'settings.conf'),MINICON_CONFIG=str(folder/'empty.conf'),MINICON_LOG_DIR=str(folder/'logs'),LOCALAPPDATA=str(folder/'profile'))
    (folder/'settings.conf').write_text('ui.language=en\nappearance.mode=dark\n',encoding='utf-8')
    subprocess.run(['rustc',str(p.ROOT/'scripts/fixtures/run_probe.rs'),'--out-dir',str(folder)],check=True)
    targets={decision:folder/'Program Files'/f'UAC TEST - CLICK {decision.upper()}.exe' for decision in ['Yes','No']}
    targets['Yes'].parent.mkdir()
    for target in targets.values():shutil.copy2(folder/'run_probe.exe',target)
    report=dict(passed=False,scope="cancellation" if args.cancel_only else "approval" if args.approve_only else "all",candidate_sha256=s.digest(candidate),checks=[])
    isolation=None
    def check(name,value):
        report['checks'].append(dict(name=name,passed=bool(value)))
        if not value:
            report['failure_state']=gui.state()
            raise AssertionError(name)
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
        p.wait(lambda:gui.state()['foreground']==gui.state()['hwnd'])
    def command(output):
        # UAC obscures this conversation. Its unsigned program name must carry
        # the requested decision instead of making the user remember a sequence.
        target=targets['No' if output.stem=='cancelled' else 'Yes']
        return f'"{target}" "{output}" --name "\u5f20 \u4e09"'
    def prepare(output):
        summon();check('reopened_default_unchecked',p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0)==0)
        # Keyboard traversal has its own ordinary-desktop regression. Set up
        # this authorization gate through owned native controls, so unrelated
        # foreground input cannot submit a different command or hide Run.
        value=c.create_unicode_buffer(command(output))
        p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2001),0xc,0,c.addressof(value))
        p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf5,0,0)
        check('explicit_checkbox_checked',p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0)==1)
    try:
        with desktop_session.isolated(candidate,restore) as isolation:
            if not (args.cancel_only or args.approve_only):
                phase('1 startup of exact Winmgr as administrator','Yes')
                shell=c.WinDLL('shell32',use_last_error=True)
                class ExecuteInfo(c.Structure):
                    _fields_=[('size',w.DWORD),('mask',w.ULONG),('hwnd',w.HWND),('verb',w.LPCWSTR),('file',w.LPCWSTR),('parameters',w.LPCWSTR),('directory',w.LPCWSTR),('show',c.c_int),('instance',w.HINSTANCE),('id_list',c.c_void_p),('class_name',w.LPCWSTR),('class_key',w.HANDLE),('hotkey',w.DWORD),('icon',w.HANDLE),('process',w.HANDLE)]
                shell.ShellExecuteExW.argtypes=[c.POINTER(ExecuteInfo)];shell.ShellExecuteExW.restype=w.BOOL
                info=ExecuteInfo(size=c.sizeof(ExecuteInfo),mask=0x40,verb='runas',file=str(candidate),parameters='--settings',directory=str(folder),show=1)
                accepted_by_shell=bool(shell.ShellExecuteExW(c.byref(info)))
                report['startup_shell_result']=dict(accepted=accepted_by_shell,error=c.get_last_error(),has_process=bool(info.process))
                check('administrator_entry_accepted',accepted_by_shell and bool(info.process))
                try:
                    from types import SimpleNamespace
                    parent=token_status(SimpleNamespace(handle=info.process))
                    report['startup_parent']=parent
                    check('startup_parent_is_elevated',parent['elevated'] and parent['integrity']>=12288)
                    p.wait(lambda:p.message() and p.panel(),timeout=30)
                    p.wait(lambda:s.Desktop().send(p.message(),0x8000+142,pid=p.pid(p.message()))==1,timeout=30)
                    p.wait(lambda:p.text(p.panel()).endswith('0.1.14'))
                    check('elevated_launcher_exits',p.k.WaitForSingleObject(info.process,30000)==0)
                    code=w.DWORD();p.k.GetExitCodeProcess.argtypes=[w.HANDLE,c.POINTER(w.DWORD)]
                    check('elevated_launcher_exits_successfully',bool(p.k.GetExitCodeProcess(info.process,c.byref(code))) and code.value==0)
                finally:p.k.CloseHandle(info.process)
                report['startup_child']=ordinary_owned(candidate)
                process=m.Process(p.pid(p.message()))
                try:s.Desktop().close(process)
                finally:process.close()
            shutil.copy2(candidate,p.LIVE);p.start(env,['--settings'])
            ordinary_owned(p.LIVE)
            history=Path(env['LOCALAPPDATA'])/'minicon-winmgr/run-history.json'
            if not args.cancel_only:
                accepted=folder/'accepted.txt';prepare(accepted)
                phase('2 Run administrator target','Yes')
                p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2002),0xf5,0,0)
                p.wait(lambda:accepted.exists() or 'Authorization cancelled' in p.text(p.u.GetDlgItem(gui.popup(),2008)),timeout=180)
                check('system_approval_received',accepted.exists())
                p.wait(lambda:not p.u.IsWindowVisible(gui.popup()),timeout=15)
                text=accepted.read_text(encoding='utf-8')
                check('authorized_target_is_elevated','elevated=true' in text)
                check('authorized_unicode_arguments_preserved','\u5f20 \u4e09' in text)
                check('authorized_working_directory',f'cwd={env["USERPROFILE"]}' in text)
                ordinary_owned(p.LIVE)
                p.wait(lambda:history.exists() and command(accepted) in json.loads(history.read_text(encoding='utf-8')),timeout=10)
            if args.approve_only:
                check('accepted_binary_unchanged',s.digest(candidate)==report['candidate_sha256'])
                report['passed']=True
                return
            before=history.read_bytes() if history.exists() else None
            cancelled=folder/'cancelled.txt';prepare(cancelled)
            edit=p.u.GetDlgItem(gui.popup(),2001)
            p.u.SendMessageW(edit,0xb1,3,8)
            phase('3 Run administrator target','No')
            p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2002),0xf5,0,0)
            status=p.u.GetDlgItem(gui.popup(),2008)
            p.wait(lambda:cancelled.exists() or 'Authorization cancelled' in p.text(status),timeout=180)
            check('system_cancellation_received','Authorization cancelled' in p.text(status))
            check('cancelled_request_keeps_dialog_and_input',bool(p.u.IsWindowVisible(gui.popup())) and p.text(edit)==command(cancelled))
            packed=p.u.SendMessageW(edit,0xb0,0,0)
            check('cancelled_request_keeps_selection',(packed&65535,(packed>>16)&65535)==(3,8))
            check('cancelled_target_never_started',not cancelled.exists())
            check('cancelled_request_does_not_record_history',(history.read_bytes() if history.exists() else None)==before)
            ordinary_owned(p.LIVE)
            gui.capture('cancelled-uac.png')
            p.u.SendMessageW(gui.popup(),0x10,0,0);summon()
            check('after_cancel_reopen_resets_unchecked',p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0)==0)
            p.u.SendMessageW(gui.popup(),0x10,0,0)
            check('accepted_binary_unchanged',s.digest(candidate)==report['candidate_sha256'])
            report['passed']=True
    except Exception as error:
        report['passed']=False
        report['error']=repr(error)
        report.setdefault('failure_state',gui.state())
        raise
    finally:
        report['original_shell_restored']=bool(isolation and isolation.get('restored',False))
        (folder/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
        print(json.dumps(report,indent=2),flush=True)


if __name__=='__main__':main()
