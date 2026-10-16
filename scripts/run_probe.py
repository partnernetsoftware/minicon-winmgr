"""Reproducible Run regression, called inside settings desktop isolation."""
import run_probe_input as gui, pathlib, time, json, shutil, os, subprocess

def main(environment=None):
    environment=environment or os.environ
    root=pathlib.Path(__file__).resolve().parents[1]/'target/test-reports/run'/str(time.time_ns())
    root.mkdir(parents=True)
    gui.OUTPUT=root
    subprocess.run(['rustc',str(pathlib.Path(__file__).parent/'fixtures/run_probe.rs'),'--out-dir',str(root)],check=True)
    (root/'run-probe.exe').write_bytes((root/'run_probe.exe').read_bytes())
    report=[]
    def wait(delay=.25):time.sleep(delay)
    def state():return gui.state()
    def check(name,condition,detail=None):
        report.append({'test':name,'passed':bool(condition),'detail':detail})
        if not condition:raise AssertionError(name)
    def ctrl(s,id):return s['controls'][str(id)]
    def replace(value):gui.key('CTRL+A');gui.key('BACK');gui.typing(value);wait(.2)
    def summon():gui.key('WIN+R');wait(.12);return state()
    try:
        s=summon();check('hotkey_focus',s['visible'] and s['foreground']==s['hwnd'] and s['focus']==ctrl(s,2001)['hwnd'])
        replace('');s=state();check('empty_disabled',not ctrl(s,2002)['enabled'])
        replace('minicon-missing-014-EXE');gui.key('ENTER');wait(1)
        s=state();check('failure_retains_input',s['visible'] and ctrl(s,2001)['text']=='minicon-missing-014-EXE' and bool(ctrl(s,2008)['text']),ctrl(s,2008)['text'])
        gui.capture('run-error.png')
        gui.key('WIN+R');wait();s=state();check('repeat_hotkey_retains_input',s['visible'] and ctrl(s,2001)['text']=='minicon-missing-014-EXE')
        replace('"C:\\unfinished');gui.key('ENTER');wait(.6);s=state();check('unclosed_quote_feedback','引号未闭合' in ctrl(s,2008)['text'])
        replace('draft not run');gui.key('ESC');wait();check('escape_hides',not state()['visible'])
        s=summon();check('draft_restored',ctrl(s,2001)['text']=='draft not run')
        folder=root/'fixtures'/'Program Files';folder.mkdir(parents=True,exist_ok=True);probe=folder/'Run App.exe';shutil.copy2(root/'run-probe.exe',probe)
        output=root/'probe-output.txt';output.unlink(missing_ok=True)
        command=f'"{probe}" "{output}" --name "张 三" %UNKNOWN%'
        replace(command);gui.key('ENTER')
        deadline=time.monotonic()+10
        while not output.exists() and time.monotonic()<deadline:wait(.1)
        wait(.2)
        check('successful_launch_produced_file',output.exists())
        data=output.read_text(encoding='utf8');check('quoted_unicode_arguments','张 三' in data and '%UNKNOWN%' in data,data)
        check('explicit_user_directory',f'cwd={environment["USERPROFILE"]}' in data,data)
        check('success_hides',not state()['visible'])
        s=summon();check('successful_command_recalled',ctrl(s,2001)['text']==command)
        gui.key('ALT+DOWN');wait(.5);s=state();check('history_dropdown',ctrl(s,2006)['visible'] and any(command in x for x in ctrl(s,2006)['items']))
        gui.capture('run-history.png')
        gui.key('DOWN');gui.key('ENTER');wait(.2);s=state();check('adoption_does_not_execute',s['visible'] and not ctrl(s,2006)['visible'])
        # Tab goes to clear history, then run, then cancel, then edit.
        focus=[]
        for _ in range(4):gui.key('TAB');wait(.05);focus.append(state()['focus'])
        s=state();check('tab_cycle',focus==[ctrl(s,2004)['hwnd'],ctrl(s,2002)['hwnd'],ctrl(s,2003)['hwnd'],ctrl(s,2001)['hwnd']],focus)
        pathprefix=str(root/'fixtures')+'\\';replace(pathprefix)
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            s=state()
            if ctrl(s,2006)['visible'] and any(x.startswith('[路径]') and 'Program Files' in x for x in ctrl(s,2006)['items']):break
            wait(.05)
        check('local_path_suggestions',ctrl(s,2006)['visible'] and any(x.startswith('[路径]') and 'Program Files' in x for x in ctrl(s,2006)['items']))
        gui.capture('run-path.png')
        gui.key('ESC');wait(.1);check('escape_closes_only_candidates',state()['visible'] and not ctrl(state(),2006)['visible'])
        gui.key('ESC');wait(.1);check('second_escape_hides',not state()['visible'])
        s=summon();gui.key('ALT+F4');wait(.15);check('alt_f4_hides_reusable',not state()['visible']);s=summon();check('reopen_after_close',s['visible'])
        gui.click_control(2004);wait(.6);s=state();check('clear_history',not ctrl(s,2004)['enabled'] or not ctrl(s,2004)['visible'])
        history=pathlib.Path(environment['LOCALAPPDATA'])/'minicon-winmgr'/'run-history.json';check('clear_disk_history',not history.exists())
        gui.key('ESC');wait(.1)
    except Exception as error:
        report.append({'error':repr(error),'state':state()})
    finally:
        (root/'acceptance-results.json').write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf8')
        print(json.dumps(report,ensure_ascii=True,indent=2))
        if any(not x.get('passed',True) or 'error'in x for x in report):raise RuntimeError('Run native acceptance failed; inspect local report')
    return report

if __name__=="__main__":
    raise SystemExit("Use scripts/test.py --layer release with explicit restore and candidate artifacts.")
