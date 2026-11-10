"""Synthetic provisioning boundary tests: never contact services or a device."""
from copy import deepcopy
import hashlib
import io
import json
from types import SimpleNamespace
from urllib.error import HTTPError, URLError

import pytest

from harness import provisioning as provision
from harness.state import Blocked, Failed, private_read


TXID = "a" * 64
FUNDING_TXID = "b" * 64
ADDRESS = "tb1q" + "c" * 58


@pytest.fixture(autouse=True)
def forbid_external_io(monkeypatch):
    def forbidden(*args, **kwargs):
        raise AssertionError("Synthetic provisioning tests must not use external I/O")
    monkeypatch.setattr(provision, "http", forbidden)
    monkeypatch.setattr(provision, "build_opener", forbidden)
    monkeypatch.setattr(provision.subprocess, "run", forbidden)
    monkeypatch.setattr(provision, "attach", forbidden)
    monkeypatch.setattr(provision.time, "sleep", lambda _: None)


@pytest.fixture
def config(tmp_path):
    return {"version": 2, "account_email": "qa@example.invalid", "profile_dir": str(tmp_path / "desktop"),
            "connect_rest": provision.REST, "connect_grpc": provision.GRPC,
            "ios": {"minimum_build": 35}, "amount_sats": 10000, "fee_sats": 612}


@pytest.fixture
def runner(config, tmp_path):
    result = provision.FreshRunner(config, tmp_path / "run", deadline=0)
    yield result
    result.close()


@pytest.mark.parametrize("field,value,code", [
    ("fixture", {}, "CONFIG_SCHEMA"),
    ("version", 1, "CONFIG_SCHEMA"),
    ("connect_rest", "https://dev-api.coincube.io", "CONNECT_ENVIRONMENT"),
    ("connect_grpc", "http://127.0.0.1:50051", "CONNECT_ENVIRONMENT"),
    ("profile_dir", "relative", "ABSOLUTE_PATH"),
    ("amount_sats", True, "SPEND_LIMIT"),
    ("amount_sats", 545, "SPEND_LIMIT"),
    ("amount_sats", 999, "SPEND_LIMIT"),
    ("amount_sats", 100001, "SPEND_LIMIT"),
    ("fee_sats", 10001, "SPEND_LIMIT"),
    ("ios", {"minimum_build": 34}, "IOS_BUILD"),
    ("ios", {"minimum_build": 35, "pin_file": "/fixture/pin"}, "IOS_CONFIG"),
])
def test_fresh_config_rejects_fixtures_environment_and_limits(config, field, value, code):
    config[field] = value
    with pytest.raises(Failed) as caught:
        provision.validate_config(config)
    assert caught.value.code == code


def test_native_response_cannot_select_another_chain(tmp_path, monkeypatch):
    monkeypatch.setattr(provision.subprocess, "run", lambda *a, **k: SimpleNamespace(
        returncode=0, stdout=b'{"ok":true,"network":"bitcoin"}'))
    native = provision.Provision("/synthetic/native", tmp_path, tmp_path, "run")
    with pytest.raises(Failed) as caught:
        native.call("cube")
    assert caught.value.code == "PROVISION_CHAIN"


def faucet_stub(monkeypatch, responses, journal):
    calls = []
    outcomes = iter(responses)
    def send(request, timeout):
        # The durable attempt record must exist before a network mutation.
        assert json.loads(private_read(journal.path))["artifacts"]["funding_claim_started"] is True
        assert request.get_header("User-agent") == "COINCUBE-Testnet4-QA/1.0"
        assert request.get_header("Content-type") == "application/json"
        calls.append(json.loads(request.data))
        outcome = next(outcomes)
        if isinstance(outcome, BaseException):
            raise outcome
        return io.BytesIO(json.dumps(outcome).encode())
    monkeypatch.setattr(provision, "build_opener", lambda *a: SimpleNamespace(open=send))
    return calls


def test_faucet_lost_response_is_not_reclaimed_after_process_restart(runner, config, monkeypatch):
    directory = runner.journal.directory
    calls = faucet_stub(monkeypatch, [URLError("response lost")], runner.journal)
    with pytest.raises(Blocked) as caught:
        provision.faucet_claim(ADDRESS, runner.journal)
    assert caught.value.code == "FAUCET_UNCERTAIN"
    runner.close()
    resumed = provision.FreshRunner(config, directory)
    try:
        with pytest.raises(Blocked) as caught:
            provision.faucet_claim(ADDRESS, resumed.journal)
        assert caught.value.code == "FUNDING_PENDING"
        assert len(calls) == 1
    finally:
        resumed.close()


def test_explicit_faucet_rate_limit_waits_then_allows_one_retry(runner, monkeypatch):
    clock = [1000]
    monkeypatch.setattr(provision.time, "time", lambda: clock[0])
    rate_limit = HTTPError(provision.FAUCET, 429, "rate limit", {}, io.BytesIO(b'{"retry_after":120}'))
    calls = faucet_stub(monkeypatch, [rate_limit, {"ok": True, "network": "btc-testnet", "txid": FUNDING_TXID}], runner.journal)
    with pytest.raises(Blocked) as caught:
        provision.faucet_claim(ADDRESS, runner.journal)
    assert caught.value.code == "FAUCET_RATE_LIMIT"
    assert runner.artifacts["faucet_retry_at"] == 1120
    assert runner.artifacts["funding_claim_started"] is False
    clock[0] = 1119
    with pytest.raises(Blocked):
        provision.faucet_claim(ADDRESS, runner.journal)
    assert len(calls) == 1
    clock[0] = 1120
    provision.faucet_claim(ADDRESS, runner.journal)
    assert len(calls) == 2
    assert runner.artifacts["funding_claim_txid"] == FUNDING_TXID
    assert runner.artifacts["funding_claim_started"] is True


@pytest.mark.parametrize("response", [{"ok": False}, {"ok": True, "network": "bitcoin", "txid": FUNDING_TXID}])
def test_invalid_faucet_reply_preserves_uncertain_attempt(runner, monkeypatch, response):
    calls = faucet_stub(monkeypatch, [response], runner.journal)
    with pytest.raises(Blocked) as caught:
        provision.faucet_claim(ADDRESS, runner.journal)
    assert caught.value.code == "FAUCET_RESPONSE"
    with pytest.raises(Blocked):
        provision.faucet_claim(ADDRESS, runner.journal)
    assert len(calls) == 1


def explorer_responses(monkeypatch, *, left=None, right=None, wrong_genesis=False):
    def get(url, **kwargs):
        is_left = url.startswith(provision.EXPLORERS[0])
        if url.endswith("/block-height/0"):
            return (("0" * 64) if wrong_genesis and not is_left else provision.GENESIS).encode()
        result = left if is_left else right
        return result.encode() if isinstance(result, str) else json.dumps(result).encode()
    monkeypatch.setattr(provision, "http", get)


def test_both_explorers_must_confirm_testnet4_genesis(monkeypatch):
    explorer_responses(monkeypatch, wrong_genesis=True)
    with pytest.raises(Failed) as caught:
        provision.Explorer().chain()
    assert caught.value.code == "EXPLORER_CHAIN"


def test_disagreeing_funding_observations_are_not_selected(monkeypatch):
    entry = {"txid": FUNDING_TXID, "vout": 0, "value": 100000}
    explorer_responses(monkeypatch, left=[entry], right=[])
    with pytest.raises(Blocked) as caught:
        provision.Explorer().utxos(ADDRESS)
    assert caught.value.code == "FUNDING_PROPAGATION"


@pytest.mark.parametrize("right", ["00bb", "a", "not hex"])
def test_raw_funding_requires_identical_valid_bytes(monkeypatch, right):
    explorer_responses(monkeypatch, left="00aa", right=right)
    with pytest.raises(Failed) as caught:
        provision.Explorer().raw(FUNDING_TXID)
    assert caught.value.code == "FUNDING_RAW"


def test_lost_faucet_reply_reconciles_address_without_another_claim(runner):
    runner.journal.artifact("receive_address", ADDRESS)
    runner.journal.artifact("funding_claim_started", True)
    runner.core = SimpleNamespace(utxos=lambda _: [(FUNDING_TXID, 1, 100000)], raw=lambda _: "00aa")
    runner.stage_funding()
    assert runner.artifacts["funding_outpoint"] == {"txid": FUNDING_TXID, "vout": 1, "value_sats": 100000}
    assert private_read(runner.journal.directory / "funding.hex") == b"00aa"
    # A later rerun keeps the original funding identity even if unavailable.
    runner.core = SimpleNamespace()
    runner.stage_funding()


def test_missing_funding_after_claim_blocks_without_replacement(runner):
    runner.journal.artifact("receive_address", ADDRESS)
    runner.journal.artifact("funding_claim_started", True)
    runner.core = SimpleNamespace(utxos=lambda _: [])
    with pytest.raises(Blocked) as caught:
        runner.stage_funding()
    assert caught.value.code == "FUNDING_PENDING"
    assert "funding_outpoint" not in runner.artifacts


def test_multiple_eligible_funding_outputs_never_guess(runner):
    runner.journal.artifact("receive_address", ADDRESS)
    runner.journal.artifact("funding_claim_started", True)
    runner.core = SimpleNamespace(utxos=lambda _: [(FUNDING_TXID, 0, 100000), (FUNDING_TXID, 1, 100000)])
    with pytest.raises(Failed) as caught:
        runner.stage_funding()
    assert caught.value.code == "FUNDING_AMBIGUOUS"


def phone_key(runner, **changes):
    key = {"id": 12, "name": "QA " + runner.run_id, "network": "testnet4", "cubeId": None,
           "xpub": "synthetic-public-key", "fingerprint": "aabbccdd", "derivationPath": "m/48h/1h/0h/2h"}
    return dict(key, **changes)


@pytest.mark.parametrize("kind,code", [("baseline", "KEY_NOT_FRESH"), ("duplicate", "KEY_AMBIGUOUS"),
                                      ("foreign_cube", "KEY_OWNERSHIP"), ("blind", "KEY_OWNERSHIP")])
def test_new_key_must_have_exact_unambiguous_identity(runner, kind, code):
    runner.journal.artifact("cube_id", 17)
    runner.journal.artifact("phone_key_creation_started", True)
    runner.journal.artifact("baseline_key_ids", [])
    key = phone_key(runner)
    if kind == "baseline":
        runner.journal.artifact("baseline_key_ids", [str(key["id"])])
    if kind == "foreign_cube":
        key["cubeId"] = 18
    if kind == "blind":
        key["xpub"] = ""
    runner.keys = lambda: [key, deepcopy(key)] if kind == "duplicate" else [key]
    with pytest.raises(Failed) as caught:
        runner.stage_phone_key()
    assert caught.value.code == code
    assert "target_key_id" not in runner.artifacts


def test_existing_matching_label_without_creation_evidence_is_not_fresh(runner):
    runner.journal.artifact("cube_id", 17)
    runner.keys = lambda: [phone_key(runner)]
    with pytest.raises(Failed) as caught:
        runner.stage_phone_key()
    assert caught.value.code == "KEY_NOT_FRESH"


def test_resumed_key_requires_durable_baseline(runner):
    runner.journal.artifact("cube_id", 17)
    runner.journal.artifact("phone_key_creation_started", True)
    runner.keys = lambda: [phone_key(runner)]
    with pytest.raises(Failed) as caught:
        runner.stage_phone_key()
    assert caught.value.code == "KEY_NOT_FRESH"


def test_registered_key_resumes_without_creating_another(runner):
    runner.journal.artifact("cube_id", 17)
    runner.journal.artifact("phone_key_creation_started", True)
    runner.journal.artifact("baseline_key_ids", ["1", "2"])
    runner.onboarding = SimpleNamespace(reconcile_pin=lambda _: None)
    runner.keys = lambda: [phone_key(runner)]
    runner.stage_phone_key()
    assert runner.artifacts["target_key_id"] == "12"
    assert runner.artifacts["target_fingerprint"] == "aabbccdd"
    saved = json.loads(private_read(runner.journal.directory / "phone-key.json"))
    assert saved["network"] == "testnet4"
    assert saved["key_id"] == "12"
    runner.stage_phone_key()


def broadcast_ready(runner):
    runner.intent = {"txid": TXID, "inputs": [FUNDING_TXID + ":1"],
                     "outputs": [{"address": "tb1recipient", "sats": 10000}, {"address": "tb1change", "sats": 89388}]}
    runner.journal.artifact("fee_sats", 612)
    runner.core = SimpleNamespace(chain=lambda: None, known=lambda _: False)
    final = {"txid": TXID, "fee_sats": 612, "raw_tx": "00aa",
             "inputs": [{"txid": FUNDING_TXID, "vout": 1}],
             "outputs": [{"address": out["address"], "value_sats": out["sats"]} for out in runner.intent["outputs"]]}
    runner.provision = SimpleNamespace(call=lambda *a, **k: final)
    return final


@pytest.mark.parametrize("field,value", [("txid", "d" * 64), ("fee_sats", 613),
                                        ("inputs", []), ("outputs", [])])
def test_final_transaction_cannot_change_pinned_intent(runner, field, value):
    final = broadcast_ready(runner)
    final[field] = value
    with pytest.raises(Failed) as caught:
        runner.stage_broadcast()
    assert caught.value.code == "FINAL_INTENT"
    assert "broadcast_hex_sha256" not in runner.artifacts


def test_final_signed_bytes_cannot_change_after_attempt(runner):
    broadcast_ready(runner)
    runner.journal.artifact("broadcast_hex_sha256", hashlib.sha256(b"00bb").hexdigest())
    with pytest.raises(Failed) as caught:
        runner.stage_broadcast()
    assert caught.value.code == "PROVISION_CHANGED"


def test_broadcast_lost_response_reconciles_before_resending(runner, monkeypatch):
    broadcast_ready(runner)
    sent = []
    def send(url, *, body, headers):
        assert private_read(runner.journal.directory / "signed.hex") == body
        assert json.loads(private_read(runner.journal.path))["artifacts"]["broadcast_hex_sha256"] == hashlib.sha256(body).hexdigest()
        sent.append(body)
        raise Blocked("SERVICE_UNREACHABLE", "Synthetic lost response")
    monkeypatch.setattr(provision, "http", send)
    with pytest.raises(Blocked):
        runner.stage_broadcast()
    runner.core.known = lambda _: True
    runner.provision = SimpleNamespace()  # Reconciliation needs no finalization.
    runner.stage_broadcast()
    assert sent == [b"00aa"]
    assert runner.artifacts["broadcast_txid"] == TXID


def test_spent_input_does_not_create_replacement_funding(runner):
    runner.intent = {"txid": TXID}
    runner.native_call = lambda _: {"fee_sats": 612}
    runner.core = SimpleNamespace(known=lambda _: False, utxos=lambda _: [])
    runner.journal.artifact("receive_address", ADDRESS)
    runner.journal.artifact("funding_outpoint", {"txid": FUNDING_TXID, "vout": 1, "value_sats": 100000})
    with pytest.raises(Blocked) as caught:
        runner.stage_connect()
    assert caught.value.code == "INPUT_SPENT"


def test_run_identity_is_stable_isolated_and_exclusively_locked(config, tmp_path):
    first = provision.FreshRunner(config, tmp_path / "first")
    try:
        with pytest.raises(Blocked) as caught:
            provision.FreshRunner(config, tmp_path / "first")
        assert caught.value.code == "RUN_BUSY"
        other = provision.FreshRunner(config, tmp_path / "second")
        try:
            assert other.run_id != first.run_id
        finally:
            other.close()
        identity = first.run_id
    finally:
        first.close()
    resumed = provision.FreshRunner(config, tmp_path / "first")
    try:
        assert resumed.run_id == identity
    finally:
        resumed.close()


@pytest.mark.parametrize('funded,accepted', [(100111, False), (100112, True)])
def test_funding_preserves_native_minimum_change(runner, funded, accepted):
    runner.config['amount_sats'] = 98500
    runner.journal.artifact('receive_address', ADDRESS)
    runner.journal.artifact('funding_claim_started', True)
    runner.core = SimpleNamespace(utxos=lambda _: [(FUNDING_TXID, 0, funded)], raw=lambda _: '00aa')
    if accepted:
        runner.stage_funding()
        assert runner.artifacts['funding_outpoint']['value_sats'] == funded
    else:
        with pytest.raises(Blocked) as caught:
            runner.stage_funding()
        assert caught.value.code == 'FUNDING_PENDING'
        assert 'funding_outpoint' not in runner.artifacts


def test_account_credential_binding_survives_new_desktop_registration(config, tmp_path):
    from harness.state import atomic_write
    config['phone_state_dir'] = str(tmp_path / 'phone-state')
    account_ids, desktop_ids, credential_paths = [], [], []
    for ordinal in range(2):
        candidate = provision.FreshRunner(config, tmp_path / f'fresh-{ordinal}')
        candidate.profile.mkdir(mode=0o700)
        (candidate.profile / 'testnet4').mkdir(mode=0o700)
        account = {'email': config['account_email'], 'device_id': f'device-{ordinal}'}
        atomic_write(candidate.profile / 'testnet4/connect.json', json.dumps({'accounts': [account], 'active_email': account['email']}).encode())
        candidate.provision = SimpleNamespace(binary='/synthetic/native', call=lambda *a, **k: dict(account, user_id=42))
        candidate.phone = SimpleNamespace(device_digest='same-phone', labels=lambda: {'Use a PIN'})
        try:
            candidate.stage_authentication()
            account_ids.append(candidate.artifacts['account_identity'])
            desktop_ids.append(candidate.artifacts['desktop_identity'])
            credential_paths.append(candidate.onboarding.store.locator)
        finally:
            candidate.close()
    assert account_ids[0] == account_ids[1]
    assert desktop_ids[0] != desktop_ids[1]
    assert credential_paths[0] == credential_paths[1]


@pytest.mark.parametrize('code,category', [('desktop_session_unavailable', Blocked),
    ('desktop_session_incomplete', Blocked), ('device_registration_uncertain', Blocked),
    ('connect_account_mismatch', Failed), ('transport_invalid', Failed)])
def test_native_bootstrap_distinguishes_prerequisites_from_assertions(tmp_path, monkeypatch, code, category):
    monkeypatch.setattr(provision.subprocess, 'run', lambda *a, **k: SimpleNamespace(
        returncode=2, stdout=json.dumps({'ok': False, 'code': code}).encode()))
    native = provision.Provision('/synthetic/native', tmp_path, tmp_path, 'run')
    with pytest.raises(category) as error:
        native.call('bootstrap_auth')
    assert type(error.value) is category
