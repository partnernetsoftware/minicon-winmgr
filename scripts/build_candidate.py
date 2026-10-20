"""Build the companion first, then bind its exact digest into the shell."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import release_artifact as artifact

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target-dir', type=Path, default=ROOT/'target')
    parser.add_argument('--output', type=Path, help='Prepare CI provenance and attachments after building')
    args = parser.parse_args()
    target = args.target_dir.resolve()
    environment = os.environ.copy()
    environment.pop('MINICON_TSKMGR_SHA256', None)
    command = ['cargo', 'build', '--release', '--locked', '--target-dir', str(target), '--bin']
    subprocess.run([*command, 'minicon-tskmgr'], cwd=ROOT, env=environment, check=True)
    tool = target/'release/minicon-tskmgr.exe'
    bound = artifact.digest(tool)
    environment['MINICON_TSKMGR_SHA256'] = bound
    subprocess.run([*command, 'minicon-winmgr'], cwd=ROOT, env=environment, check=True)
    if artifact.digest(tool) != bound:
        raise RuntimeError('Companion changed while building the shell')
    print('Built Winmgr bound to Tskmgr SHA-256 '+bound, flush=True)
    if args.output:
        subprocess.run([sys.executable, str(ROOT/'scripts/release_artifact.py'), 'prepare',
                        '--exe', str(target/'release/minicon-winmgr.exe'), '--tool', str(tool),
                        '--output', str(args.output.resolve())],
                       cwd=ROOT, env=environment, check=True)


if __name__ == '__main__':
    main()
