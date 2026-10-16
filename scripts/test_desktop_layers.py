"""Layer dispatch and failure cleanup with fake builds/windows; no desktop I/O."""
from contextlib import contextmanager, redirect_stderr, redirect_stdout
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]


class DesktopLayers(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        (self.root/'src').mkdir();(self.root/'src/main.rs').write_text('fn main() {}')
        self.restore=self.root/'normal.exe';self.restore.write_bytes(b'MZ'+b'\0'*510)
        self.candidate=self.root/'candidate.exe';self.candidate.write_bytes(b'MZ'+b'c'*510)
        spec=importlib.util.spec_from_file_location('layer_dispatch_fixture',ROOT/'scripts/test.py')
        self.module=importlib.util.module_from_spec(spec);spec.loader.exec_module(self.module)
        self.module.ROOT=self.root;self.events=[];self.commands=[];self.fail=False
        settings=types.ModuleType('settings_probe')
        def desktop_checks(**options):
            self.events.append(('checks',options))
            if self.fail:raise RuntimeError('Injected probe failure')
        settings.main=desktop_checks
        isolation=types.ModuleType('desktop_session')
        @contextmanager
        def isolated(candidate,restore):
            self.events.append(('enter',candidate,restore));result={'restored':False}
            try:yield result
            finally:
                result['restored']=True;self.events.append(('restored',))
        isolation.isolated=isolated
        self.modules={'settings_probe':settings,'desktop_session':isolation}

    def tearDown(self):self.temp.cleanup()

    def build(self,args,**options):
        self.commands.append(args)
        self.assertEqual(args[:2],('cargo','build'))
        target=Path(args[args.index('--target-dir')+1])
        self.assertEqual(target,self.root/'target/desktop-validation')
        binary=target/'debug/minicon-winmgr.exe';binary.parent.mkdir(parents=True)
        binary.write_bytes(self.candidate.read_bytes())
        return subprocess.CompletedProcess(args,0)

    def invoke(self,*args):
        with patch.object(sys,'argv',['test.py',*map(str,args)]), \
                patch.dict(sys.modules,self.modules), \
                patch.object(self.module.subprocess,'run',side_effect=self.build), \
                redirect_stdout(io.StringIO()),redirect_stderr(io.StringIO()):
            self.module.main()

    def test_every_host_replacing_layer_requires_restore_before_actions(self):
        for layer in self.module.DESKTOP_LAYERS:
            with self.subTest(layer=layer),self.assertRaises(SystemExit) as error:
                self.invoke('--layer',layer)
            self.assertEqual(error.exception.code,2)
        self.assertEqual(self.commands,[]);self.assertEqual(self.events,[])
        self.assertFalse((self.root/'target').exists())

    def test_missing_restore_file_is_rejected_before_build_or_window_actions(self):
        with self.assertRaises(FileNotFoundError):
            self.invoke('--layer','settings','--restore-exe',self.root/'missing.exe')
        self.assertEqual(self.commands,[]);self.assertEqual(self.events,[])

    def test_development_build_is_separate_and_runs_inside_restoration_context(self):
        self.invoke('--layer','settings','--restore-exe',self.restore)
        self.assertEqual(len(self.commands),1)
        self.assertEqual([event[0] for event in self.events],['enter','checks','restored'])
        self.assertEqual(self.events[0][1],self.root/'target/desktop-validation/debug/minicon-winmgr.exe')
        self.assertEqual(self.events[0][2],self.restore)
        report=json.loads((self.root/'target/test-reports/settings/summary.json').read_text())
        self.assertTrue(report['passed'] and report['original_shell_restored'])

    def test_prebuilt_candidate_skips_build_and_records_its_actual_digest(self):
        self.invoke('--layer','settings','--restore-exe',self.restore,'--candidate-exe',self.candidate)
        self.assertEqual(self.commands,[]);self.assertEqual(self.events[0][1],self.candidate)
        report=json.loads((self.root/'target/test-reports/settings/summary.json').read_text())
        self.assertEqual(report['tested_binary_sha256'],hashlib.sha256(self.candidate.read_bytes()).hexdigest())

    def test_probe_failure_records_failure_after_context_restores_original(self):
        self.fail=True
        with self.assertRaisesRegex(RuntimeError,'Injected'):
            self.invoke('--layer','settings','--restore-exe',self.restore,'--candidate-exe',self.candidate)
        report=json.loads((self.root/'target/test-reports/settings/summary.json').read_text())
        self.assertFalse(report['passed']);self.assertTrue(report['original_shell_restored'])
        self.assertEqual(self.events[-1],('restored',))

    def test_prebuilt_candidate_is_not_allowed_to_bypass_release_resource_gate(self):
        with self.assertRaises(SystemExit) as error:
            self.invoke('--layer','release','--restore-exe',self.restore,'--candidate-exe',self.candidate)
        self.assertEqual(error.exception.code,2)
        self.assertEqual(self.commands,[]);self.assertEqual(self.events,[])


if __name__=='__main__':unittest.main()
