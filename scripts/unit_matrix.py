"""Run complete ordinary Rust units in isolated language/concurrency environments."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    parser.add_argument('--quick', action='store_true',
                        help='One isolated English/default-concurrency run; full matrix remains opt-in by omitting this flag')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    for language in (('en',) if args.quick else ('en', 'zh')):
        for serial in ((False,) if args.quick else (False, True)):
            with tempfile.TemporaryDirectory(prefix='minicon-unit-') as directory:
                folder = Path(directory)
                environment = {key: value for key, value in os.environ.items()
                               if not key.startswith('MINICON_')}
                environment.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
                environment.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
                environment.update(
                    USERPROFILE=str(folder), LOCALAPPDATA=str(folder / 'appdata'),
                    MINICON_CONFIG=str(folder / 'empty.conf'),
                    MINICON_SETTINGS=str(folder / 'winmgr.cnf'),
                    MINICON_LANGUAGE=language,
                    MINICON_LOG_DIR=str(folder / 'logs'))
                command = ['cargo', 'test', '--all', '--locked']
                if args.release:
                    command.append('--release')
                command.extend(['--', '--quiet'])
                if serial:
                    command.append('--test-threads=1')
                print(f'Ordinary units: {language}, {"serial" if serial else "default concurrency"}', flush=True)
                subprocess.run(command, cwd=root, env=environment, check=True)


if __name__ == '__main__':
    main()
