"""Create and remove one standard account on a disposable hosted Windows runner.

Refuses local use. The random fixture password stays in subprocess environment;
neither product executables nor a user workstation are modified.
"""
import os
from pathlib import Path
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]


def powershell(script, environment):
    # Match the hosted workflow's PowerShell 7 module environment. Launching
    # Windows PowerShell 5 with its inherited PSModulePath loads incompatible
    # Security modules before the fixture can even create its owned account.
    return subprocess.run(['pwsh.exe', '-NoProfile', '-NonInteractive', '-Command', script],
                          env=environment, capture_output=True, text=True, timeout=30)


def main():
    if (os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_OS') != 'Windows'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise RuntimeError('This real-account fixture is restricted to disposable hosted Windows CI')
    name = 'mc_task_' + uuid.uuid4().hex[:10]
    folder = (ROOT/'target/test-reports/cross-account'/uuid.uuid4().hex).resolve()
    assert folder.is_relative_to((ROOT/'target').resolve())
    folder.mkdir(parents=True)
    environment = os.environ.copy()
    environment.update(MINICON_CI_USER_NAME=name,
                       MINICON_CI_USER_PASSWORD='Aa1!'+uuid.uuid4().hex,
                       MINICON_CI_ORIGIN_REPORT=str(folder/'origin.json'))
    sid = None
    try:
        result = powershell('''$ErrorActionPreference='Stop'
$secret=ConvertTo-SecureString $env:MINICON_CI_USER_PASSWORD -AsPlainText -Force
$account=New-LocalUser -Name $env:MINICON_CI_USER_NAME -Password $secret -PasswordNeverExpires -UserMayNotChangePassword
Write-Output ('SID:'+ $account.SID.Value)
Add-LocalGroupMember -SID ([System.Security.Principal.SecurityIdentifier]'S-1-5-32-545') -Member $account
''', environment)
        sid = next((line[4:].strip() for line in result.stdout.splitlines() if line.startswith('SID:')), None)
        if result.returncode or not sid:
            raise RuntimeError('CI standard-account setup failed: '+result.stderr)
        environment['MINICON_CI_USER_SID'] = sid
        # This gate also runs before the first compilation, so new test images
        # inherit the owned fixture's read/execute grant.
        (ROOT/'target/release/deps').mkdir(parents=True, exist_ok=True)
        for target, rights in [(folder, '(OI)(CI)M'), (ROOT/'target/release/deps', '(OI)(CI)RX')]:
            subprocess.run(['icacls.exe', str(target), '/grant', f'*{sid}:{rights}'],
                           check=True, stdout=subprocess.DEVNULL, timeout=30)
        subprocess.run(['cargo', 'test', '--release', '--locked', '--lib',
                        'task_access::cross_account_tests::parent_retains_original_user_with_different_credentials',
                        '--', '--ignored', '--exact', '--nocapture', '--test-threads=1'],
                       cwd=ROOT, env=environment, check=True, timeout=600)
    finally:
        if sid:
            environment['MINICON_CI_USER_SID'] = sid
            result = powershell('''$ErrorActionPreference='Stop'
Remove-LocalUser -SID ([System.Security.Principal.SecurityIdentifier]$env:MINICON_CI_USER_SID)
''', environment)
            if result.returncode:
                raise RuntimeError('Failed to remove the owned CI fixture account: '+result.stderr)
        environment.pop('MINICON_CI_USER_PASSWORD', None)
    print('Cross-account fixture completed and the owned CI account was removed.', flush=True)


if __name__ == '__main__':
    main()
