"""Controlled native-resource experiments; no normal shell replacement."""
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
        cwd=ROOT, text=True, encoding='utf-8', stdout=subprocess.PIPE, check=True,
    )
    executables = [item['executable'] for line in build.stdout.splitlines()
                   if (item := json.loads(line)).get('reason') == 'compiler-artifact'
                   and item.get('executable') and item.get('profile', {}).get('test')]
    if len(executables) != 1:
        raise RuntimeError('Could not uniquely identify the test executable')
    executable = executables[0]
    output = ROOT / 'target/test-reports/resources' / str(time.time_ns())
    output.mkdir(parents=True)
    report = {'profile': 'release' if release else 'debug', 'passed': False, 'queue_samples': []}
    with Path(executable).open('rb') as binary:
        report['test_binary_sha256'] = hashlib.file_digest(binary, 'sha256').hexdigest()
    try:
        result = subprocess.run([executable, '--exact',
            'hover_preview::tests::compositor_preview_survives_stalled_source_and_releases_registrations',
            '--ignored', '--nocapture'], cwd=ROOT, capture_output=True, text=True, encoding='utf-8', timeout=30)
        (output/'compositor-local.log').write_text(result.stdout+result.stderr, encoding='utf-8')
        if result.returncode or '1 passed' not in result.stdout:
            raise RuntimeError('Compositor stalled-source experiment failed; see local log')
        report['stalled_source_preview_passed'] = True
        result = subprocess.run([executable, '--exact', 'window_icon::tests::owned_path_icon_outlives_cache_eviction',
            '--ignored', '--nocapture'], cwd=ROOT, capture_output=True, text=True, encoding='utf-8', timeout=30)
        (output/'icon-ownership-local.log').write_text(result.stdout+result.stderr, encoding='utf-8')
        if result.returncode or '1 passed' not in result.stdout:
            raise RuntimeError('Owned icon lifetime experiment failed; see local log')
        report['owned_icon_lifetime_passed'] = True
        report['icon_worker_samples'] = []
        for repetition in range(3):
            result = subprocess.run([executable, '--exact',
                'window_icon_worker::tests::stalled_windows_coalesce_and_owned_results_outlive_the_source',
                '--ignored', '--nocapture'], cwd=ROOT, capture_output=True, text=True, encoding='utf-8', timeout=30)
            (output/f'icon-worker-{repetition+1}-local.log').write_text(result.stdout+result.stderr, encoding='utf-8')
            if result.returncode or '1 passed' not in result.stdout:
                raise RuntimeError('Asynchronous icon worker experiment failed; see local log')
            match = re.search(r'ICON_WORKER_SAMPLE (\{[^\n]+\})', result.stdout)
            if not match:
                raise RuntimeError('Missing asynchronous icon worker measurement')
            sample = json.loads(match[1])
            sample['repetition'] = repetition + 1
            report['icon_worker_samples'].append(sample)
        report['icon_worker_passed'] = True
        for repetition in range(3):
            # Fresh process per sample: allocator retention cannot bias the next capacity.
            for capacity in [128000, 512]:
                environment = dict(os.environ, WINMGR_TEST_LOG_QUEUE=str(capacity))
                result = subprocess.run([executable, '--exact', 'logging::tests::logging_queue_commit_footprint',
                    '--ignored', '--nocapture'], cwd=ROOT, env=environment, capture_output=True,
                    text=True, encoding='utf-8', timeout=30, check=True)
                match = re.search(r'QUEUE_SAMPLE (\{[^\n]+\})', result.stdout)
                if not match:
                    raise RuntimeError('Missing isolated queue measurement')
                sample = json.loads(match[1])
                if sample['capacity'] != capacity:
                    raise RuntimeError('Wrong test queue capacity')
                sample['repetition'] = repetition + 1
                sample['private_delta'] = sample['private_after'] - sample['private_before']
                report['queue_samples'].append(sample)
        medians = {str(capacity): statistics.median(sample['private_delta']
                   for sample in report['queue_samples'] if sample['capacity'] == capacity)
                   for capacity in [128000, 512]}
        report['median_private_delta_bytes'] = medians
        report['queue_commit_reduction_bytes'] = medians['128000'] - medians['512']
        if report['queue_commit_reduction_bytes'] <= 0:
            raise RuntimeError('Queue change needs investigation: no measured commit reduction')
        report['passed'] = True
    finally:
        (output/'summary.json').write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
        print(json.dumps({'passed': report['passed'],
                          'queue_commit_reduction_bytes': report.get('queue_commit_reduction_bytes')}), flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--release', action='store_true')
    main(parser.parse_args().release)
