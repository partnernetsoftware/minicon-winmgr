"""Own-window screenshots and live theme/docking acceptance; no elevation."""
import ctypes as c
import hashlib
import json
import os
import shutil
import time
import winreg
from pathlib import Path
import settings_probe as p

OUT = p.ROOT / 'target/industrial-acceptance'
CFG = OUT / 'appearance.conf'

def work():
    r = p.w.RECT()
    assert p.u.SystemParametersInfoW(48, 0, c.byref(r), 0)
    return [r.left, r.top, r.right, r.bottom]

def main(*, layout_only=False):
    OUT.mkdir(parents=True, exist_ok=True)
    original = os.environ.copy()
    env = original.copy()
    for name in ['MINICON_NO_TASKBAR', 'MINICON_TASKBAR', 'MINICON_TRAY', 'MINICON_GROUP_TASKS', 'MINICON_GROUP_BY', 'MINICON_HOVER_PREVIEW', 'MINICON_THEME_MODE', 'MINICON_TASKBAR_POSITION', 'MINICON_LANGUAGE']:
        env.pop(name, None)
    env.update(MINICON_SETTINGS=str(CFG), MINICON_CONFIG=str(OUT/'empty.conf'))
    CFG.write_text('appearance.mode = auto\ntaskbar.position = bottom\n', encoding='utf-8')
    report = {'binary_sha256':hashlib.sha256(p.SOURCE.read_bytes()).hexdigest(), 'checks': [], 'limits': ['Single physical DPI/session; no active UAC or system theme changes.']}
    shell = p.shell_settings()
    def check(name, condition, detail=None):
        report['checks'].append(dict(name=name, passed=bool(condition), detail=detail))
        assert condition, name
    try:
        p.stop()
        baseline = work()
        report["baseline_work_area"]=baseline
        shutil.copy2(p.SOURCE,p.LIVE)
        p.start(env, ['--settings'])
        p.show()
        process = p.pid(p.message())
        if layout_only:
            check('default_language_is_english',p.checked(6150) and p.text(p.panel()).startswith('Settings'),p.text(p.panel()))
            def relative(ident):
                r=p.rect(p.ctrl(ident));origin=p.rect(p.panel());return [r[0]-origin[0],r[1]-origin[1],r[2]-origin[0],r[3]-origin[1]]
            for theme,ident in [('light',6130),('dark',6131)]:
                p.click(ident);p.saved();time.sleep(.1)
                check(theme+'_inline_theme_label',relative(6211)[1]==relative(6130)[1])
                check(theme+'_inline_position_label',relative(6212)[1]==relative(6140)[1])
                check(theme+'_parallel_visibility_controls',relative(6101)[1]==relative(6102)[1])
                check(theme+'_parallel_secondary_entries',relative(6120)[1]==relative(6205)[1])
                p.capture_settings(OUT/('compact_'+theme+'.png'))
            for language,language_id,title in [('zh',6151,'设置'),('en',6150,'Settings')]:
                p.click(language_id);p.saved();time.sleep(.1)
                check(language+'_live_language_switch',p.checked(language_id) and p.text(p.panel()).startswith(title),p.text(p.panel()))
                for theme,theme_id in [('light',6130),('dark',6131)]:
                    p.click(theme_id);p.saved();time.sleep(.15)
                    p.capture_settings(OUT/('settings_'+language+'_'+theme+'.png'))
            expected=relative(6130)
            origin=p.rect(p.panel())
            for width in [400,640,180,640]:
                assert p.u.MoveWindow(p.panel(),origin[0],origin[1],width,origin[3]-origin[1],True)
                time.sleep(.08)
                if width==640:check('wide_layout_restored_after_resize',relative(6130)==expected,dict(expected=expected,actual=relative(6130)))
            check('preview_and_secondary_entries_fit_default_viewport',p.rect(p.ctrl(6205))[3] < p.rect(p.ctrl(6114))[1])
            p.click(6151);p.saved();p.stop();p.start(env,['--settings'])
            check('restart_preserves_language',p.checked(6151) and p.text(p.panel()).startswith('设置'))
            p.stop();env['MINICON_LANGUAGE']='en';p.start(env,['--settings'])
            check('environment_language_is_read_only',p.checked(6150) and not p.u.IsWindowEnabled(p.ctrl(6151)) and p.text(p.ctrl(6312)) == 'Fixed by startup configuration')
            report['passed']=True
            return
        states = {}
        report["edge_work_areas"]=states
        screen_w=p.u.GetSystemMetrics(0);screen_h=p.u.GetSystemMetrics(1)
        for theme, ident in [('light', 6130), ('dark', 6131)]:
            p.click(ident); p.saved()
            for edge, button in [('bottom',6141), ('top',6140), ('left',6142), ('right',6143)]:
                p.click(button); p.saved(); time.sleep(.3)
                r=p.rect(p.window('minicon_winmgr_taskbar'))
                vertical=edge in ('left','right')
                check(theme+'_'+edge+'_geometry', (r[2]-r[0]==180 and (r[0]==0 if edge=='left' else r[2]==screen_w)) if vertical else (r[3]-r[1]==36 and (r[1]==0 if edge=='top' else r[3]==screen_h)), r)
                check(theme+'_'+edge+'_selection',p.checked(ident) and p.checked(button))
                states[edge]=work()
                p.capture_settings(OUT/(theme+'_'+edge+'_settings.png'))
                p.capture_settings(OUT/(theme+'_'+edge+'_bar.png'),p.window('minicon_winmgr_taskbar'))
        p.click(6132);p.saved()
        p.u.SendMessageW(p.window('minicon_winmgr_taskbar'),0x1A,0,0)
        p.capture_settings(OUT/'auto_settings.png')
        from PIL import Image
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER,r'Software\Microsoft\Windows\CurrentVersion\Themes\Personalize') as key:
                light=winreg.QueryValueEx(key,'AppsUseLightTheme')[0]!=0
        except FileNotFoundError:light=True
        check('auto_palette_matches_windows_application_setting',Image.open(OUT/'auto_settings.png').getpixel((0,0))==((245,241,233) if light else (36,33,30)))
        check('footer_close_remains_visible',p.u.IsWindowVisible(p.ctrl(6114)) and p.rect(p.ctrl(6114))[3]<=p.rect(p.panel())[3],p.rect(p.ctrl(6114)))
        for theme,ident in [('light',6130),('dark',6131)]:
            p.click(ident);p.saved()
            p.click(6120)
            info=p.wait(lambda:p.window('minicon_winmgr_settings_info'))
            p.wait(lambda:p.u.IsWindowVisible(info));time.sleep(.15)
            p.capture_settings(OUT/(theme+'_info.png'),info)
            p.u.SendMessageW(info,16,0,0)
            notification=p.window('minicon_winmgr_notification_panel')
            p.u.SendMessageW(notification,0x312,0,0)
            p.wait(lambda:p.u.IsWindowVisible(notification));time.sleep(.15)
            p.capture_settings(OUT/(theme+'_notifications.png'),notification)
            p.u.SendMessageW(notification,16,0,0)
            p.u.SendMessageW(p.message(),0x312,3,0)
            run=p.wait(lambda:p.window('minicon_winmgr_run_dialog'))
            p.wait(lambda:p.u.IsWindowVisible(run));time.sleep(.15)
            p.capture_settings(OUT/(theme+'_run.png'),run)
            p.u.SendMessageW(run,16,0,0)
        before=p.metrics()
        for i in range(40):
            p.click([6130,6131,6132][i%3])
            p.click([6141,6140,6142,6143][i%4])
        p.saved()
        after=p.metrics()
        report['resources']={'before':before,'after':after}
        check('same_instance_for_live_changes',p.pid(p.message())==process)
        check('no_gdi_growth_after_switches',after['gdi']<=before['gdi']+2, [before['gdi'],after['gdi']])
        for edge,ident in [('bottom',6141),('top',6140),('left',6142),('right',6143)]:
            p.click(ident);p.saved()
            check(edge+'_work_area_does_not_accumulate',work()==states[edge],work())
        p.click(6131);p.click(6142);p.saved()
        p.stop()
        check('exit_restores_original_work_area',work()==baseline,dict(before=baseline,after=work()))
        p.start(env,['--settings'])
        check('restart_reads_saved_theme_and_position',p.checked(6131) and p.checked(6142))
        p.stop()
        env['MINICON_THEME_MODE']='light';env['MINICON_TASKBAR_POSITION']='right'
        p.start(env,['--settings'])
        check('valid_environment_overrides_are_read_only',p.checked(6130) and p.checked(6143) and not p.u.IsWindowEnabled(p.ctrl(6131)) and not p.u.IsWindowEnabled(p.ctrl(6142)))
        report['passed']=True
    except Exception as error:
        report['passed']=False;report['error']=repr(error)
        raise
    finally:
        p.stop();p.start(original)
        report['normal_session_restored']=bool(p.window('minicon_winmgr_taskbar'))
        report['shell_registration_unchanged']=p.shell_settings()==shell
        (OUT/('layout.json' if layout_only else 'appearance.json')).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps(report,ensure_ascii=True,indent=2))

if __name__ == '__main__':main()
