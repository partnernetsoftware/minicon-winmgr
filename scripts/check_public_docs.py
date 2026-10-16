"""Check the Git index for raw reports and workstation paths before publishing."""
from pathlib import Path
import os
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def violations(name, data, root):
    problems = []
    if re.fullmatch(r'docs/acceptance[^/]*\.json', name, re.I) or re.fullmatch(
        r'docs/releases/[^/]*-published\.json', name, re.I
    ):
        problems.append('raw workstation report')
    try:
        text = data.decode('utf-8')
    except UnicodeError:
        return problems  # Images require visual review.
    normalized = re.sub(r'\\+', '/', text).lower()
    locations = [str(root), str(Path.home())]
    if os.environ.get('USERPROFILE'):
        locations.append(os.environ['USERPROFILE'])
    needles = [re.sub(r'\\+', '/', value).lower() for value in locations]
    public_document = name.startswith('docs/') or name.lower().endswith('.md')
    for number, line in enumerate(normalized.splitlines(), 1):
        # Source tests may use fictional Windows paths. The actual workstation
        # locations are forbidden everywhere; document paths get a broader check.
        if any(value and value in line for value in needles) or (public_document and re.search(
            r'\b[a-z]:/(?:users|dev|work|repos|projects)/[^\s`"<>]+', line
        )):
            problems.append(f'workstation path at line {number}')
    return problems


def audit(root):
    # Read staged blobs, not working copies: an unstaged cleanup must not hide a leak.
    entries = subprocess.check_output(
        ['git', 'ls-files', '--stage', '-z'], cwd=root
    ).split(b'\0')
    blobs = []
    for entry in entries:
        if not entry:
            continue
        metadata, path = entry.split(b'\t', 1)
        _, oid, stage = metadata.split()
        if stage != b'0':
            raise RuntimeError('Resolve index conflicts before publication')
        blobs.append((oid, path.decode('utf-8')))
    result = subprocess.run(
        ['git', 'cat-file', '--batch'], cwd=root,
        input=b''.join(oid + b'\n' for oid, _ in blobs),
        stdout=subprocess.PIPE, check=True,
    ).stdout
    cursor = 0
    errors = []
    for _, name in blobs:
        end = result.index(b'\n', cursor)
        _, kind, length = result[cursor:end].split()
        if kind != b'blob':
            raise RuntimeError('Unexpected Git object')
        size = int(length)
        data = result[end + 1:end + 1 + size]
        cursor = end + 1 + size + 1
        errors.extend(f'{name}: {problem}' for problem in violations(name, data, root))
    return errors


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory(prefix='winmgr-docs-check-') as folder:
        root = Path(folder)
        subprocess.run(['git', 'init', '-q'], cwd=root, check=True)
        report = root / 'docs/acceptance_example.json'
        report.parent.mkdir()
        report.write_text('{}', encoding='utf-8')
        subprocess.run(['git', 'add', '.'], cwd=root, check=True)
        report.unlink()
        assert any('raw workstation report' in e for e in audit(root)), 'Index leak was hidden by working tree deletion'
        subprocess.run(['git', 'add', '-u'], cwd=root, check=True)
        assert not audit(root)
        note = root / 'docs/summary.md'
        note.write_text(str(root), encoding='utf-8')
        subprocess.run(['git', 'add', '.'], cwd=root, check=True)
        note.write_text('Reviewed summary', encoding='utf-8')
        assert any('workstation path' in e for e in audit(root)), 'Index path leak was hidden by working tree edit'
        subprocess.run(['git', 'add', '.'], cwd=root, check=True)
        assert not audit(root)
        assert violations('docs/report.md', str(root).replace('\\', '\\\\').encode(), root)
        assert not violations('docs/report.md', b'%USERPROFILE%/config.json', root)
        fixture = ('C:' + '/Users/example/config.json').encode()
        assert violations('docs/report.md', fixture, root)
        assert not violations('src/tests.rs', fixture, root)
    print('Publication guard self-test passed (staged reports, paths, escaped paths, safe placeholders).')


if __name__ == '__main__':
    if sys.argv[1:] == ['--self-test']:
        self_test()
    elif sys.argv[1:]:
        raise SystemExit('Usage: python scripts/check_public_docs.py [--self-test]')
    else:
        findings = audit(ROOT)
        for finding in findings:
            print(finding)  # Never print sensitive matching text.
        print(f'Publication guard: {len(findings)} issue(s). Images still require manual review.')
        raise SystemExit(bool(findings))
