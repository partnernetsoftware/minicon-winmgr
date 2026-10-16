"""Compare the previous update buffers with bounded staging in fresh processes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]


def main(release=False):
    build = subprocess.run(
        ['cargo', 'test', '--bin', 'minicon-winmgr', '--no-run', '--message-format=json',
         *(['--release', '--target-dir', 'target/release-validation'] if release else [])],
        cwd=ROOT, text=True, encoding='utf-8', stdout=subprocess.PIPE, check=True)
    executables = [item['executable'] for line in build.stdout.splitlines()
                   if (item := json.loads(line)).get('reason') == 'compiler-artifact'
                   and item.get('executable') and item.get('profile', {}).get('test')]
    if len(executables) != 1:
        raise RuntimeError('Could not uniquely identify the test executable')
    executable = executables[0]
    output = ROOT / 'target/test-reports/update-memory' / str(time.time_ns())
    output.mkdir(parents=True)
    report = {'profile': 'release' if release else 'debug', 'passed': False, 'samples': [],
              'scope': '32 MiB synthetic payload/old-file staging; excludes network, live shell and handoff'}
    with Path(executable).open('rb') as binary:
        report['test_binary_sha256'] = hashlib.file_digest(binary, 'sha256').hexdigest()
    try:
        for repetition in range(3):
            for mode in ['legacy', 'stream']:
                result = subprocess.run([executable, '--exact',
                    'update_install::tests::streaming_preparation_commit_footprint', '--ignored', '--nocapture'],
                    cwd=ROOT, env=dict(os.environ, WINMGR_TEST_UPDATE_BUFFER=mode), capture_output=True,
                    text=True, encoding='utf-8', timeout=60, creationflags=subprocess.CREATE_NO_WINDOW)
                (output/f'{mode}-{repetition+1}-local.log').write_text(result.stdout+result.stderr, encoding='utf-8')
                if result.returncode or '1 passed' not in result.stdout:
                    raise RuntimeError('Update-memory experiment failed; inspect the local log')
                match = re.search(r'UPDATE_MEMORY_SAMPLE (\{[^\n]+\})', result.stdout)
                if not match:
                    raise RuntimeError('Missing update-memory measurement')
                sample = json.loads(match[1])
                if sample['mode'] != mode or sample['file_bytes'] != 32 * 1024 * 1024:
                    raise RuntimeError('Wrong update-memory workload')
                sample['repetition'] = repetition + 1
                report['samples'].append(sample)
        medians = {mode: statistics.median(s['private_peak_delta_bytes'] for s in report['samples']
                    if s['mode'] == mode) for mode in ['legacy', 'stream']}
        report['median_private_peak_delta_bytes'] = medians
        report['peak_reduction_bytes'] = medians['legacy'] - medians['stream']
        if report['peak_reduction_bytes'] < 48 * 1024 * 1024:
            raise RuntimeError('Update buffering needs investigation: insufficient measured reduction')
        report['passed'] = True
    finally:
        (output/'summary.json').write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
        print(json.dumps({'passed': report['passed'],
                          'peak_reduction_bytes': report.get('peak_reduction_bytes')}), flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    main(parser.parse_args().release)
