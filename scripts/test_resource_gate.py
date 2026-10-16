"""Synthetic validator tests; they are never resource-acceptance evidence."""
import csv
import json
from pathlib import Path
import shutil
import tempfile
import time
import unittest
from unittest.mock import patch
import resource_gate as g


class GateTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='winmgr-resource-gate-test-')
        self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name);self.base=self.root/'target/evidence';self.base.mkdir(parents=True)
        self.candidate=self.root/'candidate.exe';self.candidate.write_bytes(b'C'*512)
        self.baseline=self.root/'baseline.exe';self.baseline.write_bytes(b'B'*512)
        self.cursor=time.time_ns()-3*86400*10**9
        self.paths=[]
        for scenario,work,repetitions in [('idle',900,3),('settings',300,3),('windows',60,3),('soak',7200,1),('soak',28800,1)]:
            self.make(scenario,work,repetitions)

    def make(self,scenario,work,repetitions):
        folder=self.base/str(len(self.paths));folder.mkdir()
        campaign=dict(schema=2,cohort='b'*32,passed=True,restored=True,scenario=scenario,role='companion',declared_profile='release',
            repetitions=repetitions,settings_profile=g.PROFILE.copy(),
            display=dict(primary_width=1920,primary_height=1080,virtual_left=0,virtual_top=0,virtual_width=1920,virtual_height=1080,system_dpi=96),runs=[])
        for repetition in range(1,repetitions+1):
            for subject in (['candidate'] if scenario=='soak' else ['baseline','candidate']):
                case=folder/f'{repetition}-{subject}';case.mkdir()
                image=self.candidate if subject=='candidate' else self.baseline
                shutil.copy2(image,case/'winmgr.exe')
                elapsed=300+work+600
                with (case/'samples.csv').open('w',newline='',encoding='utf-8') as file:
                    writer=csv.DictWriter(file,fieldnames=['elapsed_seconds','phase',*g.m.METRICS]);writer.writeheader()
                    for second in range(0,elapsed+1,5):
                        writer.writerow(dict(elapsed_seconds=second,phase='warmup' if second<300 else 'work' if second<300+work else 'recovery',
                            private_bytes=(10 if subject=='candidate' else 12)*1048576,working_set_bytes=30*1048576,private_working_set_bytes=None,
                            handles=100,gdi=20,user=20,threads=10,cpu_seconds=second*.01))
                entry=dict(schema=2,scenario='resources' if scenario=='windows' else scenario,binary_sha256=g.digest(image),stop_reason='duration_completed',
                    interval_seconds=5,sample_count=elapsed//5+1,elapsed_seconds=elapsed,os_build=26100,
                    phases=[dict(phase=phase,elapsed_seconds=start) for phase,start in [('warmup',0),('work',300),('recovery',300+work)]],
                    started_utc_ns=self.cursor,finished_utc_ns=self.cursor+elapsed*10**9,
                    process_created_filetime=116444736000000000+(self.cursor-10**9)//100,
                    operations=dict(settings_cycles=200,settings_changes=1800,windows_created=300,windows_destroyed=300))
                self.cursor+=int((elapsed+60)*10**9)
                self.save(case/'summary.json',entry)
                campaign['runs'].append(dict(subject=subject,repetition=repetition,**entry))
        path=folder/'summary.json';self.save(path,campaign);self.paths.append(path)

    def save(self,path,data):path.write_text(json.dumps(data),encoding='utf-8')
    def read(self,path):return json.loads(path.read_text(encoding='utf-8'))
    def evaluate(self):
        with patch.object(g,'ROOT',self.root):return g.evaluate(self.candidate,self.baseline,self.paths)
    def change_run(self,campaign_index,run_index,change):
        path=self.paths[campaign_index];campaign=self.read(path);entry=campaign['runs'][run_index];change(entry)
        case=path.parent/f"{entry['repetition']}-{entry['subject']}"
        self.save(case/'summary.json',{key:value for key,value in entry.items() if key not in ['subject','repetition']})
        self.save(path,campaign)
    def edit_rows(self,index,run,transform):
        path=self.paths[index];entry=self.read(path)['runs'][run];csv_path=path.parent/f"{entry['repetition']}-{entry['subject']}"/'samples.csv'
        with csv_path.open(newline='') as file:
            reader=csv.DictReader(file);fields=reader.fieldnames;rows=list(reader)
        transform(rows)
        with csv_path.open('w',newline='') as file:
            writer=csv.DictWriter(file,fieldnames=fields);writer.writeheader();writer.writerows(rows)

    def test_valid_synthetic_inputs_only_pass_resource_prerequisites(self):
        result=self.evaluate();self.assertTrue(result['resource_prerequisites_passed']);self.assertFalse(result['full_release_accepted'])

    def test_missing_campaigns_and_duplicate_paths_fail(self):
        with patch.object(g,'ROOT',self.root):
            with self.assertRaises(g.EvidenceError):g.evaluate(self.candidate,self.baseline,self.paths[:3])
            with self.assertRaises(g.EvidenceError):g.evaluate(self.candidate,self.baseline,[*self.paths[:4],self.paths[0]])

    def test_same_artifact_or_changed_executable_fails(self):
        with patch.object(g,'ROOT',self.root):
            with self.assertRaises(g.EvidenceError):g.evaluate(self.candidate,self.candidate,self.paths)
        (self.paths[0].parent/'1-candidate/winmgr.exe').write_bytes(b'X'*512)
        with self.assertRaisesRegex(g.EvidenceError,'executable differs'):self.evaluate()

    def test_declared_success_cannot_hide_short_or_missing_soak(self):
        self.change_run(4,0,lambda entry:entry.update(elapsed_seconds=100))
        with self.assertRaises(g.EvidenceError):self.evaluate()

    def test_two_hour_run_must_precede_separate_eight_hour_run(self):
        self.change_run(3,0,lambda entry:entry.update(started_utc_ns=self.cursor,finished_utc_ns=self.cursor+8100*10**9,
            process_created_filetime=116444736000000000+(self.cursor-10**9)//100))
        with self.assertRaisesRegex(g.EvidenceError,'two-hour then eight-hour'):self.evaluate()

    def test_environment_or_restoration_mismatch_fails(self):
        path=self.paths[2];campaign=self.read(path);campaign['restored']=False;self.save(path,campaign)
        with self.assertRaisesRegex(g.EvidenceError,'restoration'):self.evaluate()
        campaign['restored']=True;campaign['cohort']='c'*32;self.save(path,campaign)
        with self.assertRaisesRegex(g.EvidenceError,'environments differ'):self.evaluate()

    def test_missing_metric_excessive_gap_and_backwards_cpu_fail(self):
        csv_path=self.paths[0].parent/'1-candidate/samples.csv';original=csv_path.read_bytes()
        for transform in [lambda rows:rows[5].update(handles=''),lambda rows:rows[5].update(elapsed_seconds='10000'),lambda rows:rows[5].update(cpu_seconds='0')]:
            self.edit_rows(0,1,transform)
            with self.assertRaises(g.EvidenceError):self.evaluate()
            csv_path.write_bytes(original)

    def test_operation_counts_are_verified(self):
        self.change_run(1,0,lambda entry:entry['operations'].update(settings_cycles=1))
        with self.assertRaisesRegex(g.EvidenceError,'cycles'):self.evaluate()

    def test_duplicate_process_fails(self):
        duplicate=self.read(self.paths[0])['runs'][0]['process_created_filetime']
        self.change_run(0,1,lambda entry:entry.update(process_created_filetime=duplicate,started_utc_ns=self.read(self.paths[0])['runs'][0]['started_utc_ns'],finished_utc_ns=self.read(self.paths[0])['runs'][0]['finished_utc_ns']))
        with self.assertRaisesRegex(g.EvidenceError,'reused'):self.evaluate()

    def test_individual_summary_disagreement_fails(self):
        path=self.paths[0].parent/'1-candidate/summary.json'
        summary=self.read(path);summary['sample_count']+=1;self.save(path,summary)
        with self.assertRaisesRegex(g.EvidenceError,'summaries disagree'):self.evaluate()

    def test_private_commit_regression_and_sustained_growth_fail(self):
        for index in [1,3,5]:
            self.edit_rows(0,index,lambda rows:[row.update(private_bytes=str(20*1048576)) for row in rows])
        with self.assertRaisesRegex(g.EvidenceError,'regression'):self.evaluate()

    def test_sustained_resource_growth_fails_even_without_summary_alarm(self):
        self.edit_rows(4,0,lambda rows:[row.update(handles=str(100+int(float(row['elapsed_seconds']))//100)) for row in rows if row['phase']=='work'])
        with self.assertRaisesRegex(g.EvidenceError,'soak growth'):self.evaluate()

    def test_wrong_profile_or_incomplete_repetitions_fail(self):
        path=self.paths[0];campaign=self.read(path);campaign['declared_profile']='debug';self.save(path,campaign)
        with self.assertRaisesRegex(g.EvidenceError,'profile'):self.evaluate()
        campaign['declared_profile']='release';campaign['repetitions']=1;self.save(path,campaign)
        with self.assertRaises(g.EvidenceError):self.evaluate()


if __name__=='__main__':unittest.main()
