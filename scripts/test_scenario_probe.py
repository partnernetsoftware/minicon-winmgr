"""Driver lifecycle checks with fake processes; no desktop or shell changes."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import scenario_probe as s


def arguments(root,**changes):
    result=argparse.Namespace(candidate=root/'candidate.exe',restore_exe=root/'normal.exe',baseline=None,
        scenario='idle',repetitions=1,warmup=0,observe=.001,recovery=0,interval=.1,iterations=None,fail_at=None)
    for key,value in changes.items():setattr(result,key,value)
    return result


class Fixture:
    def __init__(self,root):
        self.root=root.resolve()
        (root/'normal.exe').write_bytes(b'original fixture executable')
        (root/'candidate.exe').write_bytes(b'candidate fixture executable')
        self.current=10;self.paths={10:root/'normal.exe'};self.children={};self.events=[]
        self.fail_stop=False;self.fail_restore=False;self.fail_preflight=False
        self.u=self

    def image(self,process):return self.paths[process.pid].resolve()
    def same_user_session(self,process):return True
    def workarea(self):return [18,0,1920,1080]
    def display(self):return dict(primary_width=1920,primary_height=1080,system_dpi=96)
    def pump(self):pass
    def close(self,process):
        self.events.append(('close',process.pid))
        if self.fail_stop:raise RuntimeError('original refused exit')
        if process.pid in self.children:self.children[process.pid].code=0
        self.current=0
    def PostMessageW(self,hwnd,*args):
        if hwnd not in self.children:raise AssertionError('must only signal an owned test child')
        self.children[hwnd].code=0;self.current=0;self.events.append(('child_close',hwnd))
    def preflight(self,exe,env):
        if self.fail_preflight:raise RuntimeError('executable preflight failed')
    def start(self,exe,env):
        if exe==self.root/'normal.exe' and self.fail_restore:raise RuntimeError('restore failed')
        pid=20+len(self.children)
        child=Child(self,pid);self.children[pid]=child;self.paths[pid]=exe;self.current=pid
        self.events.append(('start',exe.name))
        return child
    def Process(self,pid):return Process(self,pid)


class Child:
    def __init__(self,fixture,pid):self.fixture=fixture;self.pid=pid;self.code=None
    def poll(self):return self.code
    def wait(self,timeout):
        if self.code is None:raise AssertionError('child still running')
        return self.code
    def kill(self):self.code=-1;self.fixture.events.append(('kill',self.pid));self.fixture.current=0


class Process:
    def __init__(self,fixture,pid):
        self.fixture=fixture;self.pid=pid;self.created=134000000000000000+pid;self.image_hash=s.digest(fixture.paths[pid])
    def close(self):pass
    def sample(self):return {key:10 for key in s.m.METRICS}


class DriverTests(unittest.TestCase):
    def run_case(self,root,fixture,args):
        with patch.object(s,'ROOT',root),patch.object(s,'Desktop',return_value=fixture),patch.object(s.m,'Process',side_effect=fixture.Process),patch.object(s.p,'message',side_effect=lambda:fixture.current),patch.object(s.p,'pid',side_effect=lambda hwnd:hwnd),patch.object(s.p,'shell_settings',return_value={'user':('original',1)}),contextlib.redirect_stdout(io.StringIO()):
            return s.main(args)

    def report(self,root):
        files=list((root/'target/test-reports/scenarios').glob('*/summary.json'))
        self.assertEqual(len(files),1)
        return json.loads(files[0].read_text())

    def test_bounds_and_soak_cannot_be_shortened_into_smoke(self):
        root=Path('.')
        for change in [dict(warmup=float('nan')),dict(repetitions=0),dict(interval=.01),dict(iterations=1001),dict(scenario='soak',observe=2),dict(observe=86401),dict(observe=10000,interval=.1)]:
            with self.assertRaises(ValueError):s.options(arguments(root,**change))
        args=arguments(root,scenario='windows');s.options(args);self.assertEqual(args.iterations,300)

    def test_preflight_identity_mismatch_does_not_stop_original(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root)
            (root/'other.exe').write_bytes(b'other')
            with self.assertRaisesRegex(RuntimeError,'does not match'):
                self.run_case(root,fixture,arguments(root,restore_exe=root/'other.exe'))
            self.assertEqual(fixture.events,[]);self.assertEqual(fixture.current,10)

    def test_executable_preflight_failure_preserves_original(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root);fixture.fail_preflight=True
            with self.assertRaisesRegex(RuntimeError,'preflight failed'):
                self.run_case(root,fixture,arguments(root))
            self.assertEqual(fixture.events,[])
            self.assertTrue(self.report(root)['original_preserved'])

    def test_failure_after_stopping_original_restores_without_replacing_file(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root);before=s.digest(root/'normal.exe')
            with self.assertRaisesRegex(RuntimeError,'Injected'):
                self.run_case(root,fixture,arguments(root,fail_at='after-stop'))
            report=self.report(root)
            self.assertTrue(report['restored']);self.assertFalse(report['passed'])
            self.assertEqual(s.digest(root/'normal.exe'),before)
            self.assertEqual(fixture.events,[('close',10),('start','normal.exe')])

    def test_failure_after_test_start_closes_only_owned_child_then_restores(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root)
            with self.assertRaisesRegex(RuntimeError,'Injected'):
                self.run_case(root,fixture,arguments(root,fail_at='after-start'))
            self.assertTrue(self.report(root)['restored'])
            self.assertEqual(fixture.events,[('close',10),('start','winmgr.exe'),('child_close',20),('start','normal.exe')])

    def test_restore_failure_is_recorded_as_incomplete(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root);fixture.fail_restore=True
            with self.assertRaisesRegex(RuntimeError,'restore failed'):
                self.run_case(root,fixture,arguments(root,fail_at='after-stop'))
            report=self.report(root)
            self.assertFalse(report['restored']);self.assertFalse(report['passed'])
            self.assertTrue((root/'normal.exe').exists())

    def test_restore_failure_is_detected_with_a_noncanonical_temporary_root(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);alias=root/'..'/root.name
            fixture=Fixture(alias);fixture.fail_restore=True
            with self.assertRaisesRegex(RuntimeError,'restore failed'):
                self.run_case(alias,fixture,arguments(alias,fail_at='after-stop'))
            self.assertFalse(self.report(root)['restored'])

    def test_original_refusing_exit_is_not_killed_or_restarted(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root);fixture.fail_stop=True
            with self.assertRaisesRegex(RuntimeError,'refused'):
                self.run_case(root,fixture,arguments(root))
            self.assertEqual(fixture.events,[('close',10)]);self.assertEqual(fixture.current,10)
            self.assertTrue(self.report(root)['original_preserved'])

    def test_completed_smoke_records_both_subjects_without_acceptance_claim(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root)
            report=self.run_case(root,fixture,arguments(root))
            self.assertTrue(report['passed']);self.assertTrue(report['restored'])
            self.assertFalse(report['protocol_minimums_met'])
            self.assertEqual([run['subject'] for run in report['runs']],['baseline','candidate'])
            self.assertTrue(all(not run['long_run_passed'] for run in report['runs']))
            self.assertTrue(all(run['stop_reason']=='duration_completed' for run in report['runs']))
            self.assertTrue(all(run['sample_count']>=3 for run in report['runs']))


    def test_subject_order_alternates_between_fresh_process_pairs(self):
        with tempfile.TemporaryDirectory(prefix='winmgr-scenario-test-') as folder:
            root=Path(folder);fixture=Fixture(root)
            report=self.run_case(root,fixture,arguments(root,repetitions=2))
            self.assertEqual([run['subject'] for run in report['runs']],['baseline','candidate','candidate','baseline'])


if __name__=='__main__':unittest.main()
