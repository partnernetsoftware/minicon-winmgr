"""Build the companion first, then bind its exact digest into the shell.

Signing sits between the two builds: the shell embeds the companion's digest, so a
companion signed after the shell was built would no longer match what the shell
expects. The phases therefore run in this order when signing is enabled:

    1. --tool-only              build Tskmgr, print its digest
    2. (sign Tskmgr outside this script)
    3. --signed-tool PATH       bind the signed Tskmgr digest and build Winmgr
    4. (sign Winmgr outside this script)
    5. release_artifact.py prepare   attach the signed executables and BUILDINFO

Without flags the script builds both in one pass, as before.
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys
import release_artifact as artifact

ROOT = Path(__file__).resolve().parents[1]


def build(target, environment, package):
    command = ['cargo', 'build', '--release', '--locked', '--target-dir', str(target), '--bin', package]
    subprocess.run(command, cwd=ROOT, env=environment, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--target-dir', type=Path, default=ROOT/'target')
    parser.add_argument('--output', type=Path, help='Prepare CI provenance and attachments after building')
    phase = parser.add_mutually_exclusive_group()
    phase.add_argument('--tool-only', action='store_true', help='Build Tskmgr only and print its digest')
    phase.add_argument('--signed-tool', type=Path, help='Bind this (already signed) Tskmgr and build Winmgr')
    args = parser.parse_args()
    target = args.target_dir.resolve()
    environment = os.environ.copy()
    environment.pop('MINICON_TSKMGR_SHA256', None)

    if args.signed_tool is not None:
        tool = args.signed_tool.resolve(strict=True)
        bound = artifact.digest(tool)
    else:
        build(target, environment, 'minicon-tskmgr')
        tool = target/'release/minicon-tskmgr.exe'
        bound = artifact.digest(tool)
        if args.tool_only:
            print(bound, flush=True)
            return

    environment['MINICON_TSKMGR_SHA256'] = bound
    build(target, environment, 'minicon-winmgr')
    if args.signed_tool is None and artifact.digest(tool) != bound:
        raise RuntimeError('Companion changed while building the shell')
    print('Built Winmgr bound to Tskmgr SHA-256 '+bound, flush=True)
    if args.output:
        subprocess.run([sys.executable, str(ROOT/'scripts/release_artifact.py'), 'prepare',
                        '--exe', str(target/'release/minicon-winmgr.exe'), '--tool', str(tool),
                        '--output', str(args.output.resolve())],
                       cwd=ROOT, env=environment, check=True)


if __name__ == '__main__':
    main()
