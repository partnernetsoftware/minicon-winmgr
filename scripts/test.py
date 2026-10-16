"""Select development checks; release runs all automated desktop regressions."""
import argparse
import hashlib
from datetime import datetime, timezone
import json
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--layer', choices=['check', 'unit', 'quality', 'memory', 'soak', 'resources', 'fault', 'side-resize', 'layout', 'appearance', 'interaction', 'taskmgr', 'update', 'settings', 'release'], default='check')
    parser.add_argument('--filter', help='Rust test name/module filter; only with --layer unit')
    parser.add_argument('--pid', type=int, help='Existing test process; only memory/soak')
    parser.add_argument('--duration', type=float, help='Observation seconds; required for soak')
    parser.add_argument('--interval', type=float, default=5, help='Memory observation sample period')
    args = parser.parse_args()
    if args.filter and args.layer != 'unit': parser.error('--filter requires --layer unit')
    if args.layer in ['memory','soak']:
        if not args.pid or args.pid <= 0: parser.error('memory/soak requires --pid')
        if args.layer=='soak' and args.duration is None: parser.error('soak requires explicit --duration')
    elif args.pid is not None or args.duration is not None:
        parser.error('--pid/--duration only apply to memory/soak')
    fingerprint=hashlib.sha256(b''.join(f.read_bytes() for f in sorted((ROOT/'src').glob('*.rs')))).hexdigest()
    report = {'utc':datetime.now(timezone.utc).isoformat(), 'source_sha256':fingerprint, 'layer':args.layer, 'filter':args.filter, 'steps':[], 'passed':False}
    output=ROOT/'target/test-reports'/args.layer
    output.mkdir(parents=True,exist_ok=True)
    def step(name, action):
        started=time.monotonic()
        item={'name':name,'passed':False};report['steps'].append(item)
        print('Running: '+name,flush=True)
        try: action();item['passed']=True
        finally: item['seconds']=round(time.monotonic()-started,3)
    def command(*cmd):
        # Legacy/current instances may still lock target/release through a hard link.
        # Keep release validation outputs separate without stopping other sessions.
        if args.layer=='release' and cmd[0]=='cargo' and cmd[1] in ('test','build'):
            cmd=(*cmd[:2],'--target-dir',str(ROOT/'target/release-validation'),*cmd[2:])
        step(' '.join(cmd),lambda:subprocess.run(cmd,cwd=ROOT,check=True))
    try:
        if args.layer in ['memory','soak']:
            import sys
            import importlib.util
            import memory_probe
            observation=output/('observation-'+str(time.time_ns()))
            report['subject']='external process identified by binary hash in observation summary'
            report['observation_only']=True
            command(sys.executable,'scripts/test_memory_probe.py')
            result=memory_probe.collect(args.pid,args.duration if args.duration is not None else 60,
                                        args.interval,observation,'soak' if args.layer=='soak' else 'idle')
            report['observation_summary']=str((observation/'summary.json').relative_to(ROOT))
            if result['stop_reason']!='duration_completed':raise RuntimeError('Observation ended before requested duration')
            if importlib.util.find_spec('matplotlib') is not None:
                memory_probe.plot(observation)
                report['plot_generated']=True
            else:
                report['plot_generated']=False
                print('Samples saved; matplotlib is unavailable in this Python environment. Generate the plot later with --plot-only.',flush=True)
        if args.layer=='resources':
            import resources_probe
            step('stalled source preview and isolated logging queue measurement',resources_probe.main)
        if args.layer=='fault':
            report['scope']=['pin-file corruption/size/replace failure/external edit/lock',
                             'handoff manifest bounds and confinement',
                             'loopback HTTP size/error responses and URL/hash validation']
            report['not_covered']=['all WinHTTP timeout paths','process handoff and recovery (use update layer)',
                                   'long-run and physical display faults']
            command('cargo','test','quick_launch::tests::pin_mutations')
            command('cargo','test','update_install::tests')
            command('cargo','test','update_http::tests')
        if args.layer in ['check','quality','release']:
            command('cargo','fmt','--all','--','--check')
        if args.layer=='check': command('cargo','check')
        if args.layer in ['quality','release']:
            command('cargo','clippy','--all-targets','--','-D','warnings')
        if args.layer in ['unit','release']:
            command('cargo','test','--all',*(['--release'] if args.layer=='release' else []),*([args.filter] if args.filter else []))
        if args.layer in ['side-resize','layout','appearance','interaction','taskmgr','update','settings','release']:
            command('cargo','build',*(['--release'] if args.layer=='release' else []))
            import settings_probe as settings
            if args.layer=='release':settings.SOURCE=ROOT/'target/release-validation/release/minicon-winmgr.exe'
            if args.layer=='side-resize':
                import side_resize_probe
                step('side taskbar drag resize and persistence',side_resize_probe.main)
            if args.layer in ['update','release']:
                import update_probe
                step('manual update and rollback handoff',lambda:update_probe.main(release=args.layer=='release'))
            if args.layer in ['taskmgr','release']:
                import taskmgr_probe
                step('existing Task Manager activation',taskmgr_probe.main)
            if args.layer in ['interaction','release']:
                import interaction_probe
                step('on-demand startup and daily interactions',interaction_probe.main)
            if args.layer in ['settings','release']:
                settings.REPORT=output/'settings.json'
                step('native settings'+(' + full Run regression' if args.layer=='release' else ''),lambda:settings.main(build=False,include_run_regression=args.layer=='release'))
            if args.layer in ['layout','appearance','release']:
                import appearance_probe
                if args.layer=='release':step('compact layout',lambda:appearance_probe.main(layout_only=True))
                step('compact layout' if args.layer=='layout' else 'native themes and four-edge docking',lambda:appearance_probe.main(layout_only=args.layer=='layout'))
                report['appearance_report']='target/industrial-acceptance/'+('layout.json' if args.layer=='layout' else 'appearance.json')
        report['passed']=True
    except Exception as error:
        report['error']=repr(error)
        raise
    finally:
        (output/'summary.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        if args.filter:
            (output/(args.filter.replace('::','_')+'.json')).write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print('Report: '+str(output/'summary.json'),flush=True)

if __name__=='__main__':main()
