"""Loopback updates and real two-version handoffs, confined to owned directories."""
import ctypes as c
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import time
import settings_probe as p

OUT=p.ROOT/'target/update-acceptance'
CHECK,UPDATE,RESTART,STATUS=6401,6402,6403,6400

def build_fixture(release=False):
    """Backport the current updater into a clearly isolated 0.1.8 test build."""
    fixture=p.ROOT/'target/update-fixture-source';fixture.mkdir(parents=True,exist_ok=True)
    shutil.copytree(p.ROOT/'src',fixture/'src',dirs_exist_ok=True)
    manifest=(p.ROOT/'Cargo.toml').read_text(encoding='utf-8').replace('name = "minicon-winmgr"','name = "minicon-winmgr-update-fixture"',1).replace('version = "0.1.9"','version = "0.1.8"',1)
    manifest=manifest.replace('edition = "2024"','edition = "2024"\nautobins = false',1)+'\n[[bin]]\nname = "winmgr-update-fixture"\npath = "src/main.rs"\n'
    (fixture/'Cargo.toml').write_bytes(manifest.encode())
    shutil.copy2(p.ROOT/'Cargo.lock',fixture/'Cargo.lock')
    target=p.ROOT/('target/release-validation' if release else 'target')
    subprocess.run(['cargo','build','--manifest-path',str(fixture/'Cargo.toml'),'--target-dir',str(target),*(['--release'] if release else [])],cwd=p.ROOT,check=True)
    return target/('release' if release else 'debug')/'winmgr-update-fixture.exe'

def main(*,release=False):
    OUT.mkdir(parents=True,exist_ok=True)
    fixture=build_fixture(release)
    candidate=p.SOURCE.read_bytes();old_bytes=fixture.read_bytes()
    original=os.environ.copy();original_live=p.LIVE
    report={'binary_sha256':hashlib.sha256(candidate).hexdigest(),'fixture_sha256':hashlib.sha256(old_bytes).hexdigest(),'fixture_note':'0.1.8 test-only backport of current updater, not a historical public binary. Successful target is the actual 0.1.9 build.','checks':[]}
    shell=p.shell_settings()
    mode={'value':'good','payload':candidate};requests=[];created=[]
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_GET(self):
            requests.append(self.path)
            if mode['value']=='rate':self.send_error(429);return
            if self.path=='/latest':
                body=json.dumps({'tag_name':'v0.1.9','draft':False,'prerelease':False,'assets':[{'name':'minicon-winmgr-v0.1.9-x86_64-windows.exe','browser_download_url':base+'/app.exe'},{'name':'SHA256SUMS.txt','browser_download_url':base+'/sums'}]}).encode()
            elif self.path=='/sums':
                digest='0'*64 if mode['value']=='bad_hash' else hashlib.sha256(mode['payload']).hexdigest()
                body=(digest+'  minicon-winmgr-v0.1.9-x86_64-windows.exe\n').encode()
            elif self.path=='/app.exe':body=mode['payload']
            else:self.send_error(404);return
            self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
    base=f'http://127.0.0.1:{server.server_port}'
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    def check(name,value,detail=None):
        report['checks'].append(dict(name=name,passed=bool(value),detail=detail));assert value,name
    def status():
        handle=p.controls().get(STATUS)
        return p.text(handle) if handle else ''
    def enabled(ident):return bool(p.u.IsWindowEnabled(p.controls().get(ident,0)))
    def wait_idle():p.wait(lambda:enabled(CHECK) or enabled(RESTART),timeout=15)
    def start_case(binary):
        p.stop()
        folder=Path(tempfile.mkdtemp(prefix='case-',dir=OUT));created.append(folder);exe=folder/'winmgr.exe';exe.write_bytes(binary)
        cfg=folder/'settings.txt';cfg.write_text('ui.language=en\nappearance.mode=light\ntaskbar.position=bottom\ntaskbar.menu=false\n',encoding='utf-8')
        env=original.copy()
        for key in ['MINICON_LANGUAGE','MINICON_TASKBAR','MINICON_NO_TASKBAR','MINICON_START_MENU','MINICON_THEME_MODE','MINICON_TASKBAR_POSITION','MINICON_WINMGR_UPDATE_RECOVERED']:
            env.pop(key,None)
        env.update(MINICON_SETTINGS=str(cfg),MINICON_CONFIG=str(folder/'empty.conf'),LOCALAPPDATA=str(folder/'profile'),MINICON_WINMGR_UPDATE_API=base+'/latest')
        p.LIVE=exe;p.start(env,['--settings']);p.wait(lambda:p.u.IsWindowVisible(p.panel()) and status())
        return folder,exe,cfg
    def available():
        p.click(CHECK);p.wait(lambda:enabled(UPDATE),timeout=15)
    def ready():
        p.click(UPDATE);p.wait(lambda:enabled(RESTART),timeout=20)
        return next(p.LIVE.parent.glob('.winmgr-update-*'))
    try:
        folder,exe,cfg=start_case(candidate)
        time.sleep(.4)
        check('startup_and_open_settings_do_not_connect',not requests and enabled(CHECK) and not enabled(UPDATE) and not enabled(RESTART))
        p.click(CHECK);wait_idle()
        check('same_version_is_current_without_downgrade','Up to date' in status() and not enabled(UPDATE))
        p.capture_settings(OUT/'settings_en_light.png')
        p.click(6151);p.saved()
        check('update_controls_switch_to_chinese',p.text(p.ctrl(CHECK))=='检查更新' and '最新版本' in status())
        p.capture_settings(OUT/'settings_zh_light.png')
        p.click(6131);p.saved();p.capture_settings(OUT/'settings_zh_dark.png')
        p.click(6150);p.saved();p.capture_settings(OUT/'settings_en_dark.png')
        mode['value']='rate';p.click(CHECK);wait_idle()
        check('rate_limit_is_retryable_and_current_file_unchanged',not enabled(UPDATE) and enabled(CHECK) and exe.read_bytes()==candidate)
        mode['value']='good';p.click(CHECK);wait_idle()
        check('check_recovers_after_network_error','Up to date' in status())
        folder,exe,cfg=start_case(old_bytes);available()
        check('newer_version_enables_only_update',enabled(UPDATE) and not enabled(RESTART))
        mode['value']='bad_hash';p.click(UPDATE);wait_idle()
        check('checksum_failure_never_changes_installed_binary',exe.read_bytes()==old_bytes and not enabled(RESTART) and enabled(UPDATE))
        mode['value']='good';stage=ready()
        check('download_is_staged_and_restart_is_separate',exe.read_bytes()==old_bytes and (stage/'payload.exe').read_bytes()==candidate)
        # Save failure must prevent handing off a shell with unsaved settings.
        held=cfg.with_suffix('.held');cfg.rename(held);cfg.mkdir()
        p.click(6131);p.wait(lambda:p.u.IsWindowEnabled(p.ctrl(6113)))
        check('failed_settings_save_disables_restart',not enabled(RESTART))
        cfg.rmdir();held.rename(cfg);p.click(6113);p.saved();p.wait(lambda:enabled(RESTART))
        before=p.pid(p.message());payload=stage/'payload.exe';data=bytearray(candidate);data[-1]^=1;payload.write_bytes(data)
        p.click(RESTART);p.wait(lambda:'retry' in status().lower() and enabled(RESTART),timeout=10)
        check('restart_rechecks_staged_hash_before_exit',p.pid(p.message())==before and exe.read_bytes()==old_bytes)
        payload.write_bytes(candidate);p.click(RESTART)
        p.wait(lambda:p.message() and p.pid(p.message())!=before and p.panel() and p.text(p.panel()).endswith('0.1.9') and (stage/'started').exists(),timeout=30)
        check('real_018_fixture_to_019_handoff',exe.read_bytes()==candidate and (stage/'previous.exe').read_bytes()==old_bytes and p.text(p.panel()).endswith('0.1.9'))
        check('settings_survive_restart','appearance.mode = dark' in cfg.read_text(encoding='utf-8'))
        # Hold only our installed fixture file without delete sharing.
        folder,exe,cfg=start_case(old_bytes);available();stage=ready();before=p.pid(p.message())
        p.k.CreateFileW.argtypes=[p.w.LPCWSTR,p.w.DWORD,p.w.DWORD,c.c_void_p,p.w.DWORD,p.w.DWORD,p.w.HANDLE];p.k.CreateFileW.restype=p.w.HANDLE
        lock=p.k.CreateFileW(str(exe),0x80000000,1,None,3,0,None)
        assert lock and lock!=p.w.HANDLE(-1).value
        try:
            p.click(RESTART)
            p.wait(lambda:(stage/'error.txt').exists() and p.message() and p.pid(p.message())!=before and p.panel() and 'restored' in status().lower(),timeout=30)
            check('locked_executable_recovers_previous_shell',exe.read_bytes()==old_bytes and not (stage/'previous.exe').exists())
        finally:p.k.CloseHandle(lock)
        # Valid PE and checksum, wrong compiled version: new startup cannot ack.
        mode['payload']=old_bytes
        folder,exe,cfg=start_case(old_bytes);available();stage=ready();before=p.pid(p.message())
        p.click(RESTART)
        p.wait(lambda:(stage/'error.txt').exists() and (stage/'failed.exe').exists() and p.message() and p.pid(p.message())!=before and p.panel() and 'restored' in status().lower(),timeout=40)
        check('unacknowledged_candidate_rolls_back_and_restarts',exe.read_bytes()==old_bytes and not (stage/'started').exists())
        check('all_network_requests_stay_on_local_fixture',all(path in ['/latest','/sums','/app.exe'] for path in requests),len(requests))
        report['passed']=True
    except Exception as error:
        report['passed']=False;report['error']=repr(error)
        if p.panel():report['status']=status();p.capture_settings(OUT/'failure.png')
        raise
    finally:
        if p.LIVE!=original_live:
            held=p.LIVE.parent/'settings.held';cfg=p.LIVE.parent/'settings.txt'
            if held.exists():
                if cfg.is_dir():cfg.rmdir()
                held.rename(cfg)
        # Cancel/wait only helpers created in these fixture stages before stopping
        # the shell. Otherwise a failed test could launch a fixture after cleanup.
        for stage in (stage for folder in created for stage in folder.glob('.winmgr-update-*')):
            if not (stage/'ready').exists():continue
            try:
                helper_pid=int((stage/'ready').read_text())
                if not (stage/'started').exists():(stage/'abort').write_text('test cleanup')
                handle=p.k.OpenProcess(0x101000,False,helper_pid)
                if handle:
                    try:
                        p.k.QueryFullProcessImageNameW.argtypes=[p.w.HANDLE,p.w.DWORD,p.w.LPWSTR,c.POINTER(p.w.DWORD)]
                        image=c.create_unicode_buffer(32768);size=p.w.DWORD(len(image))
                        if p.k.QueryFullProcessImageNameW(handle,0,image,c.byref(size)) and Path(image.value).resolve()==(stage/'helper.exe').resolve():
                            p.k.WaitForSingleObject(handle,25000)
                    finally:p.k.CloseHandle(handle)
            except (OSError,ValueError):pass
        p.stop();p.LIVE=original_live;shutil.copy2(p.SOURCE,p.LIVE);p.start(original)
        server.shutdown();server.server_close();thread.join(timeout=2)
        report['transaction_evidence']=[]
        for folder in created:
            for stage in folder.glob('.winmgr-update-*'):
                report['transaction_evidence'].append({'stage':stage.name,'started':(stage/'started').exists(),'backup':(stage/'previous.exe').exists(),'failed_candidate':(stage/'failed.exe').exists(),'error':(stage/'error.txt').read_text() if (stage/'error.txt').exists() else None})
        if report.get('passed'):
            for folder in created:
                owned=folder.resolve();assert owned.parent==OUT.resolve() and owned.name.startswith('case-')
                shutil.rmtree(owned)

        report['normal_session_restored']=bool(p.message())
        report['shell_registration_unchanged']=p.shell_settings()==shell
        (OUT/'summary.json').write_bytes((json.dumps(report,ensure_ascii=False,indent=2)+'\n').encode())
        print(json.dumps(report,ensure_ascii=True,indent=2))

if __name__=='__main__':main()
