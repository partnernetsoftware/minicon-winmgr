"""Select development checks; release runs all automated desktop regressions."""
import argparse
import hashlib
from datetime import datetime, timezone
import json
import subprocess
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DESKTOP_LAYERS={'side-resize','fullscreen','layout','appearance','interaction','taskmgr','update','settings','release'}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--layer', choices=['check', 'unit', 'quality', 'memory', 'soak', 'resources', 'resource-gate', 'update-memory', 'diagnostics', 'fault', 'side-resize', 'fullscreen', 'layout', 'appearance', 'interaction', 'taskmgr', 'update', 'settings', 'release'], default='check')
    parser.add_argument('--filter', help='Rust test name/module filter; only with --layer unit')
    parser.add_argument('--pid', type=int, help='Existing test process; only memory/soak')
    parser.add_argument('--duration', type=float, help='Observation seconds; required for soak')
    parser.add_argument('--interval', type=float, default=5, help='Memory observation sample period')
    parser.add_argument('--release-binary', type=Path, help='Exact optimized artifact to accept; resource-gate/release only')
    parser.add_argument('--baseline', type=Path, help='Comparison baseline artifact; resource-gate/release only')
    parser.add_argument('--campaign', type=Path, action='append', help='Local resource campaign summary; repeat for each campaign')
    parser.add_argument('--restore-exe', type=Path, help='Required for desktop layers: actual running shell executable to restore')
    parser.add_argument('--candidate-exe', type=Path, help='Existing binary for a development desktop layer; skips rebuilding')
    parser.add_argument('--legacy-exe', type=Path, help='Official historical 0.1.4 executable; required for release compatibility checks')
    args = parser.parse_args()
    if args.filter and args.layer != 'unit': parser.error('--filter requires --layer unit')
    if args.layer in DESKTOP_LAYERS:
        if not args.restore_exe:parser.error('desktop layers require --restore-exe matching the actual running instance')
        args.restore_exe=args.restore_exe.resolve(strict=True)
    elif args.restore_exe:parser.error('--restore-exe only applies to desktop layers')
    if args.candidate_exe:
        if args.layer not in DESKTOP_LAYERS or args.layer=='release':parser.error('--candidate-exe only applies to development desktop layers')
        args.candidate_exe=args.candidate_exe.resolve(strict=True)
    if args.layer=='release' and (not args.restore_exe or not args.legacy_exe):parser.error('release requires --restore-exe and --legacy-exe')
    if args.legacy_exe:
        if args.layer!='release':parser.error('--legacy-exe only applies to release')
        args.legacy_exe=args.legacy_exe.resolve(strict=True)
    if args.layer in ['resource-gate','release']:
        if not args.release_binary or not args.baseline:
            parser.error('resource-gate/release requires --release-binary and --baseline')
        if args.layer=='resource-gate' and not args.campaign:
            parser.error('resource-gate requires repeated --campaign evidence')
        args.release_binary=args.release_binary.resolve(strict=True)
        args.baseline=args.baseline.resolve(strict=True)
    elif args.release_binary or args.baseline or args.campaign:
        parser.error('--release-binary/--baseline/--campaign only apply to resource-gate/release')
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
        if args.layer=='release':
            import release_artifact
            report['tested_binary_sha256']=release_artifact.digest(args.release_binary)
            report['long_observation_completed']=False
            report['resource_scope']='Extended resource certification is separate; no whole-application memory or long-run stability claim.'
        if args.layer=='resource-gate' or (args.layer=='release' and args.campaign):
            import resource_gate
            def resources():
                result=resource_gate.evaluate(args.release_binary,args.baseline,args.campaign)
                report['resource_prerequisites']=result
                report['long_observation_completed']=True
                report['tested_binary_sha256']=result['candidate_sha256']
            step('controlled resource prerequisites (not complete release acceptance)',resources)
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
            step('stalled source preview, asynchronous owned icons, and isolated logging queue measurement',resources_probe.main)
        if args.layer=='update-memory':
            import update_memory_probe
            step('bounded update staging memory comparison',update_memory_probe.main)
        if args.layer=='diagnostics':
            import diagnostics_probe
            command('cargo','test','diagnostic')
            step('opt-in diagnostics native language and theme matrix',diagnostics_probe.main)
        if args.layer=='fault':
            report['scope']=['pin-file corruption/size/replace failure/external edit/lock',
                             'handoff manifest bounds, streamed staging failures and confinement',
                             'loopback HTTP size/truncation, silent/trickling response cancellation, callback lifetime, sink errors and bounded incremental hashing',
                             'update worker panic, coalesced progress, hidden-UI terminal delivery, exit cancellation and retry']
            report['not_covered']=['public DNS/proxy/TLS failure matrix','process handoff and recovery (use update layer)',
                                   'long-run and physical display faults']
            command('cargo','test','quick_launch::tests::pin_mutations')
            command('cargo','test','update_install::tests')
            command('cargo','test','update_http::tests')
            command('cargo','test','update::tests')
            command('cargo','test','update_stream::tests')
        if args.layer in ['check','quality','release']:
            command('cargo','fmt','--all','--','--check')
        if args.layer=='check': command('cargo','check')
        if args.layer in ['quality','release']:
            command('cargo','clippy','--all-targets','--','-D','warnings')
        if args.layer=='quality':
            command('cargo','test','logging::tests')
        if args.layer in ['quality','release']:
            import sys
            command(sys.executable,'scripts/test_scenario_probe.py')
            command(sys.executable,'scripts/test_resource_gate.py')
            command(sys.executable,'scripts/test_release_artifact.py')
            command(sys.executable,'scripts/test_desktop_layers.py')
        if args.layer in ['unit','release']:
            command('cargo','test','--all',*(['--release'] if args.layer=='release' else []),*([args.filter] if args.filter else []))
        if args.layer=='release':
            import resources_probe
            import update_memory_probe
            step('optimized component resource ownership and stalled-source regressions',lambda:resources_probe.main(release=True))
            step('optimized bounded update staging memory comparison',lambda:update_memory_probe.main(release=True))
        if args.layer in DESKTOP_LAYERS:
            if args.layer=='release':
                command('cargo','build','--release')
                candidate=args.release_binary
            elif args.candidate_exe:
                candidate=args.candidate_exe
            else:
                target=ROOT/'target/desktop-validation'
                command('cargo','build','--target-dir',str(target))
                candidate=target/'debug/minicon-winmgr.exe'
            import settings_probe as settings
            def desktop_checks():
                if args.layer=='release':
                    import diagnostics_probe
                    step('optimized diagnostic fixture language/theme matrix (separate test executable)',lambda:diagnostics_probe.main(release=True))
                if args.layer in ['side-resize','release']:
                    import side_resize_probe
                    step('side taskbar drag resize and persistence',side_resize_probe.main)
                if args.layer in ['fullscreen','release']:
                    import fullscreen_yield_probe
                    step('fullscreen app yield and topmost restoration',fullscreen_yield_probe.main)
                if args.layer in ['update','release']:
                    import update_probe
                    step('manual update and rollback handoff',lambda:update_probe.main(release=args.layer=='release'))
                    step('manual update from a zip release archive',lambda:update_probe.main(archive=True))
                    if args.layer=='release':
                        step('published baseline updater to exact candidate and rollback',lambda:update_probe.main(release=True,previous_artifact=args.baseline))
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
                if args.layer=='release':
                    import scenario_probe
                    from types import SimpleNamespace
                    import native_feature_probe
                    step('actual candidate diagnostic entry and composed hover',native_feature_probe.main)
                    for scenario in ['docking','shortcuts']:
                        options=SimpleNamespace(candidate=args.release_binary,restore_exe=settings.LIVE,
                            baseline=args.release_binary,subject='candidate',profile='release',scenario=scenario,
                            repetitions=1,warmup=0,observe=1,recovery=0,interval=.2,iterations=1,fail_at=None)
                        step('exact candidate native '+scenario,lambda options=options:scenario_probe.main(options))
            import desktop_session
            import release_artifact
            tested_hash=release_artifact.digest(candidate)
            if report.get('tested_binary_sha256',tested_hash)!=tested_hash:
                raise RuntimeError('Selected candidate differs from resource acceptance')
            report['tested_binary_sha256']=tested_hash
            report['original_shell_restored']=False
            isolation=None
            try:
                with desktop_session.isolated(candidate,args.restore_exe) as isolation:desktop_checks()
            finally:
                if isolation is not None:report['original_shell_restored']=isolation.get('restored',False)
            if args.layer=='release':
                import instance_switch_probe
                for language in ['en','zh']:
                    step('historical instance switch '+language,lambda language=language:instance_switch_probe.main(args.release_binary,args.legacy_exe,args.restore_exe,language))
                import resource_gate
                if resource_gate.digest(args.release_binary)!=report['tested_binary_sha256']:
                    raise RuntimeError('Candidate changed during acceptance')
                report['full_release_accepted']=False
                report['remaining']='Review raw results, screenshots, coverage limits and exact draft asset before publication.'
            if release_artifact.digest(candidate)!=tested_hash:
                raise RuntimeError('Candidate changed during desktop checks')
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
