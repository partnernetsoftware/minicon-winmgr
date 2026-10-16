"""Confine legacy desktop probes to an owned executable and restore the original."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import time
import memory_probe as m
import scenario_probe as s
import settings_probe as p


@contextmanager
def isolated(candidate,restore):
    candidate=Path(candidate).resolve(strict=True);restore=Path(restore).resolve(strict=True)
    desktop=s.Desktop();original=m.Process(p.pid(p.message()))
    env=os.environ.copy();shell=p.shell_settings();area=desktop.workarea()
    folder=s.ROOT/'target/test-reports/desktop-session'/str(time.time_ns())
    folder.mkdir(parents=True)
    live=folder/'minicon-winmgr.exe'
    prior_live,prior_source=p.LIVE,p.SOURCE
    closed=False
    report=dict(passed=False,restored=False)
    try:
        if desktop.image(original)!=restore or not desktop.same_user_session(original):
            raise RuntimeError('Original shell identity does not match the explicit restore artifact')
        expected=s.digest(restore)
        if original.image_hash!=expected:raise RuntimeError('Original executable changed')
        report.update(original_sha256=expected,candidate_sha256=s.digest(candidate))
        private_env={key:value for key,value in env.items() if not key.startswith('MINICON_')}
        private_env.update(MINICON_LOG_DIR=str(folder/'preflight-logs'))
        for image in dict.fromkeys([candidate,restore]):desktop.preflight(image,private_env)
        shutil.copy2(restore,live)
        (folder/'restore-local.json').write_text(json.dumps(dict(executable=str(restore),sha256=expected)),encoding='utf-8')
        desktop.close(original);closed=True
        p.LIVE,p.SOURCE=live,candidate
        yield
        report['passed']=True
    finally:
        try:
            if closed:
                current=p.message()
                if current:
                    process=m.Process(p.pid(current))
                    try:
                        # Legacy update handoffs can launch another owned target path.
                        if not desktop.same_user_session(process) or not desktop.image(process).is_relative_to((s.ROOT/'target').resolve()):
                            raise RuntimeError('Refusing to close an unrelated desktop instance')
                        desktop.close(process)
                    finally:process.close()
                if s.digest(restore)!=expected:raise RuntimeError('Original executable changed during acceptance')
                desktop.start(restore,env)
                report['restored']=desktop.workarea()==area and p.shell_settings()==shell
                if not report['restored']:raise RuntimeError('Desktop restoration did not match its original state')
        finally:
            p.LIVE,p.SOURCE=prior_live,prior_source
            original.close()
            if not report['restored']:report['passed']=False
            (folder/'summary.json').write_text(json.dumps(report,indent=2),encoding='utf-8')
