"""Offline funding registry boundaries; no device, keys or network."""
import pytest

from harness import qa_funding
from harness.state import Blocked, Failed


def test_pool_excludes_concurrent_owner(tmp_path, monkeypatch):
    monkeypatch.setattr(qa_funding, 'ROOT', tmp_path / 'registry')
    with qa_funding.pool():
        with pytest.raises(Blocked) as caught:
            with qa_funding.pool():
                pytest.fail('second owner admitted')
        assert caught.value.code == 'QA_FUNDING_BUSY'


def test_pool_rejects_orphan_claim(tmp_path, monkeypatch):
    monkeypatch.setattr(qa_funding, 'ROOT', tmp_path / 'registry')
    with qa_funding.pool() as data:
        data['claims']['tx:1'] = {'target_key': 'missing'}
        qa_funding.save_pool(data)
    with pytest.raises(Failed) as caught:
        with qa_funding.pool():
            pytest.fail('orphan claim admitted')
    assert caught.value.code == 'QA_FUNDING_REGISTRY_INDEX'


def test_empty_explicit_allowlist_does_not_authorize_unrelated_completed_run():
    config = qa_funding.options({'version': 1, 'sources': []})
    unrelated = {'kind': 'completed', 'entry': {'source_run': '/unrelated/qa/run'}}
    data = {'accounts': {'same-account': [unrelated]}}
    assert list(qa_funding.entries(data, 'same-account', config)) == []


def test_disabled_registration_does_not_import_managed_sources():
    config = qa_funding.options({'version': 1, 'sources': [], 'register_completed': False})
    data = {'accounts': {'same-account': [
        {'kind': 'completed', 'entry': {'source_run': '/unrelated/qa/run'}}]}}
    assert list(qa_funding.entries(data, 'same-account', config)) == []


# Use the native sponsor protocol's synthetic transaction fixtures. The
# orchestrator still executes its real intent/receipt validation and file I/O.
from copy import deepcopy
import json
from pathlib import Path
from types import SimpleNamespace
from test_sponsor import scenario, no_external, address, native_reply
from harness import sponsor
from harness.state import atomic_write, digest, private_read


@pytest.fixture
def flow(scenario, monkeypatch, tmp_path):
    args, evidence = scenario
    monkeypatch.setattr(qa_funding, 'ROOT', tmp_path / 'registry')
    config = {'amount_sats': 10000, 'fee_sats': 1000,
              'qa_funding': {'version': 1, 'sources': [{'source_run': args.source_run}],
                             'faucet_wait_seconds': 0, 'register_completed': True}}
    live = Path(args.target_run)
    state = json.loads(private_read(live / 'provision.json'))
    state.update(version=1, cube_id=12, cube_uuid=state['run_id'], spend=None)
    atomic_write(live / 'provision.json', sponsor.encoded(state))
    cp = json.loads(private_read(live / 'checkpoint.json'))
    cp.update(version=1, binding=digest(config), stages={})
    cp['artifacts'].update(desktop_identity='d' * 64, cube_id=12, cube_uuid=state['run_id'],
        target_key_id='synthetic-phone-key', target_fingerprint='1234abcd', strict_account='a' * 64,
        strict_acceptance={'version': 1, 'config_binding': digest(config)},
        strict_process={'device': 'e' * 64, 'build': 41,
                        'process': {'bundle_id': 'io.coincube.keychainApp', 'pid': 42}},
        qa_build={'define': 'COINCUBE_QA_HOME_REFRESH=true', 'app': {'build': 41}})
    atomic_write(live / 'checkpoint.json', sponsor.encoded(cp))
    def artifact(k, v):
        cp['artifacts'][k] = v
        atomic_write(live / 'checkpoint.json', sponsor.encoded(cp))
    journal = SimpleNamespace(directory=live, data=cp, artifact=artifact)
    wire = {'published': False, 'posts': [], 'native': 0, 'late': None}
    def native(command, *, input, **kwargs):
        wire['native'] += 1
        request = json.loads(input)
        receipt = native_reply(request)
        wire['receipt'] = receipt
        return SimpleNamespace(returncode=0, stdout=sponsor.encoded(receipt))
    monkeypatch.setattr(sponsor.subprocess, 'run', native)
    def http(url, *, body=None, **kwargs):
        if url.endswith('/block-height/0'):
            return qa_funding.GENESIS.encode()
        if body is not None:
            # Durable attempt and signed receipt must precede network mutation.
            data = json.loads(private_read(qa_funding.ROOT / 'pool.json'))
            claim = next(iter(data['claims'].values()))
            assert claim['phase'] == 'broadcasting' and claim['receipt_sha256']
            wire['posts'].append(body)
            wire['published'] = True
            return wire['receipt']['txid'].encode()
        if url.endswith('/hex'):
            return wire['receipt']['raw_tx'].encode() if wire['published'] else None
        if url.endswith('/outspend/0'):
            return b'{"spent":false}'
        raise AssertionError(url)
    monkeypatch.setattr(qa_funding, 'http', http)
    runner = SimpleNamespace(config=config, journal=journal, run_id=state['run_id'],
        artifacts=cp['artifacts'], strict=SimpleNamespace(requested=False, observe=lambda: None),
        provision=SimpleNamespace(binary=args.native_binary), deadline=0,
        core=SimpleNamespace(utxos=lambda _: [wire['late']] if wire['late'] else [],
                             raw=lambda _: wire.get('receipt', {}).get('raw_tx', evidence[0]['raw_tx'])),
        pin_artifact=artifact)
    return runner, wire


def test_automatic_funding_signs_broadcasts_and_adopts_exact_outpoint(flow):
    runner, wire = flow
    qa_funding.Funding(runner).execute()
    point = runner.artifacts['funding_outpoint']
    assert point == {'txid': wire['receipt']['txid'], 'vout': 0, 'value_sats': 500000}
    assert len(wire['posts']) == 1 and wire['native'] == 1
    assert wire['posts'] == [wire['receipt']['raw_tx'].encode()]
    assert private_read(runner.journal.directory / 'funding.hex') == wire['posts'][0]
    with qa_funding.pool() as data:
        assert next(iter(data['claims'].values()))['phase'] == 'accepted'
        enrolled = data['accounts']['a' * 64][0]
        assert enrolled['authorization_roots'] == sorted(qa_funding.root_keys(qa_funding.options(runner.config['qa_funding'])))


@pytest.mark.parametrize('phase', ['reserved', 'signed', 'broadcasting'])
def test_crash_recovery_reuses_one_reserved_plan(flow, monkeypatch, phase):
    runner, wire = flow
    save = qa_funding.save_pool
    crashed = []
    def fault(data):
        save(data)
        if not crashed and any(c['phase'] == phase for c in data['claims'].values()):
            crashed.append(True)
            raise RuntimeError('synthetic crash after durable write')
    monkeypatch.setattr(qa_funding, 'save_pool', fault)
    with pytest.raises(RuntimeError):
        qa_funding.Funding(runner).execute()
    with qa_funding.pool() as data:
        before = deepcopy(data['claims'])
    monkeypatch.setattr(qa_funding, 'save_pool', save)
    qa_funding.Funding(runner).execute()
    with qa_funding.pool() as data:
        assert list(data['claims']) == list(before)
        assert next(iter(data['claims'].values()))['intent_sha256'] == next(iter(before.values()))['intent_sha256']
    assert len(wire['posts']) == 1
    assert wire['native'] == 1


def test_lost_post_response_reconciles_without_second_payment(flow, monkeypatch):
    runner, wire = flow
    http = qa_funding.http
    def lost(url, **kwargs):
        result = http(url, **kwargs)
        if kwargs.get('body'):
            raise Blocked('HTTP_UNCERTAIN', 'synthetic lost response')
        return result
    monkeypatch.setattr(qa_funding, 'http', lost)
    qa_funding.Funding(runner).execute()
    assert len(wire['posts']) == 1
    assert runner.artifacts['funding_outpoint']['txid'] == wire['receipt']['txid']


def test_partial_propagation_does_not_post_or_select_another_source(flow, monkeypatch):
    runner, wire = flow
    monkeypatch.setattr(qa_funding.Funding, 'observed', lambda *a: 'partial')
    with pytest.raises(Blocked) as caught:
        qa_funding.Funding(runner).execute()
    assert caught.value.code == 'QA_FUNDING_PROPAGATION'
    assert wire['posts'] == [] and wire['native'] == 1


def test_late_faucet_is_adopted_without_signing(flow):
    runner, wire = flow
    wire['late'] = ('f' * 64, 0, 100000)
    qa_funding.Funding(runner).execute()
    assert runner.artifacts['funding_outpoint']['txid'] == 'f' * 64
    assert wire['native'] == 0 and wire['posts'] == []
    with qa_funding.pool() as data:
        assert data['claims'] == {}


@pytest.mark.parametrize('field,value', [('strict_process', None), ('target_fingerprint', ''), ('qa_build', {})])
def test_missing_target_identity_prevents_key_lookup(flow, field, value):
    runner, wire = flow
    runner.journal.artifact(field, value)
    with pytest.raises(Failed):
        qa_funding.Funding(runner).execute()
    assert wire['native'] == 0 and wire['posts'] == []


def test_target_process_mutation_after_prepare_prevents_signature(flow, monkeypatch):
    runner, wire = flow
    execute = sponsor.execute
    def changed(args):
        result = execute(args)
        process = deepcopy(runner.artifacts['strict_process'])
        process['process']['pid'] += 1
        runner.journal.artifact('strict_process', process)
        return result
    monkeypatch.setattr(sponsor, 'execute', changed)
    with pytest.raises(Failed) as caught:
        qa_funding.Funding(runner).execute()
    assert caught.value.code == 'QA_FUNDING_TARGET_CHANGED'
    assert wire['native'] == 0 and wire['posts'] == []


def test_signed_receipt_tamper_never_posts(flow, monkeypatch):
    runner, wire = flow
    monkeypatch.setattr(qa_funding.Funding, 'observed', lambda *a: 'partial')
    with pytest.raises(Blocked):
        qa_funding.Funding(runner).execute()
    output = runner.journal.directory.with_name(runner.journal.directory.name + '-funding')
    raw = json.loads(private_read(output / 'sponsor-receipt.json'))
    raw['raw_tx'] += '00'
    atomic_write(output / 'sponsor-receipt.json', sponsor.encoded(raw))
    with pytest.raises(Failed) as caught:
        qa_funding.Funding(runner).execute()
    assert caught.value.code == 'QA_FUNDING_RECEIPT_CHANGED'
    assert wire['posts'] == []


@pytest.mark.parametrize('change', [{'maximum_amount_sats': 500001}, {'fee_sats': 1001},
                                    {'maximum_amount_sats': True}, {'faucet_wait_seconds': -1},
                                    {'version': True}, {'sources': [{'source_run': 'relative'}]}])
def test_automatic_pool_limits(change):
    with pytest.raises(Failed):
        qa_funding.options({'version': 1, 'sources': [], **change})


def test_global_source_reservation_cannot_be_claimed_by_another_run(flow):
    runner, wire = flow
    source = Path(runner.config['qa_funding']['sources'][0]['source_run'])
    txid = json.loads(private_read(source / 'provision.json'))['spend']['txid']
    with qa_funding.pool() as data:
        data['claims'][txid + ':1'] = {'target_key': 'other-config-and-run', 'phase': 'reserved'}
        data['runs']['other-config-and-run'] = txid + ':1'
        qa_funding.save_pool(data)
    with pytest.raises(Blocked) as caught:
        qa_funding.Funding(runner).execute()
    assert caught.value.code == 'QA_FUNDING_EMPTY'
    assert wire['native'] == 0 and wire['posts'] == []


def test_foreign_account_source_never_reaches_native_signer(flow):
    runner, wire = flow
    source = Path(runner.config['qa_funding']['sources'][0]['source_run'])
    cp = json.loads(private_read(source / 'checkpoint.json'))
    cp['artifacts']['account_identity'] = 'b' * 64
    atomic_write(source / 'checkpoint.json', sponsor.encoded(cp))
    with pytest.raises(Failed) as caught:
        qa_funding.Funding(runner).execute()
    assert caught.value.code == 'QA_FUNDING_ACCOUNT'
    assert wire['native'] == 0 and wire['posts'] == []


def test_exhausted_source_pool_blocks_without_faucet_or_signer_retry(flow, monkeypatch):
    runner, wire = flow
    def spent(*args):
        raise Failed('SOURCE_SPENT', 'synthetic spent source')
    monkeypatch.setattr(sponsor, 'observe', spent)
    with pytest.raises(Blocked) as caught:
        qa_funding.Funding(runner).execute()
    assert caught.value.code == 'QA_FUNDING_EMPTY'
    assert wire['native'] == 0 and wire['posts'] == []


def test_automatic_source_descendants_are_scoped_to_explicit_roots():
    config = qa_funding.options({'version': 1, 'sources': [{'source_run': '/pool/seed'}]})
    root = next(iter(qa_funding.root_keys(config)))
    own = {'kind': 'completed', 'entry': {'source_run': '/pool/descendant'}, 'authorization_roots': [root]}
    unrelated = {'kind': 'completed', 'entry': {'source_run': '/other/descendant'}, 'authorization_roots': ['other']}
    data = {'accounts': {'account': [own, unrelated]}}
    assert [e for e, _ in qa_funding.entries(data, 'account', config)] == [own['entry'], {'source_run': '/pool/seed'}]


@pytest.mark.parametrize('crash', [False, True])
def test_completed_registration_snapshot_survives_live_checkpoint_replay(flow, monkeypatch, crash):
    from harness.provisioning import STAGES
    runner, wire = flow
    qa_funding.Funding(runner).execute()
    live = runner.journal.directory
    raw = wire['receipt']['raw_tx']
    state = json.loads(private_read(live / 'provision.json'))
    state['spend'] = {'txid': wire['receipt']['txid'], 'change_sats': 488000, 'change_address': address(34)}
    atomic_write(live / 'provision.json', sponsor.encoded(state))
    cp = runner.journal.data
    cp['stages'] = {k: {'status': 'passed', 'at': 1} for k in STAGES}
    cp['artifacts'].update(signature_count_min=2, strict_pin_input={'completed': True},
        strict_home_refresh={'home_refresh_proven': True}, request_id='request', session_id='session',
        broadcast_txid=wire['receipt']['txid'], public_acceptance={'txid': wire['receipt']['txid']},
        broadcast_hex_sha256=sponsor.sha(raw.encode()), fee_sats=1000)
    atomic_write(live / 'checkpoint.json', sponsor.encoded(cp))
    atomic_write(live / 'signed.hex', raw.encode())
    calls = []
    runner.strict.require_completion = lambda *args: calls.append(args)
    monkeypatch.setattr(qa_funding, 'public_transaction', lambda _: {'fee': 1000})
    if crash:
        save = qa_funding.save_pool
        monkeypatch.setattr(qa_funding, 'save_pool', lambda _: (_ for _ in ()).throw(RuntimeError('crash before enrollment')))
        with pytest.raises(RuntimeError):
            qa_funding.register_completed(runner)
        monkeypatch.setattr(qa_funding, 'save_pool', save)
    qa_funding.register_completed(runner)
    with qa_funding.pool() as data:
        item = deepcopy(data['accounts']['a' * 64][0])
    snapshot = Path(item['entry']['source_run'])
    original = {n: private_read(snapshot / n) for n in qa_funding.FILES}
    cp['stages']['verify']['at'] = 2
    cp['artifacts']['public_acceptance']['confirmed'] = True
    atomic_write(live / 'checkpoint.json', sponsor.encoded(cp))
    qa_funding.register_completed(runner)
    with qa_funding.pool() as data:
        assert data['accounts']['a' * 64][0] == item
        assert len([e for e in data['accounts']['a' * 64] if e['kind'] == 'completed']) == 1
    assert {n: private_read(snapshot / n) for n in qa_funding.FILES} == original
    assert json.loads(original['provision.json'])['profile_dir'] == str(live / 'profile')
    assert calls == [('request', 'session')] * (3 if crash else 2)
    # The donor snapshot remains compatible with the native sponsor protocol,
    # including the original secret locator's profile and run identity.
    future = live.with_name('future')
    future.mkdir(mode=0o700)
    future_state = deepcopy(state)
    future_state.update(run_id='future', cube_uuid='future', vault_id=99, profile_dir=str(future / 'profile'), spend=None)
    future_cp = deepcopy(cp)
    future_cp['artifacts'].update(vault_id='99', receive_address=address(68))
    atomic_write(future / 'provision.json', sponsor.encoded(future_state))
    atomic_write(future / 'checkpoint.json', sponsor.encoded(future_cp))
    atomic_write(future / 'descriptor.txt', b'future-public-descriptor')
    monkeypatch.setattr(sponsor, 'observe', lambda txid, vout: [{'raw_tx': raw}])
    plan, _, donor = sponsor.make_intent(snapshot, future, 400000, 1000)
    assert plan['source'] == item['source_binding']
    assert donor['profile_dir'] == str(live / 'profile') and donor['run_id'] == runner.run_id
    runner.config['qa_funding']['sources'].append({'source_run': '/unapproved/new-root'})
    with pytest.raises(Failed) as caught:
        qa_funding.register_completed(runner)
    assert caught.value.code == 'QA_FUNDING_COMPLETION'


def test_faucet_failure_automatically_enters_fallback(flow, monkeypatch):
    from harness import provisioning
    runner, wire = flow
    runner.config['qa_funding']['register_completed'] = False  # no registry enrollment in this integration stub
    def unavailable(*args):
        raise Blocked('FAUCET_UNCERTAIN', 'synthetic 503')
    monkeypatch.setattr(provisioning, 'faucet_claim', unavailable)
    calls = []
    monkeypatch.setattr(qa_funding, 'Funding', lambda r: SimpleNamespace(execute=lambda: calls.append(r)))
    provisioning.FreshRunner.stage_funding(runner)
    assert calls == [runner]
    assert runner.artifacts['qa_faucet_failure']['code'] == 'FAUCET_UNCERTAIN'
    assert wire['native'] == 0
