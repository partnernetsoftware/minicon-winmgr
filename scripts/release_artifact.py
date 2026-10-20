"""Build provenance and byte-for-byte promotion of a tested CI artifact."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

ROOT=Path(__file__).resolve().parents[1]
REPO='partnernetsoftware/minicon-winmgr'
ARTIFACT='windows-x64-candidate'
INPUTS=['src','Cargo.toml','Cargo.lock','build.rs','app.manifest','tskmgr.manifest','.cargo']
FIELDS={'version','run_id','source_commit','source_sha256','exe_sha256'}


def command(*args):
    return subprocess.check_output(args,cwd=ROOT,text=True,encoding='utf-8').strip()


def source_digest():
    # Git blobs make this independent of checkout CRLF conversion. Reject
    # local source edits instead of hashing a clean commit over dirty sources.
    subprocess.run(['git','diff','--quiet','HEAD','--',*INPUTS],cwd=ROOT,check=True)
    tree=command('git','ls-tree','-r','HEAD','--',*INPUTS)
    if not tree:raise ValueError('No committed product source found')
    return hashlib.sha256(('git-tree-v1\n'+tree+'\n').encode()).hexdigest()


def digest(path):
    if not 256<=path.stat().st_size<=64*1024*1024:raise ValueError('Invalid executable size')
    with path.open('rb') as file:return hashlib.file_digest(file,'sha256').hexdigest()


def validate(info):
    if set(info)!=FIELDS:raise ValueError('Unexpected provenance fields')
    if not re.fullmatch(r'0\.[0-9]+\.[0-9]+',info['version']):raise ValueError('Invalid version')
    if type(info['run_id']) is not int or info['run_id']<=0:raise ValueError('Invalid producer run')
    for key,length in [('source_commit',40),('source_sha256',64),('exe_sha256',64)]:
        if not isinstance(info[key],str) or not re.fullmatch('[0-9a-f]{'+str(length)+'}',info[key]):
            raise ValueError('Invalid provenance digest')
    return info


def load(path):
    with path.open('rb') as file:data=file.read(4097)
    if len(data)>4096:raise ValueError('Provenance file exceeds its budget')
    return validate(tomllib.loads(data.decode('utf-8')))


def write(path,info):
    validate(info)
    path.write_text('\n'.join(f'{key} = {value if type(value) is int else json.dumps(value)}'
        for key,value in info.items())+'\n',encoding='utf-8')


def version():
    return tomllib.loads((ROOT/'Cargo.toml').read_text(encoding='utf-8'))['package']['version']


def filename(value):return f'minicon-winmgr-v{value}-x86_64-windows.exe'


def prepare(exe,output):
    output.mkdir(parents=True,exist_ok=False)
    info=validate(dict(version=version(),run_id=int(os.environ['GITHUB_RUN_ID']),
        source_commit=command('git','rev-parse','HEAD'),source_sha256=source_digest(),exe_sha256=digest(exe)))
    target=output/filename(info['version']);shutil.copyfile(exe,target)
    write(output/'BUILDINFO.toml',info)
    (output/'SHA256SUMS.txt').write_text(info['exe_sha256']+'  '+target.name+'\n',encoding='ascii')


def promote(manifest,download,output,tag):
    info=load(manifest)
    if info['version']!=version() or tag!='v'+info['version']:raise ValueError('Tag/package/provenance version mismatch')
    if info['source_sha256']!=source_digest():raise ValueError('Product source differs from the tested artifact')
    run=json.loads(command('gh','api',f'repos/{REPO}/actions/runs/{info["run_id"]}'))
    if not (run.get('status')=='completed' and run.get('conclusion')=='success'
            and run.get('head_sha')==info['source_commit'] and run.get('event')=='workflow_dispatch'
            and run.get('path')=='.github/workflows/candidate.yml'
            and run.get('repository',{}).get('full_name')==REPO
            and run.get('head_repository',{}).get('full_name')==REPO):
        raise ValueError('Producer is not a successful candidate workflow in this repository')
    download.mkdir(parents=True,exist_ok=False)
    subprocess.run(['gh','run','download',str(info['run_id']),'--repo',REPO,'--name',ARTIFACT,
        '--dir',str(download)],cwd=ROOT,check=True)
    if load(download/'BUILDINFO.toml')!=info:raise ValueError('Downloaded provenance differs from reviewed manifest')
    exe=download/filename(info['version'])
    if digest(exe)!=info['exe_sha256']:raise ValueError('Downloaded executable digest differs from the tested artifact')
    sums=info['exe_sha256']+'  '+exe.name+'\n'
    if (download/'SHA256SUMS.txt').read_text(encoding='ascii')!=sums:raise ValueError('Checksum file differs')
    output.mkdir(parents=True,exist_ok=False)
    for file in [exe,download/'SHA256SUMS.txt',download/'BUILDINFO.toml']:shutil.copyfile(file,output/file.name)
    print('Verified tested candidate promoted without rebuilding.',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    commands=parser.add_subparsers(dest='mode',required=True)
    prepare_args=commands.add_parser('prepare')
    prepare_args.add_argument('--exe',type=Path,required=True)
    prepare_args.add_argument('--output',type=Path,required=True)
    promote_args=commands.add_parser('promote')
    promote_args.add_argument('--manifest',type=Path,required=True)
    promote_args.add_argument('--download',type=Path,required=True)
    promote_args.add_argument('--output',type=Path,required=True)
    promote_args.add_argument('--tag',required=True)
    args=parser.parse_args()
    if args.mode=='prepare':prepare(args.exe,args.output)
    else:promote(args.manifest,args.download,args.output,args.tag)
