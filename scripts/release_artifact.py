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
import zipfile

ROOT=Path(__file__).resolve().parents[1]
REPO='partnernetsoftware/minicon-winmgr'
ARTIFACT='windows-x64-candidate'
INPUTS=['src','Cargo.toml','Cargo.lock','build.rs','app.manifest','tskmgr.manifest','.cargo','scripts/build_candidate.py']
FIELDS={'version','run_id','source_commit','source_sha256','exe_sha256'}
TOOL_FIELD='tskmgr_sha256'
ZIP_FIELD='zip_sha256'
ZIP_SINCE=(0,1,24)
ZIP_DATE=(1980,1,1,0,0,0)


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
    if not FIELDS<=set(info)<=FIELDS|{TOOL_FIELD,ZIP_FIELD}:raise ValueError('Unexpected provenance fields')
    if not re.fullmatch(r'0\.[0-9]+\.[0-9]+',info['version']):raise ValueError('Invalid version')
    if type(info['run_id']) is not int or info['run_id']<=0:raise ValueError('Invalid producer run')
    for key,length in [('source_commit',40),('source_sha256',64),('exe_sha256',64)]:
        if not isinstance(info[key],str) or not re.fullmatch('[0-9a-f]{'+str(length)+'}',info[key]):
            raise ValueError('Invalid provenance digest')
    if TOOL_FIELD in info and (not isinstance(info[TOOL_FIELD],str)
            or not re.fullmatch('[0-9a-f]{64}',info[TOOL_FIELD])):
        raise ValueError('Invalid tool digest')
    if ZIP_FIELD in info and (not isinstance(info[ZIP_FIELD],str)
            or not re.fullmatch('[0-9a-f]{64}',info[ZIP_FIELD])):
        raise ValueError('Invalid zip digest')
    if tuple(map(int,info['version'].split('.'))) >= (0,1,16) and TOOL_FIELD not in info:
        raise ValueError('This release requires the task tool')
    if tuple(map(int,info['version'].split('.'))) >= ZIP_SINCE and ZIP_FIELD not in info:
        raise ValueError('This release requires the zip package')
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


def tool_filename(value):return f'minicon-tskmgr-v{value}-x86_64-windows.exe'


def zip_filename(value):return f'minicon-winmgr-v{value}-x86_64-windows.zip'


def build_zip(exe,path,value):
    # One stored-name entry with fixed metadata so identical exe bytes give an
    # identical archive (for the same zlib). No directories and no file-type
    # bits; zipfile itself replaces a zero external_attr with 0o600<<16.
    entry=zipfile.ZipInfo(filename(value),date_time=ZIP_DATE)
    entry.compress_type=zipfile.ZIP_DEFLATED;entry.create_system=0;entry.external_attr=0
    with zipfile.ZipFile(path,'x') as archive:
        archive.writestr(entry,exe.read_bytes(),compress_type=zipfile.ZIP_DEFLATED,compresslevel=9)


def payloads(info):
    result=[(filename(info['version']),info['exe_sha256'])]
    if ZIP_FIELD in info:result.append((zip_filename(info['version']),info[ZIP_FIELD]))
    if TOOL_FIELD in info:result.append((tool_filename(info['version']),info[TOOL_FIELD]))
    return result


def checksums(info):return ''.join(f'{digest}  {name}\n' for name,digest in payloads(info))


def prepare(exe,output,tool=None):
    info=dict(version=version(),run_id=int(os.environ['GITHUB_RUN_ID']),
        source_commit=command('git','rev-parse','HEAD'),source_sha256=source_digest(),exe_sha256=digest(exe))
    if tool is not None:info[TOOL_FIELD]=digest(tool)
    packaged=tuple(map(int,info['version'].split('.'))) >= ZIP_SINCE
    # The zip digest exists only after the archive is built; check everything else first.
    validate(dict(info,**{ZIP_FIELD:'0'*64}) if packaged else info)
    if tuple(map(int,info['version'].split('.'))) >= (0,1,18):
        if os.environ.get('MINICON_TSKMGR_SHA256') != info.get(TOOL_FIELD):
            raise ValueError('Official build must bind the exact companion digest; use build_candidate.py')
    output.mkdir(parents=True,exist_ok=False)
    shutil.copyfile(exe,output/filename(info['version']))
    if tool is not None:shutil.copyfile(tool,output/tool_filename(info['version']))
    if packaged:
        build_zip(output/filename(info['version']),output/zip_filename(info['version']),info['version'])
        info[ZIP_FIELD]=digest(output/zip_filename(info['version']))
    write(output/'BUILDINFO.toml',info)
    (output/'SHA256SUMS.txt').write_text(checksums(info),encoding='ascii')


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
    for name,expected in payloads(info):
        if digest(download/name)!=expected:raise ValueError('Downloaded payload digest differs from the tested artifact')
    if (download/'SHA256SUMS.txt').read_text(encoding='ascii')!=checksums(info):raise ValueError('Checksum file differs')
    output.mkdir(parents=True,exist_ok=False)
    for name in [*(name for name,_ in payloads(info)),'SHA256SUMS.txt','BUILDINFO.toml']:
        shutil.copyfile(download/name,output/name)
    print('Verified tested candidate promoted without rebuilding.',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    commands=parser.add_subparsers(dest='mode',required=True)
    prepare_args=commands.add_parser('prepare')
    prepare_args.add_argument('--exe',type=Path,required=True)
    prepare_args.add_argument('--tool',type=Path)
    prepare_args.add_argument('--output',type=Path,required=True)
    promote_args=commands.add_parser('promote')
    promote_args.add_argument('--manifest',type=Path,required=True)
    promote_args.add_argument('--download',type=Path,required=True)
    promote_args.add_argument('--output',type=Path,required=True)
    promote_args.add_argument('--tag',required=True)
    args=parser.parse_args()
    if args.mode=='prepare':prepare(args.exe,args.output,args.tool)
    else:promote(args.manifest,args.download,args.output,args.tag)
