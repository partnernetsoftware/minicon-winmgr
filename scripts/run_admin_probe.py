"""Ordinary-desktop R8 controls and launch regression; never requests UAC."""
import ctypes as c
import json
import os
from pathlib import Path
import subprocess
import time
import settings_probe as p
import run_probe_input as gui


def main(environment):
    # The established Run regression intentionally fixes Chinese in its child.
    # This matrix needs live language controls, using the same isolated files.
    if 'MINICON_LANGUAGE' in environment:
        environment=dict(environment)
        environment.pop('MINICON_LANGUAGE')
        p.stop();p.start(environment,['--settings'])
    folder=p.ROOT/'target/test-reports/run-admin-controls'/str(time.time_ns())
    folder.mkdir(parents=True)
    gui.OUTPUT=folder
    report=dict(passed=False,checks=[])
    def check(name,value):
        report['checks'].append(dict(name=name,passed=bool(value)))
        if not value:raise AssertionError(name)
    def state():return gui.state()
    def trace(label):
        report.setdefault('trace',[]).append(dict(label=label,caption=p.text(p.panel()),english=p.checked(6150),chinese=p.checked(6151)))
    def checked():return bool(p.u.SendMessageW(p.u.GetDlgItem(gui.popup(),2010),0xf0,0,0))
    def summon():
        p.show();p.u.SendMessageW(p.message(),0x312,3,0)
        p.wait(lambda:gui.popup() and p.u.IsWindowVisible(gui.popup()))
        p.wait(lambda:state()['foreground']==state()['hwnd'])
    def replace(value):
        gui.key('ALT+O');gui.key('CTRL+A');gui.typing(value);time.sleep(.2)
    try:
        # End any previous presentation before testing the default.
        if gui.popup():p.u.SendMessageW(gui.popup(),0x10,0,0)
        summon();check('default_unchecked',not checked())
        replace('minicon-control-draft-014')
        gui.key('TAB');time.sleep(.08)
        check('tab_reaches_native_checkbox',state()['focus']==p.u.GetDlgItem(gui.popup(),2010))
        gui.key('SPACE');time.sleep(.08);check('space_toggles_checkbox',checked());gui.capture('checked-admin.png')
        before=state()['controls']['2001']['text']
        check('checking_does_not_run_or_edit',state()['visible'] and state()['controls']['2001']['text']==before)
        replace('edited minicon-control-draft-014');check('editing_preserves_choice',checked())
        p.u.SendMessageW(p.message(),0x312,3,0);check('repeat_summon_preserves_choice',checked())
        gui.key('ESC');p.wait(lambda:not p.u.IsWindowVisible(gui.popup()))
        summon();check('reopen_resets_unchecked',not checked())
        subprocess.run(['rustc',str(p.ROOT/'scripts/fixtures/run_probe.rs'),'--out-dir',str(folder)],check=True)
        target=folder/'Program Files'/'Run Permission Probe.exe';target.parent.mkdir()
        import shutil
        shutil.copy2(folder/'run_probe.exe',target)
        output=folder/'ordinary.txt'
        command=f'"{target}" "{output}" --name "张 三"'
        replace(command);gui.key('ENTER')
        p.wait(lambda:output.exists(),timeout=12)
        p.wait(lambda:not p.u.IsWindowVisible(gui.popup()))
        text=output.read_text(encoding='utf-8')
        check('unchecked_target_is_ordinary','elevated=false' in text)
        check('parameters_and_unicode_preserved','张 三' in text)
        check('declared_working_directory',f'cwd={environment["USERPROFILE"]}' in text)
        summon();check('successful_command_recalled',state()['controls']['2001']['text']==command)
        gui.key('ALT+DOWN');time.sleep(.25);gui.key('DOWN');time.sleep(.1)
        check('candidate_preview_preserves_unchecked',not checked())
        gui.key('ESC');gui.key('ESC');summon()
        for language,language_id in [('en',6150),('zh',6151)]:
            p.u.SendMessageW(gui.popup(),0x10,0,0);p.show();p.click(language_id);p.saved();p.wait(lambda:p.checked(language_id));trace(language+' chosen')
            for theme,theme_id in [('light',6130),('dark',6131)]:
                p.show();trace(language+' before theme');p.click(theme_id);p.saved();p.wait(lambda:p.checked(theme_id));trace(language+' theme chosen');summon();trace(language+' run summoned')
                value=state();button=value['controls']['2010'];root=value['rect']
                check(language+'_'+theme+'_checkbox_visible',button['visible'] and button['rect'][0]>=root[0] and button['rect'][2]<=root[2] and button['rect'][3]<=value['controls']['2002']['rect'][1])
                check(language+'_'+theme+'_checkbox_label',button['text']==('Run as administrator' if language=='en' else '使用管理员权限运行'))
                gui.capture(language+'-'+theme+'-run.png')
                p.u.SendMessageW(gui.popup(),0x10,0,0)
        check('shell_stays_same_process',p.pid(p.message())==value['pid'])
        report['passed']=True
    except Exception as error:
        report['error']=repr(error);report['state']=state();report['settings_language']={str(i):dict(checked=p.checked(i),enabled=bool(p.u.IsWindowEnabled(p.ctrl(i)))) for i in [6150,6151]};raise
    finally:
        if gui.popup():p.u.SendMessageW(gui.popup(),0x10,0,0)
        (folder/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
    print('Run administrator controls: '+str(len(report['checks']))+' checks passed.',flush=True)
    return report
