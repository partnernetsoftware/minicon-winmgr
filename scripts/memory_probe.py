"""Read-only, process-bound Windows resource sampling; raw output stays local."""
from __future__ import annotations
import argparse
import csv
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import math
import re
from pathlib import Path
import statistics
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
METRICS = ('private_bytes', 'working_set_bytes', 'private_working_set_bytes',
           'handles', 'gdi', 'user', 'threads', 'cpu_seconds')
MAX_SAMPLES = 25000
SCENARIOS = ('idle', 'settings', 'run', 'resources', 'update', 'soak')


def public_markdown(summary):
    """Allowlist values, not just keys; never interpolate arbitrary report strings."""
    def number(value):
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise ValueError('Invalid numeric summary field')
        return f'{value:.3f}'
    digest = summary.get('binary_sha256', '')
    if not isinstance(digest, str) or not re.fullmatch('[a-f0-9]{64}', digest):
        raise ValueError('Invalid binary digest')
    scenario = summary.get('scenario')
    if scenario not in SCENARIOS:
        raise ValueError('Unknown scenario')
    reason = summary.get('stop_reason')
    if reason not in ('duration_completed', 'target_exited', 'interrupted', 'sampling_failed'):
        raise ValueError('Unknown observation outcome')
    lines = ['# Resource observation (review before publication)', '',
             f'Binary SHA-256: `{digest}`', '', f'Scenario: `{scenario}`; outcome: `{reason}`.', '',
             f"Observed seconds: {number(summary['elapsed_seconds'])}; samples: {number(summary['sample_count'])}; "
             f"OS build: {number(summary['os_build'])}.", '',
             '| Metric (bytes/count/CPU seconds) | Median | P95 | Peak | Window delta | Units/hour |',
             '|---|---:|---:|---:|---:|---:|']
    for key in METRICS:
        item = summary.get('metrics', {}).get(key)
        if item is None:
            lines.append(f'| {key} | unavailable | | | | |')
        else:
            values = [number(item[field]) if item.get(field) is not None else 'unavailable'
                      for field in ('median', 'p95', 'peak', 'window_delta', 'units_per_hour')]
            lines.append('| ' + key + ' | ' + ' | '.join(values) + ' |')
    lines += ['', 'A single observation does not prove absence of leaks. Short samples do not satisfy long-run acceptance.', '']
    return '\n'.join(lines)


def summarize(rows):
    """Omit absent metrics; use actual elapsed time, never requested duration."""
    result = {}
    for key in METRICS:
        pairs = [(row['elapsed_seconds'], row[key]) for row in rows if row.get(key) is not None]
        if not pairs:
            result[key] = None
            continue
        values = sorted(value for _, value in pairs)
        window = max(1, min(len(pairs) // 5, 60))
        start = statistics.median(value for _, value in pairs[:window])
        end = statistics.median(value for _, value in pairs[-window:])
        # Least-squares trend is descriptive, not a leak detector.
        mean_t = statistics.mean(t for t, _ in pairs)
        mean_v = statistics.mean(value for _, value in pairs)
        denominator = sum((t - mean_t) ** 2 for t, _ in pairs)
        slope = (sum((t - mean_t) * (value - mean_v) for t, value in pairs) / denominator
                 if denominator else None)
        result[key] = {'median': statistics.median(values),
                       'p95': values[max(0, math.ceil(.95 * len(values)) - 1)],
                       'peak': max(values), 'first_window_median': start,
                       'last_window_median': end, 'window_delta': end - start,
                       'units_per_hour': slope * 3600 if slope is not None else None}
    return result


class Process:
    def __init__(self, pid):
        if sys.platform != 'win32':
            raise RuntimeError('Windows is required for live sampling')
        self.pid = pid
        self.k = c.WinDLL('kernel32', use_last_error=True)
        self.u = c.WinDLL('user32', use_last_error=True)
        self.p = c.WinDLL('psapi', use_last_error=True)
        signatures = [
            (self.k.OpenProcess, [w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            (self.k.CloseHandle, [w.HANDLE], w.BOOL),
            (self.k.WaitForSingleObject, [w.HANDLE, w.DWORD], w.DWORD),
            (self.k.GetProcessTimes, [w.HANDLE] + [c.POINTER(w.FILETIME)] * 4, w.BOOL),
            (self.k.GetProcessHandleCount, [w.HANDLE, c.POINTER(w.DWORD)], w.BOOL),
            (self.k.QueryFullProcessImageNameW, [w.HANDLE, w.DWORD, w.LPWSTR, c.POINTER(w.DWORD)], w.BOOL),
            (self.p.GetProcessMemoryInfo, [w.HANDLE, c.c_void_p, w.DWORD], w.BOOL),
            (self.u.GetGuiResources, [w.HANDLE, w.DWORD], w.DWORD),
            (self.k.CreateToolhelp32Snapshot, [w.DWORD, w.DWORD], w.HANDLE),
        ]
        for fn, args, result in signatures:
            fn.argtypes, fn.restype = args, result
        # No VM_READ/terminate/write rights. A retained handle cannot follow PID reuse.
        self.handle = self.k.OpenProcess(0x0400 | 0x100000, False, pid)
        if not self.handle:
            raise RuntimeError(f'Cannot open target (Win32 {c.get_last_error()})')
        try:
            self.created, _ = self.times()
            name = c.create_unicode_buffer(32768)
            size = w.DWORD(len(name))
            if not self.k.QueryFullProcessImageNameW(self.handle, 0, name, c.byref(size)):
                raise RuntimeError('Cannot identify target image')
            # Use the path only to hash the executable; never put it in a report.
            with Path(name.value).open('rb') as image:
                self.image_hash = hashlib.file_digest(image, 'sha256').hexdigest()
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.handle:
            self.k.CloseHandle(self.handle)
            self.handle = None

    def times(self):
        values = [w.FILETIME() for _ in range(4)]
        if not self.k.GetProcessTimes(self.handle, *(c.byref(value) for value in values)):
            raise RuntimeError('Cannot read target times')
        numbers = [(value.dwHighDateTime << 32) | value.dwLowDateTime for value in values]
        return numbers[0], (numbers[2] + numbers[3]) / 10_000_000

    def threads(self):
        class Entry(c.Structure):
            _fields_ = [('size', w.DWORD), ('usage', w.DWORD), ('pid', w.DWORD),
                        ('heap', c.c_size_t), ('module', w.DWORD), ('threads', w.DWORD),
                        ('parent', w.DWORD), ('priority', w.LONG), ('flags', w.DWORD),
                        ('image', w.WCHAR * 260)]
        snapshot = self.k.CreateToolhelp32Snapshot(2, 0)
        if snapshot == c.c_void_p(-1).value:
            return None
        try:
            for fn in [self.k.Process32FirstW, self.k.Process32NextW]:
                fn.argtypes, fn.restype = [w.HANDLE, c.POINTER(Entry)], w.BOOL
            entry = Entry(); entry.size = c.sizeof(entry)
            ok = self.k.Process32FirstW(snapshot, c.byref(entry))
            while ok:
                if entry.pid == self.pid:
                    return entry.threads
                ok = self.k.Process32NextW(snapshot, c.byref(entry))
            return None
        finally:
            self.k.CloseHandle(snapshot)

    def sample(self):
        if self.k.WaitForSingleObject(self.handle, 0) != 0x102:
            raise ProcessLookupError('Target exited')
        created, cpu = self.times()
        if created != self.created:
            raise ProcessLookupError('Target identity changed')
        class Memory(c.Structure):
            _fields_ = [('cb', w.DWORD), ('faults', w.DWORD)] + [
                (name, c.c_size_t) for name in (
                    'peak_ws', 'ws', 'peak_paged', 'paged', 'peak_nonpaged',
                    'nonpaged', 'pagefile', 'peak_pagefile', 'private')]
        class Memory2(c.Structure):
            _fields_ = Memory._fields_ + [('private_ws', c.c_size_t), ('shared_commit', c.c_ulonglong)]
        # EX2 needs a patched OS. Conservatively use it only on build 26100+;
        # older supported systems get EX with an explicit unavailable private WS.
        counters = Memory2() if sys.getwindowsversion().build >= 26100 else Memory()
        counters.cb = c.sizeof(counters)
        if not self.p.GetProcessMemoryInfo(self.handle, c.byref(counters), counters.cb):
            counters = Memory(); counters.cb = c.sizeof(counters)
            if not self.p.GetProcessMemoryInfo(self.handle, c.byref(counters), counters.cb):
                raise RuntimeError('Memory sample failed')
        handles = w.DWORD()
        handle_count = handles.value if self.k.GetProcessHandleCount(self.handle, c.byref(handles)) else None
        def gui(kind):
            c.set_last_error(0)
            value = self.u.GetGuiResources(self.handle, kind)
            return None if value == 0 and c.get_last_error() else value
        result = dict(private_bytes=counters.private, working_set_bytes=counters.ws,
                      private_working_set_bytes=getattr(counters, 'private_ws', None),
                      handles=handle_count, gdi=gui(0), user=gui(1),
                      threads=self.threads(), cpu_seconds=cpu)
        # Discard a partial sample if the process exited during API calls.
        if self.k.WaitForSingleObject(self.handle, 0) != 0x102:
            raise ProcessLookupError('Target exited during sample')
        return result


def collect(pid, duration, interval, output, scenario):
    if not isinstance(pid, int) or pid <= 0:
        raise ValueError('A positive process ID is required')
    if scenario not in SCENARIOS:
        raise ValueError('Unknown scenario')
    if not math.isfinite(duration) or not 0 < duration <= 86400:
        raise ValueError('Observation duration must be finite and at most 24 hours')
    if not math.isfinite(interval) or not .1 <= interval <= 60:
        raise ValueError('Sampling interval must be between 0.1 and 60 seconds')
    if math.ceil(duration / interval) + 1 > MAX_SAMPLES:
        raise ValueError('Too many samples; increase the interval to bound observer memory')
    output = Path(output).resolve()
    if not output.is_relative_to(ROOT / 'target'):
        raise ValueError('Raw reports must stay under the ignored target directory')
    output.mkdir(parents=True, exist_ok=True)
    # Prevent silent overwrite of an existing experiment.
    raw = (output / 'samples.csv').open('x', newline='', encoding='utf-8')
    rows = []
    process = None
    started = time.monotonic()
    observer_cpu = time.process_time()
    reason = 'duration_completed'
    try:
        process = Process(pid)
        writer = csv.DictWriter(raw, fieldnames=['elapsed_seconds', *METRICS])
        writer.writeheader(); raw.flush()
        while True:
            elapsed = time.monotonic() - started
            row = {'elapsed_seconds': round(elapsed, 4), **process.sample()}
            rows.append(row); writer.writerow(row); raw.flush()
            remaining = duration - (time.monotonic() - started)
            if remaining <= 0:
                break
            # Sampling period includes observer execution time; no UI activity.
            next_tick = (math.floor((time.monotonic() - started) / interval) + 1) * interval
            deadline = started + min(duration, next_tick)
            time.sleep(max(0, deadline - time.monotonic()))
    except ProcessLookupError:
        reason = 'target_exited'
    except KeyboardInterrupt:
        reason = 'interrupted'
    except Exception:
        reason = 'sampling_failed'
        raise
    finally:
        raw.close()
        elapsed = time.monotonic() - started
        summary = {'schema': 1, 'scenario': scenario, 'binary_sha256': process.image_hash if process else None,
                   'os_build': sys.getwindowsversion().build, 'requested_seconds': duration,
                   'elapsed_seconds': round(elapsed, 3), 'interval_seconds': interval,
                   'sample_count': len(rows), 'stop_reason': reason,
                   'observer_cpu_seconds': round(time.process_time() - observer_cpu, 4),
                   'metrics': summarize(rows), 'long_run_passed': False}
        if process:
            process.close()
        (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n', encoding='utf-8')
        print(json.dumps({'sample_count': len(rows), 'stop_reason': reason,
                          'elapsed_seconds': round(elapsed, 3)}), flush=True)
    return summary


def plot(output):
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    output = Path(output)
    with (output / 'samples.csv').open(encoding='utf-8') as source:
        rows = list(csv.DictReader(source))
    fig, axes = plt.subplots(2, 1, figsize=(9, 6), sharex=True)
    for key in METRICS[:3]:
        pairs = [(float(row['elapsed_seconds']) / 60, float(row[key]) / 1048576)
                 for row in rows if row[key]]
        if pairs:
            axes[0].plot(*zip(*pairs), label=key)
    for key in ('handles', 'gdi', 'user', 'threads'):
        pairs = [(float(row['elapsed_seconds']) / 60, float(row[key])) for row in rows if row[key]]
        if pairs:
            axes[1].plot(*zip(*pairs), label=key)
    axes[0].set_ylabel('MiB'); axes[1].set_ylabel('Object count')
    axes[1].set_xlabel('Elapsed minutes')
    for ax in axes:
        ax.legend(); ax.grid(alpha=.2)
    fig.tight_layout(); fig.savefig(output / 'trend.png', dpi=150); plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pid', type=int)
    parser.add_argument('--duration', type=float, help='Explicit observation seconds; max 24 hours')
    parser.add_argument('--interval', type=float, default=5)
    parser.add_argument('--scenario', choices=SCENARIOS, default='idle')
    parser.add_argument('--output', type=Path, default=ROOT / 'target/test-reports/memory')
    parser.add_argument('--plot-only', action='store_true')
    parser.add_argument('--export-summary', action='store_true', help='Generate an allowlisted local Markdown draft for human review')
    args = parser.parse_args()
    if args.export_summary:
        if args.plot_only: parser.error('Choose either --plot-only or --export-summary')
        output = args.output.resolve()
        if not output.is_relative_to(ROOT / 'target'): parser.error('Exports stay in ignored target until reviewed')
        summary = json.loads((output/'summary.json').read_text(encoding='utf-8'))
        (output/'review-summary.md').write_text(public_markdown(summary), encoding='utf-8')
        print('Allowlisted summary draft generated locally; review before publication.')
        return
    if args.plot_only:
        plot(args.output); return
    if args.pid is None or args.pid <= 0 or args.duration is None or not 0 < args.duration <= 86400:
        parser.error('--pid and an explicit --duration between 0 and 86400 are required')
    if not math.isfinite(args.interval) or not 0.1 <= args.interval <= 60:
        parser.error('--interval must be between 0.1 and 60 seconds')
    collect(args.pid, args.duration, args.interval, args.output, args.scenario)


if __name__ == '__main__':
    main()
