"""Controlled whole-process scenarios; owned test executables, original shell restored.

Reports stay in ignored target. Smoke runs verify the driver, never acceptance.
No binary replacement, registry writes, global input, or other-session termination.
"""
from __future__ import annotations
import argparse
import csv
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import secrets
import subprocess
import sys
import time
import memory_probe as m
import settings_probe as p

ROOT = Path(__file__).resolve().parents[1]
SETTINGS = 0x8000 + 141
READY = 0x8000 + 142


def digest(path):
    with Path(path).open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


def options(args):
    if getattr(args,'subject','both')!='both' and args.scenario not in ['soak','docking','shortcuts']:
        raise ValueError('Single-subject runs are reserved for soak; comparisons require both subjects')
    if args.iterations is None:args.iterations=300 if args.scenario=='windows' else 200
    for name in ('warmup', 'observe', 'recovery', 'interval'):
        if not math.isfinite(getattr(args, name)):
            raise ValueError('Scenario times must be finite')
    if min(args.warmup, args.recovery) < 0 or args.observe <= 0:
        raise ValueError('Scenario times must be nonnegative; observation must be positive')
    if not .1 <= args.interval <= 60 or not 1 <= args.repetitions <= 3:
        raise ValueError('Interval or repetition count is outside the driver bounds')
    if not 1 <= args.iterations <= 1000:
        raise ValueError('Iteration count must be between 1 and 1000')
    # An operation may spend bounded time waiting for saves; also cap the run.
    if args.warmup + args.observe + args.recovery > 86400:
        raise ValueError('A run must fit within 24 hours')
    if math.ceil((args.warmup+args.observe+args.recovery)/args.interval)+6 > m.MAX_SAMPLES:
        raise ValueError('Scenario exceeds the sample cap; increase interval')
    if args.scenario == 'soak' and args.observe < 7200:
        raise ValueError('Soak requires at least two hours; use idle for driver smoke checks')


class Desktop:
    def __init__(self):
        self.u, self.k = p.u, p.k
        for name, args, result in [
            ('SendMessageTimeoutW', [w.HWND,w.UINT,w.WPARAM,w.LPARAM,w.UINT,w.UINT,c.POINTER(c.c_size_t)],c.c_ssize_t),
            ('SystemParametersInfoW',[w.UINT,w.UINT,c.c_void_p,w.UINT],w.BOOL),
            ('OpenInputDesktop',[w.DWORD,w.BOOL,w.DWORD],w.HANDLE),
            ('CloseDesktop',[w.HANDLE],w.BOOL),
            ('CreateWindowExW',[w.DWORD,w.LPCWSTR,w.LPCWSTR,w.DWORD,c.c_int,c.c_int,c.c_int,c.c_int,w.HWND,w.HMENU,w.HINSTANCE,w.LPVOID],w.HWND),
            ('DestroyWindow',[w.HWND],w.BOOL),
            ('PeekMessageW',[c.POINTER(w.MSG),w.HWND,w.UINT,w.UINT,w.UINT],w.BOOL),
            ('TranslateMessage',[c.POINTER(w.MSG)],w.BOOL),
            ('DispatchMessageW',[c.POINTER(w.MSG)],c.c_ssize_t),
        ]:
            getattr(self.u,name).argtypes=args;getattr(self.u,name).restype=result
        self.k.QueryFullProcessImageNameW.argtypes=[w.HANDLE,w.DWORD,w.LPWSTR,c.POINTER(w.DWORD)]
        self.k.QueryFullProcessImageNameW.restype=w.BOOL
        self.k.GetCurrentProcess.argtypes=[]
        self.k.GetCurrentProcess.restype=w.HANDLE
        self.k.ProcessIdToSessionId.argtypes=[w.DWORD,c.POINTER(w.DWORD)]
        self.k.ProcessIdToSessionId.restype=w.BOOL
        self.a=c.WinDLL('advapi32',use_last_error=True)
        self.a.OpenProcessToken.argtypes=[w.HANDLE,w.DWORD,c.POINTER(w.HANDLE)]
        self.a.GetTokenInformation.argtypes=[w.HANDLE,c.c_int,c.c_void_p,w.DWORD,c.POINTER(w.DWORD)]
        self.a.EqualSid.argtypes=[c.c_void_p,c.c_void_p]
        self.a.EqualSid.restype=w.BOOL
        desktop=self.u.OpenInputDesktop(0,False,1)
        if not desktop:raise RuntimeError('An unlocked input desktop is required')
        self.u.CloseDesktop(desktop)
        if int.from_bytes(self.token(self.k.GetCurrentProcess(),20)[:4],'little'):
            raise RuntimeError('Run the driver as the ordinary desktop user')

    def token(self,process,kind):
        token=w.HANDLE();size=w.DWORD()
        if not self.a.OpenProcessToken(process,8,c.byref(token)):raise RuntimeError('Cannot inspect process identity')
        try:
            self.a.GetTokenInformation(token,kind,None,0,c.byref(size))
            data=c.create_string_buffer(size.value)
            if not self.a.GetTokenInformation(token,kind,data,size,c.byref(size)):raise RuntimeError('Cannot inspect token')
            return data
        finally:self.k.CloseHandle(token)

    def same_user_session(self,process):
        ours=w.DWORD();theirs=w.DWORD()
        if not self.k.ProcessIdToSessionId(os.getpid(),c.byref(ours)) or not self.k.ProcessIdToSessionId(process.pid,c.byref(theirs)) or ours.value!=theirs.value:
            return False
        a=self.token(self.k.GetCurrentProcess(),1);b=self.token(process.handle,1)
        return bool(self.a.EqualSid(c.cast(a,c.POINTER(c.c_void_p))[0],c.cast(b,c.POINTER(c.c_void_p))[0]))

    def image(self,process):
        name=c.create_unicode_buffer(32768);length=w.DWORD(len(name))
        if not self.k.QueryFullProcessImageNameW(process.handle,0,name,c.byref(length)):
            raise RuntimeError('Cannot identify current shell executable')
        return Path(name.value).resolve()

    def send(self,hwnd,message,wp=0,lp=0,pid=None):
        if not hwnd or pid is not None and p.pid(hwnd)!=pid:raise RuntimeError('Window identity changed')
        value=c.c_size_t()
        if not self.u.SendMessageTimeoutW(hwnd,message,wp,lp,0x22,2000,c.byref(value)):
            raise RuntimeError('Test window did not respond within its message budget')
        return value.value

    def workarea(self):
        rect=w.RECT()
        if not self.u.SystemParametersInfoW(48,0,c.byref(rect),0):raise RuntimeError('Cannot read work area')
        return [rect.left,rect.top,rect.right,rect.bottom]

    def display(self):
        return dict(primary_width=self.u.GetSystemMetrics(0),primary_height=self.u.GetSystemMetrics(1),
            virtual_left=self.u.GetSystemMetrics(76),virtual_top=self.u.GetSystemMetrics(77),
            virtual_width=self.u.GetSystemMetrics(78),virtual_height=self.u.GetSystemMetrics(79),
            system_dpi=self.u.GetDpiForSystem())

    def pump(self):
        message=w.MSG()
        while self.u.PeekMessageW(c.byref(message),None,0,0,1):
            self.u.TranslateMessage(c.byref(message));self.u.DispatchMessageW(c.byref(message))

    def close(self,process):
        hwnd=p.message()
        if not hwnd or p.pid(hwnd)!=process.pid:raise RuntimeError('Shell identity changed before graceful close')
        self.u.PostMessageW(hwnd,16,0,0)
        if self.k.WaitForSingleObject(process.handle,8000)!=0:raise RuntimeError('Shell refused graceful exit')

    def preflight(self,exe,env):
        subprocess.run([str(exe),'--help'],cwd=exe.parent,env=env,check=True,timeout=8,
            stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,
            creationflags=subprocess.DETACHED_PROCESS|subprocess.CREATE_NEW_PROCESS_GROUP|0x01000000,
            close_fds=True)

    def start(self,exe,env):
        child=subprocess.Popen([str(exe)],cwd=exe.parent,env=env,
            creationflags=subprocess.DETACHED_PROCESS|subprocess.CREATE_NEW_PROCESS_GROUP|0x01000000,
            close_fds=True)
        try:
            end=time.monotonic()+12
            while time.monotonic()<end:
                if child.poll() is not None:raise RuntimeError('Test executable exited before readiness')
                hwnd=p.message()
                if hwnd and p.pid(hwnd)==child.pid and self.send(hwnd,READY,pid=child.pid):return child
                time.sleep(.03)
            raise RuntimeError('Shell readiness timed out')
        except BaseException:
            # Only the exact child this driver started may be terminated.
            if child.poll() is None:child.kill();child.wait(timeout=5)
            raise


class Observation:
    def __init__(self,process,folder,interval,scenario):
        self.process,self.folder,self.interval=process,folder,interval
        self.started=time.monotonic();self.started_utc_ns=time.time_ns();self.cpu=time.process_time();self.next=0
        self.operations=dict(settings_changes=0,settings_cycles=0,windows_created=0,windows_destroyed=0)
        self.rows=[];self.phase='warmup';self.phases=[dict(phase='warmup',elapsed_seconds=0)];self.scenario=scenario
        self.raw=(folder/'samples.csv').open('x',newline='',encoding='utf-8')
        self.writer=csv.DictWriter(self.raw,fieldnames=['elapsed_seconds','phase',*m.METRICS]);self.writer.writeheader()

    def tick(self,force=False):
        elapsed=time.monotonic()-self.started
        if elapsed>86400:raise RuntimeError('Scenario exceeded the 24-hour driver limit')
        if elapsed<self.next and not force:return
        if len(self.rows)>=m.MAX_SAMPLES:raise RuntimeError('Scenario sample cap reached')
        row=dict(elapsed_seconds=elapsed,phase=self.phase,**self.process.sample())
        self.rows.append(row);self.writer.writerow(row);self.raw.flush()
        self.next=(math.floor(elapsed/self.interval)+1)*self.interval

    def switch(self,phase):
        self.phase=phase
        self.phases.append(dict(phase=phase,elapsed_seconds=round(time.monotonic()-self.started,4)))
        self.tick(True)

    def wait(self,desktop,seconds):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            desktop.pump();self.tick();time.sleep(min(.02,max(0,deadline-time.monotonic())))
        self.tick()

    def finish(self,reason):
        self.raw.close()
        result=dict(schema=2,scenario=self.scenario,binary_sha256=self.process.image_hash,
            os_build=sys.getwindowsversion().build,interval_seconds=self.interval,
            elapsed_seconds=round(time.monotonic()-self.started,3),sample_count=len(self.rows),
            started_utc_ns=self.started_utc_ns,finished_utc_ns=time.time_ns(),process_created_filetime=self.process.created,
            observer_and_driver_cpu_seconds=round(time.process_time()-self.cpu,4),stop_reason=reason,
            phases=self.phases,operations=self.operations,metrics=m.summarize(self.rows),long_run_passed=False,
            phase_metrics={phase:m.summarize([row for row in self.rows if row['phase']==phase])
                           for phase in ['warmup','work','recovery']})
        (self.folder/'summary.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
        return result


def settings(desktop,child,observation,config,count):
    desktop.send(p.message(),SETTINGS,pid=child.pid)
    panel=p.wait(lambda:p.panel() if p.panel() and p.pid(p.panel())==child.pid and p.u.IsWindowVisible(p.panel()) else None)
    changes=[(6151,'ui.language','zh'),(6150,'ui.language','en'),(6131,'appearance.mode','dark'),
        (6130,'appearance.mode','light'),(6132,'appearance.mode','auto'),(6140,'taskbar.position','top'),
        (6142,'taskbar.position','left'),(6143,'taskbar.position','right'),(6141,'taskbar.position','bottom')]
    for _ in range(count):
        for ident,key,value in changes:
            desktop.send(p.controls().get(ident),0xF5,pid=child.pid)
            end=time.monotonic()+5
            while True:
                parsed={}
                try:lines=config.read_text(encoding='utf-8-sig').splitlines()
                except (OSError,UnicodeError):lines=[]
                for line in lines:
                    if '=' in line and not line.lstrip().startswith('#'):
                        k,v=line.split('=',1);parsed[k.strip()]=v.strip()
                if parsed.get(key)==value:break
                if time.monotonic()>end:raise RuntimeError('Preference save did not complete')
                observation.wait(desktop,.02)
            observation.operations['settings_changes']+=1;observation.tick()
        observation.operations['settings_cycles']+=1
    desktop.send(panel,16,pid=child.pid)


def windows(desktop,observation,count):
    live=[]
    try:
        for index in range(count):
            hwnd=desktop.u.CreateWindowExW(0x08040000,'STATIC',f'Winmgr resource fixture {index}',
                0x90000000,-32000,-32000,240,80,None,None,None,None)
            if not hwnd:raise RuntimeError('Could not create owned fixture window')
            live.append(hwnd);observation.operations['windows_created']+=1
            if len(live)>8:
                if not desktop.u.DestroyWindow(live[0]):raise RuntimeError('Owned fixture window refused destruction')
                live.pop(0);observation.operations['windows_destroyed']+=1
            observation.wait(desktop,.2)
    finally:
        failed=False
        for hwnd in live:
            if desktop.u.DestroyWindow(hwnd):observation.operations['windows_destroyed']+=1
            else:failed=True
        desktop.pump()
        if failed:raise RuntimeError('Owned fixture cleanup was incomplete')


def docking(desktop,child,observation,config):
    """Exercise actual maximization, not just our rectangle calculations."""
    desktop.u.ShowWindow.argtypes=[w.HWND,c.c_int];desktop.u.ShowWindow.restype=w.BOOL
    dwm=c.WinDLL('dwmapi')
    dwm.DwmGetWindowAttribute.argtypes=[w.HWND,w.DWORD,c.c_void_p,w.DWORD]
    dwm.DwmGetWindowAttribute.restype=c.c_long
    hwnd=desktop.u.CreateWindowExW(0,'STATIC','Winmgr docking acceptance',
        0x10CF0000,300,150,500,350,None,None,None,None)
    if not hwnd:raise RuntimeError('Could not create maximization fixture')
    try:
        desktop.send(p.message(),SETTINGS,pid=child.pid)
        panel=p.wait(lambda:p.panel() if p.panel() and p.pid(p.panel())==child.pid and p.u.IsWindowVisible(p.panel()) else None)
        display=desktop.display();bounds=[0,0,display['primary_width'],display['primary_height']]
        checks=[]
        for edge,ident in [('bottom',6141),('left',6142),('bottom',6141),('right',6143),('top',6140),('left',6142),('right',6143),('bottom',6141)]:
            desktop.send(p.controls().get(ident),0xF5,pid=child.pid)
            end=time.monotonic()+5
            while 'taskbar.position='+edge not in config.read_text(encoding='utf-8-sig').replace(' ',''):
                if time.monotonic()>end:raise RuntimeError('Docking preference did not save')
                observation.wait(desktop,.02)
            observation.wait(desktop,.15)
            bar=p.rect(p.window('minicon_winmgr_taskbar'))
            expected=bounds.copy()
            expected[{'left':0,'top':1,'right':2,'bottom':3}[edge]]=bar[{'left':2,'top':3,'right':0,'bottom':1}[edge]]
            actual=desktop.workarea()
            checks.append(dict(edge=edge,expected=expected,actual=actual))
            (observation.folder/'docking.json').write_text(json.dumps(checks,indent=2),encoding='utf-8')
            if actual!=expected:raise RuntimeError('Docking retained a stale work-area edge: '+edge)
            desktop.u.ShowWindow(hwnd,9);desktop.u.ShowWindow(hwnd,3)
            observation.wait(desktop,.15)
            frame=w.RECT()
            if dwm.DwmGetWindowAttribute(hwnd,9,c.byref(frame),c.sizeof(frame))<0:
                raise RuntimeError('Could not inspect maximized visible frame')
            visible=[frame.left,frame.top,frame.right,frame.bottom]
            checks[-1]['maximized_frame']=visible
            (observation.folder/'docking.json').write_text(json.dumps(checks,indent=2),encoding='utf-8')
            if any(abs(a-b)>1 for a,b in zip(visible,expected)):
                raise RuntimeError('Maximized fixture does not fill the current work area: '+edge)
        desktop.send(panel,16,pid=child.pid)
        observation.operations['docking_checks']=len(checks)
    finally:
        if not desktop.u.DestroyWindow(hwnd):raise RuntimeError('Could not destroy maximization fixture')


def shortcuts(desktop,child,observation,config):
    """Verify the actual recycle-bin folder, without touching its contents."""
    def bins():
        command="""$ErrorActionPreference='Stop'; $shell=New-Object -ComObject Shell.Application;
$ids=@($shell.Windows() | ForEach-Object {
  try { if ($_.Document.Folder.Self.Path -eq '::{645FF040-5081-101B-9F08-00AA002F954E}') { [long]$_.HWND } } catch {}
}); ConvertTo-Json -InputObject $ids -Compress"""
        result=subprocess.run(['powershell.exe','-NoProfile','-NonInteractive','-Command',command],
            check=True,timeout=12,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,
            creationflags=subprocess.CREATE_NO_WINDOW)
        values=json.loads(result.stdout)
        if not isinstance(values,list) or not all(type(value) is int for value in values):
            raise RuntimeError('Unexpected recycle-bin window identity response')
        return set(values)
    before=bins();created=set();checks=[]
    try:
        cases=[(language,ident,caption,theme,theme_id,key)
            for language,ident,caption in [('en',6150,'Recycle Bin'),('zh',6151,'回收站')]
            for theme,theme_id in [('light',6130),('dark',6131)] for key in ['enter','space']]
        for language,ident,caption,theme,theme_id,key in cases:
            desktop.send(p.message(),SETTINGS,pid=child.pid)
            panel=p.wait(lambda:p.panel() if p.panel() and p.pid(p.panel())==child.pid and p.u.IsWindowVisible(p.panel()) else None)
            desktop.send(p.controls().get(ident),0xF5,pid=child.pid)
            desktop.send(p.controls().get(theme_id),0xF5,pid=child.pid)
            observation.wait(desktop,.2)
            notification=p.window('minicon_winmgr_notification_panel')
            desktop.send(notification,0x312,pid=child.pid)
            p.wait(lambda:p.u.IsWindowVisible(notification))
            button=p.u.GetDlgItem(notification,7)
            if not button or p.text(button)!=caption or not p.u.IsWindowVisible(button):
                context=dict(language=language,theme=theme,key=key,expected=caption,
                    actual=p.text(button) if button else None,button_exists=bool(button),
                    button_visible=bool(button and p.u.IsWindowVisible(button)),
                    popup_visible=bool(p.u.IsWindowVisible(notification)),
                    foreground_pid=p.pid(p.u.GetForegroundWindow()),popup_pid=p.pid(notification))
                (observation.folder/'shortcut-failure.json').write_text(json.dumps(context,indent=2),encoding='utf-8')
                if p.u.IsWindowVisible(notification):p.capture_settings(observation.folder/'shortcut-failure.png',notification)
                raise RuntimeError('Localized recycle-bin shortcut is missing')
            p.capture_settings(observation.folder/(language+'-'+theme+'-shortcuts.png'),notification)
            # Native Tab handlers set focus on the owner thread. Exercise the
            # actual button subclass and Space key-up, without global input.
            for ident in [1,2,6]:
                desktop.send(p.u.GetDlgItem(notification,ident),0x100,9,pid=child.pid)
            if p.focus()!=button:raise RuntimeError('Tab did not reach the recycle-bin shortcut')
            code=13 if key=='enter' else 32
            desktop.send(button,0x100,code,pid=child.pid)
            desktop.send(button,0x101,code,pid=child.pid)
            end=time.monotonic()+15
            while True:
                actual=bins();created|=actual-before
                if actual:break
                if time.monotonic()>end:raise RuntimeError('Recycle-bin folder did not open')
                observation.wait(desktop,.1)
            p.wait(lambda:not p.u.IsWindowVisible(notification))
            checks.append(dict(language=language,theme=theme,key=key,tab_focus=True,
                localized_button=True,actual_namespace_opened=True))
            desktop.send(panel,16,pid=child.pid)
        observation.operations['shortcut_checks']=len(checks)
    finally:
        for hwnd in created:
            if p.u.IsWindow(hwnd):p.u.PostMessageW(hwnd,16,0,0)
        (observation.folder/'shortcuts.json').write_text(json.dumps(dict(checks=checks,contents_untouched=True)),encoding='utf-8')


def main(args):
    options(args)
    candidate=args.candidate.resolve(strict=True);restore=args.restore_exe.resolve(strict=True)
    baseline=(args.baseline or restore).resolve(strict=True)
    desktop=Desktop();original=m.Process(p.pid(p.message()))
    child=None;closed=False;report=None
    base=ROOT/'target/test-reports/scenarios';base.mkdir(parents=True,exist_ok=True)
    cohort_file=base/'cohort.id'
    try:
        with cohort_file.open('x',encoding='ascii') as file:file.write(secrets.token_hex(16))
    except FileExistsError:pass
    with cohort_file.open(encoding='ascii') as file:cohort=file.read(65)
    if len(cohort)!=32 or any(char not in '0123456789abcdef' for char in cohort):raise RuntimeError('Invalid local cohort identifier')
    output=base/str(time.time_ns());output.mkdir()
    env=os.environ.copy();shell=p.shell_settings();area=desktop.workarea()
    try:
        if desktop.image(original)!=restore or not desktop.same_user_session(original):
            raise RuntimeError('Current shell does not match the explicitly named restore executable/user/session')
        expected=digest(restore)
        if original.image_hash!=expected:raise RuntimeError('Original image changed before the test')
        report=dict(schema=2,cohort=cohort,declared_profile=getattr(args,'profile','release'),scenario=args.scenario,runs=[],passed=False,restored=False,
            original_sha256=expected,original_stopped=False,original_preserved=False,role='companion',repetitions=args.repetitions,
            warmup_seconds=args.warmup,observe_seconds=args.observe,recovery_seconds=args.recovery,
            iterations=args.iterations,interval_seconds=args.interval,display=desktop.display(),
            settings_profile=dict(language='en',theme='auto',position='bottom',menu=False,hover_preview=False),
            protocol_minimums_met=args.repetitions==3 and args.warmup>=300 and args.recovery>=600 and
                (args.iterations >= (300 if args.scenario=='windows' else 200) if args.scenario in ['windows','settings'] else args.observe>=(7200 if args.scenario=='soak' else 900)),
            limitations=['Companion role: test paths are not the registered login shell.',
                         'Timing includes the external observer and scenario driver; user desktop activity is not isolated.',
                         'Completion alone is not a stability or leak verdict.'])
        # A private crash-recovery note; never publish this path-bearing file.
        (output/'restore-local.json').write_text(json.dumps(dict(executable=str(restore),sha256=expected)),encoding='utf-8')
        preflight_env={key:value for key,value in env.items() if not key.startswith('MINICON_')}
        preflight_env.update(MINICON_LOG_DIR=str(output/'preflight-logs'),RUST_LOG='info')
        for image in dict.fromkeys([candidate,baseline,restore]):desktop.preflight(image,preflight_env)
        desktop.close(original);closed=True;report['original_stopped']=True
        if getattr(args,'fail_at',None)=='after-stop':raise RuntimeError('Injected failure after original stop')
        for repetition in range(args.repetitions):
            subjects=[('baseline',baseline),('candidate',candidate)]
            if getattr(args,'subject','both')=='candidate':subjects=[('candidate',candidate)]
            if repetition%2:subjects.reverse()
            for subject,source in subjects:
                folder=output/f'{repetition+1}-{subject}';folder.mkdir()
                exe=folder/'winmgr.exe';shutil.copy2(source,exe)
                source_hash=digest(source)
                if digest(exe)!=source_hash:raise RuntimeError('Copied test binary identity mismatch')
                config=folder/'settings.conf'
                config.write_text('ui.language=en\nappearance.mode=auto\ntaskbar.position=bottom\ntaskbar.menu=false\nbehavior.hover_preview=false\n',encoding='utf-8')
                test_env={key:value for key,value in env.items() if not key.startswith('MINICON_')}
                test_env.update(MINICON_SETTINGS=str(config),MINICON_CONFIG=str(folder/'empty.conf'),
                    MINICON_LOG_DIR=str(folder/'logs'),LOCALAPPDATA=str(folder/'profile'),RUST_LOG='info')
                print(json.dumps(dict(event='starting',subject=subject,repetition=repetition+1)),flush=True)
                child=desktop.start(exe,test_env)
                if getattr(args,'fail_at',None)=='after-start':raise RuntimeError('Injected failure after test start')
                process=m.Process(child.pid)
                observation=None;reason='sampling_failed'
                try:
                    if process.image_hash!=source_hash:raise RuntimeError('Running test binary identity mismatch')
                    observation=Observation(process,folder,args.interval,
                        'resources' if args.scenario=='windows' else args.scenario)
                    observation.wait(desktop,args.warmup);observation.switch('work')
                    if args.scenario=='settings':settings(desktop,child,observation,config,args.iterations)
                    elif args.scenario=='docking':docking(desktop,child,observation,config)
                    elif args.scenario=='shortcuts':shortcuts(desktop,child,observation,config)
                    elif args.scenario=='windows':windows(desktop,observation,args.iterations)
                    else:observation.wait(desktop,args.observe)
                    observation.switch('recovery');observation.wait(desktop,args.recovery)
                    observation.tick(True);reason='duration_completed'
                finally:
                    if observation:
                        result=observation.finish(reason);report['runs'].append(dict(subject=subject,repetition=repetition+1,**result))
                    try:
                        if child.poll() is None:desktop.close(process)
                    finally:process.close()
                    child.wait(timeout=8);child=None
                print(json.dumps(dict(event='completed',subject=subject,repetition=repetition+1)),flush=True)
        report['passed']=True
    finally:
        try:
            if child is not None and child.poll() is None:
                hwnd=p.message()
                if hwnd and p.pid(hwnd)==child.pid:desktop.u.PostMessageW(hwnd,16,0,0)
                try:child.wait(timeout=8)
                except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)
            if closed:
                # No copy over the original executable, no settings/registry rewrite.
                if p.message():raise RuntimeError('Unexpected shell appeared; original not restarted over it')
                restored=desktop.start(restore,env)
                identity=m.Process(restored.pid)
                try:
                    p.wait(lambda:desktop.workarea()==area,timeout=8)
                    good=identity.image_hash==expected and digest(restore)==expected and p.shell_settings()==shell
                    if report:report['restored']=good;report['passed']=report['passed'] and good
                    if not good:raise RuntimeError('Original shell identity or registration changed')
                finally:identity.close()
        finally:
            if report and not closed:
                report['original_preserved']=bool(p.message()) and p.pid(p.message())==original.pid
            original.close()
            if report:
                if not report['restored']:report['passed']=False
                (output/'summary.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
                print(json.dumps(dict(passed=report['passed'],restored=report['restored'],runs=len(report['runs']))),flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate',type=Path,required=True)
    parser.add_argument('--restore-exe',type=Path,required=True)
    parser.add_argument('--baseline',type=Path)
    parser.add_argument('--subject',choices=['both','candidate'],default='both')
    parser.add_argument('--profile',choices=['release','debug'],default='release',help='Declared build profile; verify with the build record')
    parser.add_argument('--scenario',choices=['idle','settings','windows','soak','docking','shortcuts'],default='idle')
    parser.add_argument('--repetitions',type=int,default=3)
    parser.add_argument('--warmup',type=float,default=300)
    parser.add_argument('--observe',type=float,default=900)
    parser.add_argument('--recovery',type=float,default=600)
    parser.add_argument('--interval',type=float,default=5)
    parser.add_argument('--iterations',type=int)
    parser.add_argument('--fail-at',choices=['after-stop','after-start'],help=argparse.SUPPRESS)
    main(parser.parse_args())
