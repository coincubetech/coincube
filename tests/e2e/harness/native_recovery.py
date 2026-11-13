"""Signed native Recovery Kit lifecycle, separate from GUI/mobile acceptance.

Every action is a new owned process. No external receipt or arbitrary executable
can satisfy this gate. Preflight runs before credential, cloud or fixture access.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import uuid

from .state import Blocked, Failed, Journal, atomic_write, digest, private_read
from .native_recovery_verify import CHAINS, dependencies, verify

PREREQUISITE = '9e9a1f25995b4be9a6ab188240a2bf92d5f3209b'
IDENTITY = 'Developer ID Application: COINCUBE TECHNOLOGY LLC (8UVR249AD5)'
ACTIONS = ('create_export', 'wipe_original', 'restore', 'reopen_sign')


def require(ok, code):
    if not ok:
        raise Failed('NATIVE_RECOVERY_' + code, 'Native recovery prerequisite/validation failed: ' + code)


def options(value):
    require(isinstance(value, dict) and set(value) == {'version', 'checkout', 'revision'}, 'CONFIG')
    require(type(value['version']) is int and value['version'] == 1
            and isinstance(value['checkout'], str) and Path(value['checkout']).is_absolute()
            and isinstance(value['revision'], str)
            and (value['revision'] == 'current' or re.fullmatch('[0-9a-f]{40}', value['revision'])), 'CONFIG')
    return dict(value)


def source(value):
    cfg = options(value); checkout = Path(cfg['checkout'])
    try:
        remote = subprocess.check_output(['git', 'remote', 'get-url', 'origin'], cwd=checkout).decode().strip()
        head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=checkout).decode().strip()
        tree = subprocess.check_output(['git', 'rev-parse', 'HEAD^{tree}'], cwd=checkout).decode().strip()
        clean = not subprocess.check_output(['git', 'status', '--porcelain'], cwd=checkout).strip()
        result = subprocess.run(['git', 'merge-base', '--is-ancestor', PREREQUISITE, head], cwd=checkout,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        raise Blocked('NATIVE_RECOVERY_SOURCE', 'The native source binding could not be verified.') from None
    require(remote in ('git@github.com:coincubetech/coincube.git', 'https://github.com/coincubetech/coincube.git')
            and (cfg['revision'] == 'current' or cfg['revision'] == head), 'SOURCE_BINDING')
    if result.returncode:
        raise Blocked('NATIVE_RECOVERY_PR1174', 'Reviewed Recovery Kit cold-loader prerequisite is not integrated; no credentials or fixtures accessed.')
    if not clean:
        raise Blocked('NATIVE_RECOVERY_DIRTY', 'Native recovery requires reviewed clean source; no live action attempted.')
    return {'head': head, 'tree': tree, 'prerequisite': PREREQUISITE}


def preflight(value):
    binding = source(value)
    dependencies()  # Before credentials, helper launch or mobile fixtures.
    return binding


def build(cfg, binding):
    """Build/sign only this source. No caller-supplied binary or compile assertion."""
    checkout = Path(cfg['checkout']); local = checkout / 'tests/e2e/.local/native-recovery'
    local.mkdir(mode=0o700, parents=True, exist_ok=True)
    require(not local.is_symlink() and not local.stat().st_mode & 0o077, 'BUILD_DIRECTORY')
    import fcntl
    with (local / 'build.lock').open('a+b') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        require(source(cfg) == binding, 'SOURCE_CHANGED')
        bundle = local / 'RecoveryNative.app'; executable = bundle / 'Contents/MacOS/recovery_kit_native'
        receipt_path = local / 'build.json'
        if receipt_path.exists() and executable.exists():
            receipt = json.loads(private_read(receipt_path))
            if receipt.get('source') == binding and receipt.get('sha256') == hashlib.sha256(executable.read_bytes()).hexdigest():
                checked = subprocess.run(['codesign', '--verify', '--strict', str(bundle)], capture_output=True)
                profile = checkout / 'contrib/release/macos'
                identity_check = subprocess.run([str(profile / 'check-profile-authorises-cert.sh'), str(profile / 'embedded.provisionprofile'), '--signed-bundle', str(bundle)], capture_output=True)
                authority = subprocess.run(['codesign', '-d', '--verbose=4', str(bundle)], capture_output=True)
                if (checked.returncode == identity_check.returncode == authority.returncode == 0
                        and ('Authority=' + IDENTITY).encode() in authority.stderr
                        and b'TeamIdentifier=8UVR249AD5' in authority.stderr):
                    return executable, receipt
        env = dict(os.environ, BREEZ_API_KEY='native-recovery-fixture-placeholder',
                   COINCUBE_API_URL='https://api.coincube.io',
                   COINCUBE_NATIVE_RECOVERY_SOURCE=binding['head'], COINCUBE_NATIVE_RECOVERY_PREREQUISITE='1',
                   CARGO_TARGET_DIR=str(checkout / 'target'), CARGO_INCREMENTAL='0', CARGO_BUILD_JOBS='8')
        env['PATH'] = '/opt/homebrew/opt/rustup/bin:' + env.get('PATH', '')
        release = checkout / 'contrib/release/macos'
        try:
            with (local / 'build.log').open('wb') as log:
                def run(argv):
                    subprocess.run([str(a) for a in argv], cwd=checkout, env=env, stdout=log, stderr=log,
                                   check=True, timeout=1800)
                run(['cargo', 'build', '--locked', '-p', 'coincube-gui', '--features', 'integration-tests', '--example', 'recovery_kit_native'])
                run([release / 'check-profile-authorises-cert.sh', release / 'embedded.provisionprofile', '--identity', IDENTITY])
                executable.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                import plistlib
                with (bundle / 'Contents/Info.plist').open('wb') as f:
                    plistlib.dump({'CFBundleIdentifier': 'io.coincube.tenshu', 'CFBundleExecutable': 'recovery_kit_native',
                                  'CFBundleName': 'NativeRecoveryQA', 'CFBundlePackageType': 'APPL', 'CFBundleVersion': '1'}, f)
                shutil.copy2(checkout / 'target/debug/examples/recovery_kit_native', executable)
                shutil.copy2(release / 'embedded.provisionprofile', bundle / 'Contents/embedded.provisionprofile')
                run(['codesign', '-f', '-s', IDENTITY, '--options', 'runtime', '--entitlements', release / 'coincube.entitlements', bundle])
                run(['codesign', '--verify', '--strict', bundle])
                run([release / 'check-profile-authorises-cert.sh', release / 'embedded.provisionprofile', '--signed-bundle', bundle])
        except (OSError, subprocess.SubprocessError):
            raise Blocked('NATIVE_RECOVERY_BUILD', 'Native build/signing unavailable; private diagnostics retained.') from None
        require(source(cfg) == binding, 'SOURCE_CHANGED')
        receipt = {'version': 1, 'source': binding, 'sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
                   'signing_identity': IDENTITY, 'provenance': 'native_product_integration'}
        atomic_write(receipt_path, json.dumps(receipt).encode())
        return executable, receipt


def process(executable, request, directory, *, access_token=None):
    """Wait on fresh process; sample kernel start before providing any action."""
    argv = [str(executable)] + (['--bind-auth'] if access_token is not None else [])
    env = dict(os.environ, COINCUBE_API_URL='https://api.coincube.io')
    # Do not allow runtime environment to assert source/dependency provenance.
    env.pop('COINCUBE_NATIVE_RECOVERY_SOURCE', None); env.pop('COINCUBE_NATIVE_RECOVERY_PREREQUISITE', None)
    try:
        with (directory / 'native-stderr.log').open('ab') as errors:
            child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors,
                                     env=env, start_new_session=True)
            try:
                start = subprocess.check_output(['/bin/ps', '-p', str(child.pid), '-o', 'lstart=']).decode().strip()
                require(bool(start), 'KERNEL_PROCESS')
                payload = dict(request)
                if access_token is not None:
                    payload = {'config': request['config'], 'access_token': access_token}
                output, _ = child.communicate(json.dumps(payload).encode(), timeout=300)
            except BaseException:
                child.kill(); child.wait(); raise
    except (OSError, subprocess.TimeoutExpired):
        raise Blocked('NATIVE_RECOVERY_PROCESS', 'Native process unavailable or timed out; retain uncertain fixture evidence.') from None
    require(len(output) < 4096, 'PROTOCOL_SIZE')
    reply = json.loads(output)
    if child.returncode or reply.get('ok') is not True:
        code = reply.get('code')
        require(isinstance(code, str) and re.fullmatch('[A-Za-z0-9_]{1,96}', code), 'PROTOCOL')
        raise Blocked('NATIVE_RECOVERY_ACTION', 'Native action stopped: ' + code)
    require(reply == {'ok': True, 'provenance': 'native_product_integration', 'funded': False, 'broadcast': False}, 'PROTOCOL')
    return {'pid': child.pid, 'start_id': start, 'exited': True, 'returncode': child.returncode}


class NativeRecovery:
    def __init__(self, cfg, directory, consumer):
        self.config = options(cfg); self.binding = preflight(self.config)  # BEFORE any run/credential access.
        self.directory = Path(directory).absolute()
        self.journal = Journal(self.directory, {'native_recovery_version': 1, 'config': self.config,
                                              'consumer': consumer, 'source': self.binding})
        self.consumer = consumer

    def close(self):
        self.journal.close()

    def execute(self, account, owner_id):
        require(type(owner_id) is int and owner_id > 0, 'ACCOUNT')
        executable, receipt = build(self.config, self.binding)
        self.journal.artifact('build', receipt)
        for chain in CHAINS:
            scope = str(uuid.uuid5(uuid.NAMESPACE_URL, 'native-recovery/' + str(self.directory.resolve()) + '/' + chain))
            case = self.directory / scope
            auth_account = hashlib.sha256(f'recovery-native-v1:{case}:{scope}:{chain}:auth'.encode()).hexdigest()
            config = {'version': 1, 'case_root': str(case), 'run_id': scope, 'chain': chain,
                      'auth_account': auth_account, 'owner_id': owner_id, 'source_head': self.binding['head']}
            records = self.journal.data['artifacts'].setdefault(chain, {})
            pending = self.journal.data['artifacts'].get('pending_native_action')
            if pending is not None and 'retire_remote' not in records:
                require(pending == {'chain': chain, 'action': 'retire_remote', 'config_sha256': digest(config)}
                        and all(action in records for action in ('preflight', 'bind_auth', *ACTIONS)), 'UNCERTAIN_RETIREMENT')
            for action in ('preflight', 'bind_auth', *ACTIONS):
                if action in records:
                    continue  # Exact same consumer/action only; proofs below revalidated.
                if self.journal.data['artifacts'].get('pending_native_action'):
                    raise Blocked('NATIVE_RECOVERY_UNCERTAIN', 'A native action has uncertain outcome; preserve the sealed fixture and reconcile it.')
                require(source(self.config) == self.binding and hashlib.sha256(executable.read_bytes()).hexdigest() == receipt['sha256'], 'BUILD_CHANGED')
                self.journal.artifact('pending_native_action', {'chain': chain, 'action': action, 'config_sha256': digest(config)})
                record = process(executable, {'action': action, 'config': config}, self.directory,
                                 access_token=account['tokens']['access_token'] if action == 'bind_auth' else None)
                records[action] = record
                if action in ('create_export', 'restore'):
                    state = json.loads(private_read(case / 'state.json'))
                    require(state['config'] == config and state['pending'] is None
                            and state['creator' if action == 'create_export' else 'restorer'] == {k: record[k] for k in ('pid', 'start_id')},
                            'PROCESS_BINDING')
                del self.journal.data['artifacts']['pending_native_action']; self.journal.save()
            state_raw = private_read(case / 'state.json')
            if 'retire_remote' in records:
                require(hashlib.sha256(state_raw).hexdigest() == records['state_sha256'], 'RETAINED_STATE_CHANGED')
            state = json.loads(state_raw)
            require(state['config'] == config and state['stage'] in ('signed_unfunded', 'retired_unfunded') and state['pending'] in (None, 'retire_remote')
                    and state['creator']['pid'] != state['restorer']['pid'] != records['reopen_sign']['pid']
                    and state['original_factor'] != state['restored_factor']
                    and not (case / 'original').exists() and (case / 'quarantined-original').is_dir(), 'RESTORED_BINDING')
            raw = private_read(case / 'unfunded-key-control.psbt')
            require(hashlib.sha256(raw).hexdigest() == state['proof_sha256'], 'EXPORT_CHANGED')
            proof = verify(raw, state['identity'], chain)
            records['proof'] = proof; records['state_sha256'] = hashlib.sha256(private_read(case / 'state.json')).hexdigest()
            self.journal.save()
            if 'retire_remote' not in records:
                self.journal.artifact('pending_native_action', {'chain': chain, 'action': 'retire_remote', 'config_sha256': digest(config)})
                records['retire_remote'] = process(executable, {'action': 'retire_remote', 'config': config}, self.directory)
                retired = json.loads(private_read(case / 'state.json'))
                require(retired['stage'] == 'retired_unfunded' and retired['pending'] is None and retired['proof_sha256'] == proof['psbt_sha256'], 'RETIREMENT')
                records['state_sha256'] = hashlib.sha256(private_read(case / 'state.json')).hexdigest()
                del self.journal.data['artifacts']['pending_native_action']; self.journal.save()
        acceptance = {'version': 1, 'consumer': self.consumer, 'source': self.binding, 'helper_sha256': receipt['sha256'],
                      'chains': {chain: self.journal.data['artifacts'][chain]['proof'] for chain in CHAINS},
                      'gui': False, 'funded': False, 'broadcast': False, 'remote_retirement': 'verified_native_fixture_only'}
        self.journal.artifact('native_recovery_acceptance', acceptance)
        return acceptance



def gate(runner):
    cfg = runner.config.get('native_recovery')
    if cfg is None:
        runner.journal.artifact('recovery_coverage', 'legacy_mobile_only')
        return
    suite = NativeRecovery(cfg, runner.journal.directory / 'native-recovery',
                           {'run_id': runner.run_id, 'binding': runner.journal.data['binding']})
    try:
        proof = suite.execute(runner.account, runner.account_user_id)
        runner.journal.artifact('native_recovery_acceptance_version', 1)
        runner.journal.artifact('native_recovery_acceptance', proof)
    finally:
        suite.close()
