"""Disposable retirement boundaries; every cloud/native/chain interaction is fake."""
from copy import deepcopy
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from harness import qa_cleanup as cleanup, qa_funding, sponsor
from harness.provisioning import FreshRunner
from harness.state import Blocked, Failed, atomic_write, digest, private_read
from test_sponsor import address, raw_transaction


@pytest.fixture
def flow(tmp_path, monkeypatch):
    monkeypatch.setattr(qa_funding, 'ROOT', tmp_path / 'registry')
    directory = tmp_path / 'completed'
    snapshot = tmp_path / 'completed-completed-source'
    directory.mkdir(mode=0o700); snapshot.mkdir(mode=0o700)
    run_id = '12345678-1234-5678-abcd-123456789012'
    raw = raw_transaction('b' * 64, [(10000, '0014' + '11' * 20), (89000, '0020' + '22' * 32)])
    txid = sponsor.transaction(raw)['txid']
    state = {'version': 1, 'run_id': run_id, 'cube_uuid': run_id, 'cube_name': 'Testnet4 QA ' + run_id,
             'cube_id': 12, 'vault_id': 23, 'target_key_id': '34', 'target_fingerprint': 'aabbccdd',
             'descriptor_id': '1234abcd', 'owner_user_id': 5, 'registration_started': True,
             'profile_dir': str(directory / 'profile'),
             'spend': {'txid': txid, 'change_sats': 89000, 'change_address': address(34)}}
    artifacts = {'account_identity': 'a' * 64, 'cube_id': 12, 'cube_uuid': run_id, 'vault_id': '23',
                 'descriptor_id': '1234abcd', 'target_key_id': '34', 'target_fingerprint': 'aabbccdd',
                 'receive_address': address(51), 'broadcast_txid': txid, 'public_acceptance': {'txid': txid}, 'request_id': run_id}
    cp = {'artifacts': artifacts, 'binding': 'binding'}
    for target in (directory, snapshot):
        atomic_write(target / 'provision.json', sponsor.encoded(state))
        atomic_write(target / 'checkpoint.json', sponsor.encoded(cp))
        atomic_write(target / 'descriptor.txt', b'synthetic-public-descriptor')
    for name, value in {'signed.hex': raw.encode(), 'local-signed.psbt': b'synthetic-public-psbt',
                        'provision-secrets.keychain.json': b'{"locator":"native-only"}'}.items():
        atomic_write(directory / name, value)
    record = {'version': 1, 'account': 'a' * 64, 'owner': 5, 'run_id': run_id,
              'directory': str(directory), 'snapshot': str(snapshot), 'source_binding': sponsor.read_run(snapshot)[2],
              'cube_id': 12, 'cube_name': state['cube_name'], 'vault_id': 23, 'descriptor_id': '1234abcd',
              'key_id': 34, 'fingerprint': 'aabbccdd', 'keychain_id': 45, 'phase': 'enrolled',
              'encryption_pubkey': '02' + 'ab' * 32,
              'retained_files': {n: sponsor.sha(private_read(directory / n)) for n in
                  ('provision.json', 'descriptor.txt', 'signed.hex', 'local-signed.psbt', 'provision-secrets.keychain.json')}}
    runner = SimpleNamespace(config={'amount_sats': 10000, 'fee_sats': 1000,
                                    'qa_cleanup': {'version': 1, 'preserve_runs': []}},
        strict=SimpleNamespace(requested=False, observe=lambda: None), account_user_id=5,
        artifacts={'account_identity': 'a' * 64}, run_id='new-run', profile=tmp_path / 'current-profile',
        signing_binary='/synthetic/signing', provision=SimpleNamespace(binary='/synthetic/provision'),
        journal=SimpleNamespace(data={'binding': 'new-binding'}, artifact=lambda k, v: runner.artifacts.update({k: v})))
    key = {'id': 34, 'cubeId': 12, 'keychainId': 45, 'network': 'testnet4',
           'fingerprint': 'aabbccdd', 'status': 'active', 'vaultId': 23}
    cube = {'id': 12, 'userId': 5, 'uuid': run_id, 'name': state['cube_name'], 'network': 'testnet4',
            'status': 'active', 'members': [], 'pendingInvites': [], 'lightningAddress': None,
            'encryptionPubkey': '02' + 'ab' * 32,
            'sparkStableBalance': None, 'hasRecoveryKit': False,
            'keys': [{'id': 34, 'primaryOwnerId': 5}], 'vault': {'id': 23}}
    vault = {'id': 23, 'cubeId': 12, 'fingerprint': '1234abcd', 'status': 'active',
             'members': [{'keyId': 34, 'role': 'keyholder'}]}
    cloud = {'cube': cube, 'vault': vault, 'key': key, 'other': [], 'deletes': [],
             'fail_delete': None, 'pending': [], 'decrypt': [], 'residual': 89000}
    def api(_, path, *, delete=False, body=None, missing=False):
        if delete:
            with open(qa_funding.ROOT / 'pool.json') as f: saved = json.load(f)
            r = saved['retirements']['fixture']
            phase = {'/connect/cubes/12/vault': 'vault_intent', '/keys/delete': 'key_intent', '/connect/cubes/12': 'cube_intent'}[path]
            assert r['phase'] == phase  # fsynced before the destructive API call
            cloud['deletes'].append(path)
            if cloud['fail_delete'] == path:
                raise Blocked('SYNTHETIC_LOST_DELETE', 'synthetic transport failure')
            if path.endswith('/vault'):
                cloud['vault'] = None; cloud['cube']['vault'] = None
            elif path == '/keys/delete':
                assert body == {'keyId': 34, 'keychainId': 45}
                cloud['key'] = None; cloud['cube']['keys'] = []
            else: cloud['cube'] = None
            return None
        if path == '/keys': return {'keys': [cloud['key']] if cloud['key'] else []}
        if path == '/connect/cubes': return ([cloud['cube']] if cloud['cube'] else []) + cloud['other']
        if path == '/connect/cubes/12': return deepcopy(cloud['cube'])
        if path == '/connect/cubes/12/vault': return deepcopy(cloud['vault'])
        raise AssertionError(path)
    def http(url, **kwargs):
        if url.endswith('/block-height/0'): return cleanup.GENESIS.encode()
        if url.endswith('/hex'): return raw.encode()
        if url.endswith('/address/' + address(51) + '/utxo'): return b'[]'
        if url.endswith('/address/' + address(34) + '/utxo'):
            return sponsor.encoded([{'txid': txid, 'vout': 1, 'value': cloud['residual']}])
        raise AssertionError(url)
    def native_call(self, action, **kwargs):
        assert action == 'cleanup_audit'
        return {'owner_user_id': 5, 'sessions': cloud['pending'], 'decrypt_cubes': cloud['decrypt']}
    from harness.provisioning import Provision
    monkeypatch.setattr(Provision, 'call', lambda self, action: {'backup_verified': True, 'run_id': run_id})
    monkeypatch.setattr(cleanup.Native, 'call', native_call)
    monkeypatch.setattr(cleanup, 'api', api)
    monkeypatch.setattr(cleanup, 'http', http)
    with qa_funding.pool() as data:
        data['retirements'] = {'fixture': deepcopy(record)}
        qa_funding.save_pool(data)
    return runner, cloud, record


def test_cleanup_retires_exact_cloud_graph_and_keeps_recovery_and_residual(flow):
    runner, cloud, record = flow
    before = {p: p.read_bytes() for p in Path(record['directory']).iterdir()}
    cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == ['/connect/cubes/12/vault', '/keys/delete', '/connect/cubes/12']
    assert before == {p: p.read_bytes() for p in Path(record['directory']).iterdir()}
    assert runner.artifacts['qa_cleanup']['residual_sats'] == 89000
    with qa_funding.pool() as data:
        assert cleanup.retired(data, {'source_run': record['snapshot']})
        assert data['retirements']['fixture']['phase'] == 'complete'
    cleanup.ensure_capacity(runner)
    assert len(cloud['deletes']) == 3  # a second candidate can never be selected


@pytest.mark.parametrize('change', ['owner', 'network', 'members', 'pending_invites', 'shared_key', 'foreign_key', 'vault_members', 'descriptor', 'recovery_kit', 'lightning'])
def test_cleanup_refuses_foreign_shared_or_changed_graph(flow, change):
    runner, cloud, _ = flow
    if change == 'owner': cloud['cube']['userId'] = 99
    elif change == 'network': cloud['key']['network'] = 'mainnet'
    elif change == 'members': cloud['cube']['members'] = [{'id': 99}]
    elif change == 'pending_invites': cloud['cube']['pendingInvites'] = [{'id': 99}]
    elif change == 'shared_key': cloud['other'] = [{'id': 99, 'keys': [{'id': 34}]}]
    elif change == 'foreign_key': cloud['cube']['keys'].append({'id': 99, 'primaryOwnerId': 99})
    elif change == 'vault_members': cloud['vault']['members'].append({'keyId': 99, 'role': 'heir'})
    elif change == 'descriptor': cloud['vault']['fingerprint'] = 'changed'
    elif change == 'recovery_kit': cloud['cube']['hasRecoveryKit'] = True
    else: cloud['cube']['lightningAddress'] = 'personal@example.invalid'
    with pytest.raises(Failed): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


@pytest.mark.parametrize('pending', ['vault', 'key', 'decrypt_numeric', 'decrypt_uuid'])
def test_active_signing_or_recovery_request_prevents_retirement(flow, pending):
    runner, cloud, record = flow
    if pending == 'vault': cloud['pending'] = [{'vault_id': '23', 'key_ids': []}]
    elif pending == 'key': cloud['pending'] = [{'vault_id': 'other', 'key_ids': ['34']}]
    elif pending == 'decrypt_numeric': cloud['decrypt'] = ['12']
    else: cloud['decrypt'] = [record['run_id']]
    with pytest.raises(Failed, match='ACTIVE_REQUEST'): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


@pytest.mark.parametrize('path', ['/connect/cubes/12/vault', '/keys/delete', '/connect/cubes/12'])
def test_crash_after_delete_reconciles_exact_ids_without_second_delete(flow, monkeypatch, path):
    runner, cloud, _ = flow
    api = cleanup.api
    crashed = []
    def lost(*args, **kwargs):
        result = api(*args, **kwargs)
        if kwargs.get('delete') and args[1] == path and not crashed:
            crashed.append(True)
            raise Blocked('LOST_RESPONSE', 'synthetic response lost after server deletion')
        return result
    monkeypatch.setattr(cleanup, 'api', lost)
    with pytest.raises(Blocked): cleanup.ensure_capacity(runner)
    cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == ['/connect/cubes/12/vault', '/keys/delete', '/connect/cubes/12']


def test_key_delete_failure_does_not_cascade_cube(flow):
    runner, cloud, _ = flow
    cloud['fail_delete'] = '/keys/delete'
    with pytest.raises(Blocked): cleanup.ensure_capacity(runner)
    assert cloud['cube'] is not None
    assert '/connect/cubes/12' not in cloud['deletes']


def test_personal_qa_label_is_never_cleanup_authority(flow):
    runner, cloud, _ = flow
    with qa_funding.pool() as data:
        data['retirements'] = {}
        qa_funding.save_pool(data)
    cloud['key']['name'] = 'QA looks-disposable'
    with pytest.raises(Blocked, match='No safely enrolled'): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


@pytest.mark.parametrize('field', ['account', 'owner'])
def test_foreign_enrollment_does_not_authorize_cleanup(flow, field):
    runner, cloud, _ = flow
    with qa_funding.pool() as data:
        data['retirements']['fixture'][field] = 'foreign' if field == 'account' else 99
        qa_funding.save_pool(data)
    with pytest.raises(Blocked): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


def test_preserved_funding_root_uses_ids_even_when_recovery_label_differs(flow):
    runner, cloud, record = flow
    runner.config['qa_cleanup']['preserve_runs'] = [record['directory']]
    cloud['key']['name'] = 'QA recovery-label-from-another-attempt'
    with pytest.raises(Blocked, match='No safely enrolled'): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


@pytest.mark.parametrize('name', ['provision.json', 'descriptor.txt', 'provision-secrets.keychain.json', 'signed.hex'])
def test_changed_recovery_or_history_stops_before_cloud_mutation(flow, name):
    runner, cloud, record = flow
    p = Path(record['directory']) / name
    atomic_write(p, p.read_bytes() + b' ')
    with pytest.raises(Failed): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


def test_unexpected_incoming_funds_stop_retirement(flow):
    runner, cloud, _ = flow
    cloud['residual'] = 90000
    with pytest.raises(Failed, match='UNEXPECTED_FUNDS'): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


def test_capacity_available_performs_no_cleanup(monkeypatch):
    runner = SimpleNamespace(keys=lambda: [], phone_key_label=lambda: 'QA current', artifacts={},
        account={'tokens': {'access_token': 'synthetic'}}, account_user_id=5, config={'qa_cleanup': {'version': 1, 'preserve_runs': []}})
    monkeypatch.setattr('harness.provisioning.http', lambda url, **kwargs: sponsor.encoded(
        {'success': True, 'data': {'entitlements': {'personalKeyLimit': 7}}} if url.endswith('/plan') else
        {'success': True, 'data': {'userId': 5, 'network': 'testnet4', 'noOfKeys': 0, 'keyLimit': 21}}))
    monkeypatch.setattr(cleanup, 'ensure_capacity', lambda _: pytest.fail('cleanup without a capacity refusal'))
    FreshRunner.preflight_capacity(runner)


def test_current_key_resume_performs_no_cleanup(monkeypatch):
    runner = SimpleNamespace(keys=lambda: [{'name': 'QA current', 'network': 'testnet4'}],
        phone_key_label=lambda: 'QA current', artifacts={'phone_key_creation_started': True})
    monkeypatch.setattr(cleanup, 'ensure_capacity', lambda _: pytest.fail('cleanup during current-key resume'))
    FreshRunner.preflight_capacity(runner)


def test_cleanup_after_strict_request_is_refused(flow):
    runner, cloud, _ = flow
    runner.strict.requested = True
    with pytest.raises(Failed, match='MODE'): cleanup.ensure_capacity(runner)
    assert cloud['deletes'] == []


@pytest.mark.parametrize('phase', ['reserved', 'signed', 'broadcasting', 'external', 'quarantined'])
def test_uncertain_funding_claim_protects_source_and_target(flow, phase):
    runner, _, record = flow
    data = {'claims': {'source:1': {'phase': phase, 'entry': {'source_run': record['directory']},
                                  'identity': {'artifacts': {'cube_id': 999}}}}, 'accounts': {}, 'external': {}}
    assert cleanup.protected_ids(runner, data) == {12, 999}


def test_eligible_managed_donor_is_protected_until_change_is_spent(flow, monkeypatch):
    runner, _, record = flow
    runner.config['qa_funding'] = {'version': 1, 'sources': []}
    data = {'claims': {}, 'external': {}, 'accounts': {'a' * 64: [
        {'kind': 'completed', 'entry': {'source_run': record['snapshot']}, 'source_binding': record['source_binding']}]}}
    monkeypatch.setattr(qa_funding, 'source_metadata', lambda *a: (Path(record['snapshot']), None, 500000, {}))
    monkeypatch.setattr(sponsor, 'observe', lambda *a: [{'spent': False}])
    assert cleanup.protected_ids(runner, data) == {12}
    def spent(*a): raise Failed('SOURCE_SPENT', 'synthetic spent donor')
    monkeypatch.setattr(sponsor, 'observe', spent)
    assert cleanup.protected_ids(runner, data) == set()


def test_cloud_tombstone_skips_managed_source_but_not_explicit_protected_root(flow):
    runner, _, record = flow
    cleanup.ensure_capacity(runner)
    with qa_funding.pool() as data:
        config = qa_funding.options({'version': 1, 'sources': [{'source_run': record['directory']}]})
        entry = {'source_run': record['snapshot']}
        data['accounts']['a' * 64] = [{'kind': 'completed', 'entry': entry,
            'source_binding': record['source_binding'], 'authorization_roots': sorted(qa_funding.root_keys(config))}]
        assert list(qa_funding.entries(data, 'a' * 64, config)) == [(config['sources'][0], None)]
    assert Path(record['snapshot']).is_dir()
    assert Path(record['directory'], 'provision-secrets.keychain.json').exists()


def test_no_second_fixture_for_one_consumer_even_when_first_did_not_free_capacity(flow):
    runner, cloud, record = flow
    cleanup.ensure_capacity(runner)
    with qa_funding.pool() as data:
        second = deepcopy(record); second['cube_id'] = 99
        data['retirements']['unrelated-second'] = second
        qa_funding.save_pool(data)
    cleanup.ensure_capacity(runner)
    assert len(cloud['deletes']) == 3


def test_completed_strict_run_auto_enrolls_without_enabling_funding(flow, monkeypatch):
    from harness.provisioning import STAGES
    runner, _, record = flow
    runner.stop = False
    runner.run_id = record['run_id']
    runner.journal.directory = Path(record['directory'])
    cp = json.loads(private_read(runner.journal.directory / 'checkpoint.json'))
    cp.update(version=1, binding=digest(runner.config), stages={k: {'status': 'passed'} for k in STAGES})
    raw = private_read(runner.journal.directory / 'signed.hex').decode()
    cp['artifacts'].update(signature_count_min=2, strict_pin_input={'completed': True},
        strict_home_refresh={'home_refresh_proven': True}, session_id='complete-session', fee_sats=1000,
        broadcast_hex_sha256=sponsor.sha(raw.encode()))
    for directory in (Path(record['directory']), Path(record['snapshot'])):
        atomic_write(directory / 'checkpoint.json', sponsor.encoded(cp))
    runner.journal.data = cp
    runner.strict.require_completion = lambda request_id, session_id: None
    monkeypatch.setattr(qa_funding, 'public_transaction', lambda _: {'fee': 1000})
    monkeypatch.setattr(qa_funding, 'http', lambda *a, **k: raw.encode())
    with qa_funding.pool() as data:
        data['retirements'] = {}
        qa_funding.save_pool(data)
    cleanup.register_completed(runner)
    with qa_funding.pool() as data:
        assert data['accounts'] == {}  # cleanup authorization does not enroll a funding source
        enrolled = next(iter(data['retirements'].values()))
        assert enrolled['run_id'] == record['run_id'] and enrolled['key_id'] == 34
        assert enrolled['phase'] == 'enrolled'
    cleanup.register_completed(runner)
    with qa_funding.pool() as data:
        assert len(data['retirements']) == 1


def test_incomplete_run_cannot_auto_enroll_cleanup(flow):
    runner, _, record = flow
    runner.stop = False
    runner.run_id = record['run_id']
    runner.journal.directory = Path(record['directory'])
    with pytest.raises((Failed, KeyError)):
        cleanup.register_completed(runner)
