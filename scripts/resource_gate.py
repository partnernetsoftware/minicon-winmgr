"""Validate resource campaign prerequisites; this is not full release acceptance.

Read only local ignored evidence; re-read samples instead of trusting pass flags.
A passing resource check still requires desktop regressions and human review.
"""
from __future__ import annotations
import argparse
import csv
import hashlib
import io
import json
import math
from pathlib import Path
import re
import statistics
import time
import memory_probe as m

ROOT=Path(__file__).resolve().parents[1]
REQUIRED=('private_bytes','working_set_bytes','handles','gdi','user','threads','cpu_seconds')
GROWTH=('private_bytes','handles','gdi','user','threads')
DISPLAY={'primary_width','primary_height','virtual_left','virtual_top','virtual_width','virtual_height','system_dpi'}
PROFILE=dict(language='en',theme='auto',position='bottom',menu=False,hover_preview=False)


class EvidenceError(ValueError):pass


def require(condition,message):
    if not condition:raise EvidenceError(message)


def number(value):
    require(not isinstance(value,bool) and isinstance(value,(int,float)) and math.isfinite(value),'Invalid numeric evidence')
    return value


def load(path,limit):
    with Path(path).open('rb') as source:data=source.read(limit+1)
    require(len(data)<=limit,'Evidence file exceeds its size budget')
    return data


def digest(path):
    require(256<=Path(path).stat().st_size<=64*1024*1024,'Executable size is outside the accepted range')
    with Path(path).open('rb') as source:return hashlib.file_digest(source,'sha256').hexdigest()


def rising(rows,key):
    # Require growth in every successive fifth of a recovery/soak phase.
    # This is an alarm for review, never proof of absence of a smaller leak.
    if len(rows)<25:return False
    medians=[statistics.median(row[key] for row in rows[len(rows)*i//5:len(rows)*(i+1)//5]) for i in range(5)]
    return all(a<b for a,b in zip(medians,medians[1:]))


def inspect_run(folder,entry,campaign,expected):
    require(entry.get('schema')==2 and entry.get('stop_reason')=='duration_completed','Incomplete or unsupported run')
    require(entry.get('binary_sha256')==expected and digest(folder/'winmgr.exe')==expected,'Run executable differs from the requested artifact')
    interval=number(entry.get('interval_seconds'))
    require(.1<=interval<=5,'Release resource evidence requires sampling at most every five seconds')
    raw=load(folder/'samples.csv',8*1024*1024).decode('utf-8-sig')
    reader=csv.DictReader(io.StringIO(raw))
    require(reader.fieldnames==['elapsed_seconds','phase',*m.METRICS],'Unexpected sample schema')
    rows=[]
    order={'warmup':0,'work':1,'recovery':2}
    for item in reader:
        require(len(rows)<m.MAX_SAMPLES,'Sample count exceeds its budget')
        require(item.get('phase') in order,'Unknown observation phase')
        row=dict(phase=item['phase'])
        for key in ['elapsed_seconds',*m.METRICS]:
            value=item.get(key)
            row[key]=None if value=='' else number(float(value))
        require(row['elapsed_seconds'] is not None and row['elapsed_seconds']>=0,'Invalid sample time')
        for key in REQUIRED:
            require(row[key] is not None and row[key]>=0,'A required metric is missing or negative')
        require(row['private_bytes']>0 and row['working_set_bytes']>0 and row['handles']>0 and row['threads']>0,'A required process metric is zero')
        if row['private_working_set_bytes'] is not None:require(row['private_working_set_bytes']>=0,'Negative private working set')
        if rows:
            gap=row['elapsed_seconds']-rows[-1]['elapsed_seconds']
            require(0<gap<=3*interval+1,'Samples are reordered, duplicated or have an excessive gap')
            require(order[row['phase']]>=order[rows[-1]['phase']],'Observation phases are reordered')
            require(row['cpu_seconds']>=rows[-1]['cpu_seconds'],'CPU time went backwards')
        rows.append(row)
    require(len(rows)==entry.get('sample_count') and len(rows)>=25,'Sample count does not match the report')
    require(rows[0]['elapsed_seconds']<=interval+1,'Initial sampling is missing')
    elapsed=number(entry.get('elapsed_seconds'))
    require(abs(rows[-1]['elapsed_seconds']-elapsed)<=interval+1,'Observation duration is not supported by samples')
    phases=entry.get('phases')
    require(isinstance(phases,list) and all(isinstance(p,dict) for p in phases) and [p.get('phase') for p in phases]==['warmup','work','recovery'],'Missing phase boundaries')
    boundaries=[number(p.get('elapsed_seconds')) for p in phases]
    require(boundaries[0]==0 and 300<=boundaries[1]<boundaries[2]<elapsed,'Warmup or phase order is incomplete')
    require(elapsed-boundaries[2]>=600,'Recovery is shorter than ten minutes')
    for row in rows:
        start=boundaries[order[row['phase']]]
        end=boundaries[order[row['phase']]+1] if row['phase']!='recovery' else elapsed+.01
        require(start-.01<=row['elapsed_seconds']<=end+.01,'Sample phase differs from the recorded timeline')
    require(len(rows)>=math.floor(elapsed/interval)*.9,'Insufficient sample coverage')
    started=number(entry.get('started_utc_ns'));finished=number(entry.get('finished_utc_ns'))
    created=number(entry.get('process_created_filetime'))
    require(all(type(value) is int for value in [started,finished,created]) and started>0 and finished>started and finished<=time.time_ns()+5_000_000_000 and created>116444736000000000,'Missing process or UTC identity')
    require(abs((finished-started)/1e9-elapsed)<=max(5,elapsed*.001),'UTC and monotonic duration disagree')
    age=(started-(created-116444736000000000)*100)/1e9
    require(-1<=age<=60,'Run does not identify a freshly started process')
    scenario=campaign['scenario']
    operations=entry.get('operations',{})
    require(isinstance(operations,dict),'Missing operation counts')
    if scenario=='settings':
        require(number(operations.get('settings_cycles'))>=200 and number(operations.get('settings_changes'))>=1800,'Settings cycles are incomplete')
    if scenario=='windows':
        require(number(operations.get('windows_created'))>=300 and operations.get('windows_destroyed')==operations['windows_created'],'Window creation/cleanup is incomplete')
    if scenario=='idle':require(boundaries[2]-boundaries[1]>=900,'Idle observation is shorter than fifteen minutes')
    groups={phase:[row for row in rows if row['phase']==phase] for phase in order}
    require(len(groups['work'])>=2 and len(groups['warmup'])>=math.floor(300/interval)*.9 and len(groups['recovery'])>=math.floor(600/interval)*.9,'A phase has too few samples')
    for key in GROWTH:
        require(not rising(groups['recovery'],key),f'Sustained recovery growth requires investigation: {key}')
        if scenario=='soak':require(not rising(groups['work'],key),f'Sustained soak growth requires investigation: {key}')
    return dict(subject=entry['subject'],repetition=entry['repetition'],created=created,
        started=started,finished=finished,work_seconds=boundaries[2]-boundaries[1],
        recovery_private=statistics.median(row['private_bytes'] for row in groups['recovery']))


def evaluate(candidate,baseline,campaigns):
    candidate_hash=digest(candidate);baseline_hash=digest(baseline)
    require(candidate_hash!=baseline_hash,'Candidate and baseline are the same executable')
    require(5<=len(campaigns)<=12,'Provide idle/settings/windows comparisons and separate two/eight-hour runs')
    paths=[Path(path).resolve(strict=True) for path in campaigns]
    require(len(set(paths))==len(paths),'Duplicate campaign evidence')
    observed={};environment=None;identities=set();comparisons={};soaks=[];timeline=[]
    for path in paths:
        require(path.is_relative_to((ROOT/'target').resolve()),'Raw resource evidence must stay under ignored target')
        campaign=json.loads(load(path,2*1024*1024))
        require(isinstance(campaign,dict),'Invalid campaign object')
        require(campaign.get('schema')==2 and campaign.get('passed') is True and campaign.get('restored') is True,'Campaign or restoration is incomplete')
        scenario=campaign.get('scenario')
        require(scenario in ['idle','settings','windows','soak'],'Unsupported campaign scenario')
        require(campaign.get('role')=='companion' and campaign.get('declared_profile')=='release','Unexpected process role or declared build profile')
        require(campaign.get('settings_profile')==PROFILE,'Functional profiles differ')
        cohort=campaign.get('cohort');display=campaign.get('display')
        require(isinstance(cohort,str) and re.fullmatch('[a-f0-9]{32}',cohort),'Missing anonymous experiment cohort')
        require(isinstance(display,dict) and set(display)==DISPLAY,'Incomplete numeric display configuration')
        for value in display.values():number(value)
        require(type(campaign.get('repetitions')) is int and campaign.get('repetitions') in [1,2,3],'Invalid campaign repetition count')
        runs=campaign.get('runs');require(isinstance(runs,list) and 1<=len(runs)<=6,'Invalid run list')
        pairs={}
        for entry in runs:
            require(isinstance(entry,dict),'Invalid run object')
            subject=entry.get('subject');repetition=entry.get('repetition')
            require(subject in ['baseline','candidate'] and type(repetition) is int and 1<=repetition<=campaign['repetitions'],'Invalid run identity')
            require((subject,repetition) not in pairs,'Duplicate run identity')
            require(entry.get('scenario')==('resources' if scenario=='windows' else scenario),'Run and campaign scenario disagree')
            current=(cohort,tuple(sorted(display.items())),entry.get('os_build'))
            require(type(entry.get('os_build')) is int and entry['os_build']>0,'Missing OS build')
            if environment is None:environment=current
            require(environment==current,'Experiment environments differ')
            case=path.parent/f'{repetition}-{subject}'
            individual=json.loads(load(case/'summary.json',128*1024))
            require(isinstance(individual,dict),'Invalid per-run summary')
            require(entry==dict(subject=subject,repetition=repetition,**individual),'Campaign and per-run summaries disagree')
            run=inspect_run(case,entry,campaign,candidate_hash if subject=='candidate' else baseline_hash)
            require(run['created'] not in identities,'A process observation was reused')
            identities.add(run['created']);pairs[subject,repetition]=run
            timeline.append((run['started'],run['finished']))
        if scenario!='soak':
            require(scenario not in comparisons,'Provide one complete comparison per scenario')
            require(campaign['repetitions']==3 and set(pairs)=={(subject,rep) for subject in ['baseline','candidate'] for rep in [1,2,3]},'Three fresh-process pairs are required')
            before=statistics.median(pairs['baseline',rep]['recovery_private'] for rep in [1,2,3])
            after=statistics.median(pairs['candidate',rep]['recovery_private'] for rep in [1,2,3])
            delta=after-before
            require(not (delta>2*1024*1024 and delta>before*.1),f'Private commit regression requires investigation: {scenario}')
            comparisons[scenario]=dict(baseline_private=before,candidate_private=after,delta_bytes=delta)
        else:
            soaks.extend(run for (subject,_),run in pairs.items() if subject=='candidate')
        observed[scenario]=observed.get(scenario,0)+len(runs)
    timeline.sort()
    require(all(a[1]<=b[0] for a,b in zip(timeline,timeline[1:])),'Process observations overlap')
    require(set(comparisons)=={'idle','settings','windows'},'Missing controlled comparison scenario')
    two=[run for run in soaks if 7200<=run['work_seconds']<28800]
    eight=[run for run in soaks if run['work_seconds']>=28800]
    require(any(a['finished']<=b['started'] for a in two for b in eight),'Separate completed two-hour then eight-hour candidate observations are required')
    return dict(resource_prerequisites_passed=True,candidate_sha256=candidate_hash,baseline_sha256=baseline_hash,
        scenarios=observed,comparisons=comparisons,full_release_accepted=False,
        limits=['Declared optimization profile and laboratory identity need build/operator review.',
                'These checks cannot rule out every leak or replace desktop, Run, preview, update and fault acceptance.',
                'Raw evidence is not cryptographically authenticated; review the actual execution records.'])


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate',type=Path,required=True)
    parser.add_argument('--baseline',type=Path,required=True)
    parser.add_argument('--campaign',type=Path,action='append',required=True)
    args=parser.parse_args()
    try:result=evaluate(args.candidate,args.baseline,args.campaign)
    except EvidenceError as error:
        print('Resource prerequisites failed: '+str(error));return 1
    except (OSError,ValueError,TypeError,KeyError,RecursionError) as error:
        print('Resource evidence is missing or invalid ('+type(error).__name__+')');return 1
    print(json.dumps(result,indent=2));return 0


if __name__=='__main__':raise SystemExit(main())
