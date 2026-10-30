"""Published helper compatibility, including failure after the image was renamed.

No UAC, registry writes or user file replacement. All update targets are owned
copies; desktop_session verifies and restores the actual original instance.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import tomllib
import desktop_session
import memory_probe as m
import scenario_probe as s
import settings_probe as p


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['candidate','legacy-exe','restore-exe']:
        parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args()
    candidate=args.candidate.resolve(strict=True)
    legacy=args.legacy_exe.resolve(strict=True)
    restore=args.restore_exe.resolve(strict=True)
    version=tomllib.loads((p.ROOT/'Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    output=p.ROOT/'target/test-reports/legacy-update-path'/str(time.time_ns())
    output.mkdir(parents=True)
    old_hash=s.digest(legacy);new_hash=s.digest(candidate)
    report=dict(passed=False,candidate_sha256=new_hash,legacy_sha256=old_hash,checks=[])
    isolation=None
    def check(name,value):
        report['checks'].append(dict(name=name,passed=bool(value)))
        if not value:raise AssertionError(name)
    try:
        with desktop_session.isolated(candidate,restore) as isolation:
            for mode in ['success','collision','child-process-limit']:
                print('Published helper path test: '+mode,flush=True)
                folder=output/(mode+' App 张三');folder.mkdir()
                exe=folder/'minicon-winmgr-v0.1.12-x86_64-windows.exe'
                destination=folder/f'minicon-winmgr-v{version}-x86_64-windows.exe'
                assert exe!=destination
                shutil.copy2(legacy,exe)
                config=folder/'settings.conf'
                config.write_text('ui.language=en\nappearance.mode=dark\n',encoding='utf-8')
                environment={key:value for key,value in os.environ.items() if not key.startswith('MINICON_')}
                environment.update(MINICON_SETTINGS=str(config),MINICON_CONFIG=str(folder/'empty.conf'),MINICON_LOG_DIR=str(folder/'logs'),LOCALAPPDATA=str(folder/'profile'))
                p.LIVE=exe;p.start(environment,['--settings'])
                parent=p.pid(p.message())
                stage=folder/'.winmgr-update-path-test';stage.mkdir()
                shutil.copy2(legacy,stage/'helper.exe');shutil.copy2(candidate,stage/'payload.exe')
                # Rust canonical paths use the Win32 verbatim prefix.
                target='\\\\?\\'+str(exe.resolve())
                plan=dict(target=target,parent=parent,old_hash=old_hash,new_hash=new_hash,version=version)
                manifest=stage/'plan.json';manifest.write_text(json.dumps(plan),encoding='utf-8')
                helper=None;job=None
                try:
                    helper=subprocess.Popen([str(stage/'helper.exe'),'--apply-update',str(manifest)],env=environment,creationflags=subprocess.CREATE_NO_WINDOW)
                    p.wait(lambda:(stage/'ready').exists() or helper.poll() is not None,timeout=12)
                    check(mode+'_legacy_helper_is_ready',(stage/'ready').exists())
                    if mode=='collision':destination.write_bytes(b'owned unrelated destination')
                    if mode=='child-process-limit':
                        import win32job,win32api
                        job=win32job.CreateJobObject(None,'WinmgrUpdatePath-'+str(time.time_ns()))
                        limits=win32job.QueryInformationJobObject(job,win32job.JobObjectExtendedLimitInformation)
                        limits['BasicLimitInformation']['LimitFlags']=win32job.JOB_OBJECT_LIMIT_ACTIVE_PROCESS|win32job.JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                        limits['BasicLimitInformation']['ActiveProcessLimit']=2
                        win32job.SetInformationJobObject(job,win32job.JobObjectExtendedLimitInformation,limits)
                        handle=win32api.OpenProcess(0x1fffff,False,helper.pid)
                        try:win32job.AssignProcessToJobObject(job,handle)
                        finally:handle.Close()
                    check(mode+'_waits_for_explicit_exit',p.pid(p.message())==parent and s.digest(exe)==old_hash)
                    p.stop()
                    if mode=='success':
                        p.wait(lambda:(stage/'started').exists() and p.message() and p.panel() and p.text(p.panel()).endswith(version),timeout=25)
                        process=m.Process(p.pid(p.message()))
                        try:
                            check('real_running_image_uses_new_version_filename',s.Desktop().image(process)==destination.resolve() and s.Desktop().same_user_session(process))
                            check('new_instance_remains_standard',not int.from_bytes(s.Desktop().token(process.handle,20).raw[:4],'little'))
                        finally:process.close()
                        check('old_filename_removed_new_payload_and_backup_verified',not exe.exists() and s.digest(destination)==new_hash and s.digest(stage/'previous.exe')==old_hash)
                        check('legacy_plan_adopted_new_destination',os.path.samefile(json.loads(manifest.read_text(encoding='utf-8'))['destination'],destination))
                    else:
                        p.wait(lambda:(stage/'error.txt').exists() and p.message() and p.panel() and 'restored' in p.text(p.controls().get(6400,0)).lower(),timeout=25)
                        check(mode+'_old_version_and_path_restored',s.digest(exe)==old_hash and not (stage/'started').exists())
                        if mode=='collision':
                            check('collision_never_overwrites_existing_file',destination.read_bytes()==b'owned unrelated destination')
                        else:
                            check('late_failure_restores_manifest_and_removes_new_name',not destination.exists() and 'destination' not in json.loads(manifest.read_text(encoding='utf-8')) and s.digest(stage/'failed.exe')==new_hash)
                    check(mode+'_legacy_helper_finishes',helper.wait(timeout=8)==(0 if mode=='success' else 1))
                finally:
                    if helper and helper.poll() is None:
                        (stage/'abort').write_text('owned test cleanup',encoding='utf-8')
                        helper.wait(timeout=35)
                    p.stop()
                    if job:job.Close()
            check('candidate_unchanged',s.digest(candidate)==new_hash)
        report['passed']=True
    except Exception as error:
        report['error']=repr(error);raise
    finally:
        report['original_shell_restored']=bool(isolation and isolation.get('restored'))
        (output/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
        print(json.dumps(report,indent=2),flush=True)


if __name__=='__main__':main()
