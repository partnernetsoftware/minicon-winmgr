"""Reject changed sources and tampered/untrusted candidate promotion."""
import os
import json
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import release_artifact as a


class Provenance(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.old_root=a.ROOT;a.ROOT=self.root
        self.git('init','-q');self.git('config','user.name','Release fixture')
        self.git('config','user.email','fixture@example.invalid')
        (self.root/'src').mkdir();(self.root/'src/main.rs').write_text('fn main() {}\n')
        (self.root/'Cargo.toml').write_text('[package]\nversion = "0.1.10"\n')
        self.git('add','.');self.git('commit','-qm','fixture')
        self.exe=self.root/'fixture.exe';self.exe.write_bytes(b'MZ'+b'\0'*510)

    def tearDown(self):
        a.ROOT=self.old_root;self.temp.cleanup()

    def git(self,*args):
        return subprocess.check_output(['git',*args],cwd=self.root,stderr=subprocess.PIPE)

    def prepare(self,paired=False):
        tool=None
        if paired:
            (self.root/'Cargo.toml').write_text('[package]\nversion = "0.1.16"\n')
            self.git('add','Cargo.toml');self.git('commit','-qm','paired release')
            tool=self.root/'tool.exe';tool.write_bytes(b'MZ'+b't'*510)
        with patch.dict(os.environ,GITHUB_RUN_ID='12345'):
            a.prepare(self.exe,self.root/'artifact',tool)
        return a.load(self.root/'artifact/BUILDINFO.toml')

    def test_public_metadata_and_asset_hash_match(self):
        info=self.prepare()
        self.assertEqual(info['exe_sha256'],a.digest(self.exe))
        self.assertNotIn(str(self.root),(self.root/'artifact/BUILDINFO.toml').read_text())

    def test_official_pair_requires_exact_companion_binding(self):
        (self.root/'Cargo.toml').write_text('[package]\nversion = "0.1.18"\n')
        self.git('add','Cargo.toml');self.git('commit','-qm','bound release')
        tool=self.root/'tool.exe';tool.write_bytes(b'MZ'+b't'*510)
        with patch.dict(os.environ,GITHUB_RUN_ID='12345',MINICON_TSKMGR_SHA256='0'*64):
            with self.assertRaisesRegex(ValueError,'bind the exact companion'):
                a.prepare(self.exe,self.root/'artifact',tool)
            self.assertFalse((self.root/'artifact').exists())
        with patch.dict(os.environ,GITHUB_RUN_ID='12345',MINICON_TSKMGR_SHA256=a.digest(tool)):
            a.prepare(self.exe,self.root/'artifact',tool)
        self.assertEqual(a.load(self.root/'artifact/BUILDINFO.toml')['tskmgr_sha256'],a.digest(tool))

    def test_documentation_commit_preserves_product_fingerprint(self):
        before=a.source_digest()
        (self.root/'notes.md').write_text('Reviewed evidence.\n')
        self.git('add','.');self.git('commit','-qm','docs')
        self.assertEqual(before,a.source_digest())

    def test_source_and_build_inputs_change_fingerprint(self):
        before=a.source_digest()
        (self.root/'app.manifest').write_text('<assembly/>')
        self.git('add','app.manifest');self.git('commit','-qm','manifest')
        self.assertNotEqual(before,a.source_digest())

    def test_dirty_product_source_is_rejected(self):
        (self.root/'src/main.rs').write_text('fn main() { panic!(); }')
        with self.assertRaises(subprocess.CalledProcessError):a.source_digest()

    def test_invalid_or_path_bearing_metadata_is_rejected(self):
        info=self.prepare()
        for changed in [dict(info,run_id=True),dict(info,version='../0.1.10'),dict(info,path='private'),dict(info,exe_sha256='x')]:
            with self.assertRaises(ValueError):a.validate(changed)

    def test_changed_package_or_tag_cannot_promote(self):
        self.prepare()
        with self.assertRaises(ValueError):
            a.promote(self.root/'artifact/BUILDINFO.toml',self.root/'download',self.root/'out','v0.1.9')
        self.assertFalse((self.root/'download').exists())

    def test_untrusted_producer_cannot_download(self):
        self.prepare();real_command=a.command
        def command(*args):return '{}' if args[0]=='gh' else real_command(*args)
        with patch.object(a,'command',side_effect=command),self.assertRaises(ValueError):
            a.promote(self.root/'artifact/BUILDINFO.toml',self.root/'download',self.root/'out','v0.1.10')
        self.assertFalse((self.root/'download').exists())

    def test_changed_source_cannot_promote_even_matching_version(self):
        self.prepare()
        (self.root/'src/main.rs').write_text('fn main() { println!("changed"); }')
        self.git('add','src');self.git('commit','-qm','changed')
        with self.assertRaises(ValueError):
            a.promote(self.root/'artifact/BUILDINFO.toml',self.root/'download',self.root/'out','v0.1.10')

    def promote_fixture(self,tamper=False,paired=False,tamper_tool=False):
        info=self.prepare(paired);real_command=a.command;real_run=subprocess.run
        producer=dict(status='completed',conclusion='success',head_sha=info['source_commit'],
            event='workflow_dispatch',path='.github/workflows/candidate.yml',
            repository=dict(full_name=a.REPO),head_repository=dict(full_name=a.REPO))
        def command(*args):return json.dumps(producer) if args[0]=='gh' else real_command(*args)
        def run(args,**kwargs):
            if args[0]!='gh':return real_run(args,**kwargs)
            target=Path(args[-1])
            for source in (self.root/'artifact').iterdir():shutil.copyfile(source,target/source.name)
            if tamper:(target/a.filename(info['version'])).write_bytes(b'MZ'+b'x'*510)
            if tamper_tool:(target/a.tool_filename(info['version'])).write_bytes(b'MZ'+b'x'*510)
            return subprocess.CompletedProcess(args,0)
        with patch.object(a,'command',side_effect=command),patch.object(subprocess,'run',side_effect=run):
            a.promote(self.root/'artifact/BUILDINFO.toml',self.root/'download',self.root/'out','v'+info['version'])

    def test_download_tampering_cannot_create_release_output(self):
        with self.assertRaises(ValueError):self.promote_fixture(tamper=True)
        self.assertFalse((self.root/'out').exists())

    def test_promotion_copies_the_exact_accepted_bytes(self):
        self.promote_fixture()
        self.assertEqual(self.exe.read_bytes(),(self.root/'out'/a.filename('0.1.10')).read_bytes())

    def test_paired_release_copies_both_exact_payloads(self):
        self.promote_fixture(paired=True)
        info=a.load(self.root/'out/BUILDINFO.toml')
        self.assertEqual(len(a.payloads(info)),2)
        self.assertEqual((self.root/'tool.exe').read_bytes(),
            (self.root/'out'/a.tool_filename('0.1.16')).read_bytes())
        self.assertEqual((self.root/'out/SHA256SUMS.txt').read_text(),a.checksums(info))

    def test_tampered_tool_prevents_all_release_output(self):
        with self.assertRaises(ValueError):self.promote_fixture(paired=True,tamper_tool=True)
        self.assertFalse((self.root/'out').exists())

    def test_new_release_rejects_missing_tool_and_invalid_tool_digest(self):
        info=self.prepare(paired=True)
        missing=dict(info);missing.pop(a.TOOL_FIELD)
        for changed in [missing,dict(info,tskmgr_sha256='x')]:
            with self.assertRaises(ValueError):a.validate(changed)


if __name__=='__main__':unittest.main()
