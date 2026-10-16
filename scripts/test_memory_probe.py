"""Resource sampler statistics, privacy, and Windows process-lifecycle checks."""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

import memory_probe as probe


class StatisticsTests(unittest.TestCase):
    def test_public_export_drops_unknown_fields_and_rejects_path_values(self):
        summary = {'scenario': 'idle', 'stop_reason': 'duration_completed',
                   'binary_sha256': 'a'*64, 'elapsed_seconds': 10, 'sample_count': 3,
                   'os_build': 1, 'metrics': {}, 'private_command': str(probe.ROOT)}
        text = probe.public_markdown(summary)
        self.assertNotIn(str(probe.ROOT), text)
        self.assertNotIn('private_command', text)
        summary['scenario'] = str(probe.ROOT)
        with self.assertRaises(ValueError): probe.public_markdown(summary)
        summary['scenario'] = 'idle'
        summary['metrics'] = {'private_bytes': {'median': str(probe.ROOT)}}
        with self.assertRaises(ValueError): probe.public_markdown(summary)

    def test_trend_uses_observed_time_and_missing_is_not_zero(self):
        result = probe.summarize([
            {'elapsed_seconds': 0, 'private_bytes': 100},
            {'elapsed_seconds': 10, 'private_bytes': 120},
            {'elapsed_seconds': 20, 'private_bytes': 140},
        ])
        self.assertEqual(result['private_bytes']['units_per_hour'], 7200)
        self.assertEqual(result['private_bytes']['p95'], 140)
        self.assertEqual(result['private_bytes']['window_delta'], 40)
        self.assertIsNone(result['private_working_set_bytes'])

    def test_one_sample_cannot_imply_growth(self):
        self.assertIsNone(probe.summarize([{'elapsed_seconds': 1, 'handles': 20}])['handles']['units_per_hour'])
        self.assertTrue(all(value is None for value in probe.summarize([]).values()))

    def test_raw_output_outside_ignored_directory_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'ignored target'):
            probe.collect(1, 1, 1, probe.ROOT / 'docs', 'idle')

    def test_invalid_budgets_cannot_start_unbounded_sampling(self):
        for duration, interval in [(float('nan'), 1), (float('inf'), 1),
                                   (1, float('nan')), (1, 0), (1, -1), (86401, 1), (86400, .1)]:
            with self.assertRaises(ValueError):
                probe.collect(1, duration, interval, probe.ROOT / 'target', 'idle')


@unittest.skipUnless(sys.platform == 'win32', 'Live Win32 API checks')
class WindowsTests(unittest.TestCase):
    def test_actual_process_metrics_and_identity(self):
        target = probe.Process(os.getpid())
        try:
            row = target.sample()
            self.assertGreater(row['private_bytes'], 0)
            self.assertGreater(row['working_set_bytes'], 0)
            self.assertGreater(row['handles'], 0)
            self.assertGreater(row['threads'], 0)
            self.assertEqual(target.created, target.times()[0])
            self.assertEqual(len(target.image_hash), 64)
            if row['private_working_set_bytes'] is not None:
                self.assertLessEqual(row['private_working_set_bytes'], row['working_set_bytes'])
            serialized = json.dumps(row)
            self.assertNotIn(str(Path.home()), serialized)
            self.assertNotIn(str(probe.ROOT), serialized)
        finally:
            target.close()

    def test_exit_stops_sampling_without_reopening_pid(self):
        child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        target = None
        try:
            target = probe.Process(child.pid)
            target.sample()
            child.terminate(); child.wait(timeout=5)
            with self.assertRaises(ProcessLookupError):
                target.sample()
        finally:
            if child.poll() is None:
                child.terminate(); child.wait(timeout=5)
            if target:
                target.close()


if __name__ == '__main__':
    unittest.main()
