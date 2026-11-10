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
    key = {"id": 12, "name": "QA " + runner.run_id, "network": "testnet4", "cubeId": None, "status": "active",
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


def test_account_credential_binding_survives_new_desktop_registration(config, tmp_path, monkeypatch):
    monkeypatch.setattr(provision.SecretStore, "load", lambda _: None)
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


def pending_phone(runner, purpose="key", *, validated=True, marker=True):
    runner.journal.artifact("account_identity", "synthetic-account")
    runner.phone = SimpleNamespace(device_digest="synthetic-phone")
    runner.onboarding = SimpleNamespace(identity="synthetic-account:synthetic-phone")
    runner.journal.artifact("cube_id", 17)
    runner.journal.artifact("cube_name", "synthetic-cube")
    runner.journal.artifact("phone_key_creation_started", True)
    runner.journal.artifact("phone_key_pin_entered", True)
    runner.journal.artifact("baseline_key_ids", ["1", "2"])
    if validated:
        runner.journal.outcome("phone_setup", "passed")
    if purpose == "sign":
        import uuid
        intent = {"txid": TXID, "inputs": [FUNDING_TXID + ":1"], "outputs": [{"address": ADDRESS, "sats": 10000}]}
        request = str(uuid.uuid5(uuid.UUID(runner.run_id), TXID))
        for key, value in {"intent": intent, "request_id": request, "session_id": "synthetic-session",
                           "approval_started": True, "target_key_id": "12", "vault_id": 19}.items():
            runner.journal.artifact(key, value)
        runner.request_id = request
        bound_purpose = "sign:" + request
    else:
        bound_purpose = "key:QA " + runner.run_id
    state = {"identity": runner.onboarding.identity, "verified": True, "pin": "000000"}
    if marker:
        state["pending_attempt"] = bound_purpose
    return state, bound_purpose


@pytest.mark.parametrize("purpose", ["key", "sign"])
def test_bound_pending_attempt_reuses_prior_validation_without_navigation(runner, purpose):
    state, expected = pending_phone(runner, purpose)
    prior = deepcopy(runner.journal.data["stages"]["phone_setup"])
    runner.defer_pending_phone_attempt(state)
    runner.stage_phone_setup()  # No phone/onboarding navigation methods supplied.
    assert runner.deferred_phone_purpose == expected
    receipt = runner.artifacts["phone_validation_reused"]
    assert receipt["purpose"] == expected
    assert receipt["validation"]["stage_outcome"] == prior
    assert receipt["validation"]["account"] == "synthetic-account"
    assert receipt["validation"]["device"] == "synthetic-phone"
    assert receipt["validation"]["network"] == "testnet4"
    assert state["pending_attempt"] == expected


@pytest.mark.parametrize("bad", ["key:another-run", "sign:another-request", "unexpected", "", 42])
def test_foreign_or_malformed_pending_intent_blocks_before_ui(runner, bad):
    state, _ = pending_phone(runner)
    state["pending_attempt"] = bad
    with pytest.raises(Blocked) as caught:
        runner.defer_pending_phone_attempt(state)
    assert caught.value.code in ("PHONE_PENDING_FOREIGN", "PHONE_PENDING_INTENT")
    assert runner.deferred_phone_purpose is None
    assert "phone_validation_reused" not in runner.artifacts


@pytest.mark.parametrize("purpose,missing", [("key", "baseline_key_ids"), ("key", "phone_key_creation_started"),
    ("sign", "session_id"), ("sign", "intent"), ("sign", "approval_started")])
def test_pending_attempt_requires_exact_run_checkpoints(runner, purpose, missing):
    state, _ = pending_phone(runner, purpose)
    runner.artifacts.pop(missing)
    with pytest.raises(Failed) as caught:
        runner.defer_pending_phone_attempt(state)
    assert caught.value.code == "PHONE_PENDING_INTENT"


@pytest.mark.parametrize("purpose", ["key", "sign"])
def test_verified_credential_alone_cannot_skip_first_run_account_network_checks(runner, purpose):
    state, _ = pending_phone(runner, purpose, validated=False)
    with pytest.raises(Blocked) as caught:
        runner.defer_pending_phone_attempt(state)
    assert caught.value.code == "PHONE_VALIDATION_REQUIRED"
    assert "phone_validation_reused" not in runner.artifacts


def test_prior_validation_cannot_move_to_another_device(runner):
    state, _ = pending_phone(runner)
    runner.journal.artifact("phone_validation", {"account": "synthetic-account", "device": "other-phone",
                                                "network": "testnet4", "source": "settings_ui"})
    with pytest.raises(Failed) as caught:
        runner.defer_pending_phone_attempt(state)
    assert caught.value.code == "PHONE_VALIDATION_CHANGED"


def test_pending_sign_skips_all_fixture_navigation_until_verified_collection(runner, monkeypatch):
    state, purpose = pending_phone(runner, "sign")
    runner.defer_pending_phone_attempt(state)
    runner.artifacts.update(target_fingerprint="aabbccdd", cube_uuid="synthetic-cube")
    runner.account = {"tokens": {"access_token": "synthetic-token"}}
    runner.keys = lambda: [phone_key(runner, cubeId=17)]
    monkeypatch.setattr(provision, "http", lambda *a, **k: b'{"data":[{"id":12,"xpubEnvelope":{"version":1}}]}')
    result = {"vault_id": 19, "descriptor_id": "descriptor", "descriptor_file": "/synthetic/descriptor",
              "receive_address": ADDRESS}
    runner.provision = SimpleNamespace(call=lambda *a, **k: result)
    runner.stage_phone_setup()
    runner.stage_vault_creation()
    runner.stage_phone()  # Phone intentionally has no navigation methods.
    runner.collect_once = lambda: False
    runner.stage_sign()  # approval_started prevents a second review/approval.
    with pytest.raises(Blocked) as caught:
        runner.stage_collect()
    assert caught.value.code == "SIGNATURE_DELIVERY"
    assert runner.deferred_phone_purpose == purpose
    actions = []
    runner.collect_once = lambda: True
    runner.onboarding.reconcile_pin = lambda p: actions.append(("reconciled", p))
    runner.stage_phone_setup = lambda: actions.append("checked-account-network")
    runner.phone.check_fixture = lambda *a: actions.append("checked-cube") or {"network": "testnet4", "cube_visible": True}
    runner.stage_collect()
    assert actions == [("reconciled", purpose), "checked-account-network", "checked-cube"]
    assert runner.deferred_phone_purpose is None
    assert runner.artifacts["phone_validation_reused"]["purpose"] == purpose


def test_pending_key_absence_keeps_prompt_and_match_releases_navigation(runner):
    state, purpose = pending_phone(runner)
    runner.phone.labels = lambda: {"Enter your PIN"}
    runner.defer_pending_phone_attempt(state)
    runner.keys = lambda: []
    with pytest.raises(Blocked) as caught:
        runner.stage_phone_key()
    assert caught.value.code == "KEY_REGISTRATION"
    assert runner.deferred_phone_purpose == purpose
    actions = []
    runner.keys = lambda: [phone_key(runner)]
    runner.onboarding.reconcile_pin = lambda p: actions.append(("reconciled", p))
    runner.stage_phone_setup = lambda: actions.append("checked-account-network")
    runner.stage_phone_key()
    assert actions == [("reconciled", purpose), "checked-account-network"]
    assert runner.deferred_phone_purpose is None
    assert runner.artifacts["target_key_id"] == "12"


def test_approval_before_pin_provider_still_preserves_the_open_prompt(runner):
    state, purpose = pending_phone(runner, "sign", marker=False)
    runner.defer_pending_phone_attempt(state)
    runner.stage_phone_setup()
    assert runner.deferred_phone_purpose == purpose
    assert "pending_attempt" not in state


def test_authentication_loads_pending_intent_before_even_reading_phone_labels(runner, tmp_path, monkeypatch):
    from harness.state import atomic_write, digest
    runner.config["phone_state_dir"] = str(tmp_path / "phone-state")
    runner.profile.mkdir(mode=0o700)
    (runner.profile / "testnet4").mkdir(mode=0o700)
    account = {"email": runner.config["account_email"], "device_id": "synthetic-desktop"}
    atomic_write(runner.profile / "testnet4/connect.json", json.dumps({"accounts": [account], "active_email": account["email"]}).encode())
    runner.provision = SimpleNamespace(binary="/synthetic/native", call=lambda *a, **k: dict(account, user_id=42))
    def no_ui():
        raise AssertionError("Pending authentication must not inspect or navigate the phone")
    runner.phone = SimpleNamespace(device_digest="synthetic-phone", labels=no_ui)
    identity = digest({"email": account["email"].casefold(), "user_id": 42}) + ":synthetic-phone"
    state = {"identity": identity, "verified": True, "pending_attempt": "key:QA " + runner.run_id}
    monkeypatch.setattr(provision.SecretStore, "load", lambda _: deepcopy(state))
    for key, value in {"cube_id": 17, "phone_key_creation_started": True, "phone_key_pin_entered": True,
                       "baseline_key_ids": []}.items():
        runner.journal.artifact(key, value)
    runner.journal.outcome("phone_setup", "passed")
    runner.stage_authentication()
    assert runner.deferred_phone_purpose == state["pending_attempt"]


def test_first_run_phone_setup_still_checks_live_account_and_network(runner):
    calls = []
    runner.journal.artifact("account_identity", "synthetic-account")
    runner.account = {"email": "qa@example.invalid"}
    runner.onboarding = SimpleNamespace(setup=lambda: calls.append("setup") or "000000")
    runner.phone = SimpleNamespace(device_digest="synthetic-phone", restore_navigation=lambda: calls.append("navigation"),
                                  click=lambda label: calls.append(label), labels=lambda: {"qa@example.invalid", "Testnet4"})
    runner.stage_phone_setup()
    assert calls == ["setup", "navigation", "Settings", "Home"]
    assert runner.artifacts["phone_validation"] == {"account": "synthetic-account", "device": "synthetic-phone",
                                                   "network": "testnet4", "source": "settings_ui"}


def attachment_state(runner):
    runner.artifacts.update(cube_id=17, cube_uuid='synthetic-cube', cube_name='QA synthetic cube',
                            target_key_id='12', target_fingerprint='aabbccdd')
    runner.account = {'tokens': {'access_token': 'synthetic-token'}}


def test_attachment_runs_phone_reseal_once_and_reconciles_after_lost_reply(runner, monkeypatch):
    attachment_state(runner)
    key = phone_key(runner)
    runner.keys = lambda: [key]
    calls = []

    def attach(name, cube, journal):
        assert name == 'QA ' + runner.run_id and cube == 'QA synthetic cube'
        journal.artifact('phone_key_attachment_started', True)
        calls.append(True)
        key['cubeId'] = 17
        raise Blocked('SYNTHETIC_LOST_REPLY', 'Synthetic lost UI response')

    runner.onboarding = SimpleNamespace(attach_key=attach)
    with pytest.raises(Blocked):
        runner.ensure_phone_key_attachment()
    urls = []
    def get(url, **kwargs):
        urls.append(url)
        assert 'body' not in kwargs  # Only Keychain mutates attachment/envelope.
        return b'{"data":[{"id":12,"xpubEnvelope":{"version":1}}]}'
    monkeypatch.setattr(provision, 'http', get)
    runner.ensure_phone_key_attachment()
    assert calls == [True]
    assert runner.artifacts['phone_key_attached'] is True
    assert urls == [provision.REST + '/api/v1/connect/cubes/synthetic-cube/keys']


@pytest.mark.parametrize('change', [{'cubeId': 99}, {'status': 'revoked'}, {'network': 'bitcoin'},
                                  {'fingerprint': '11223344'}, {'name': 'unrelated'}])
def test_attachment_refuses_changed_key_before_any_phone_action(runner, change):
    attachment_state(runner)
    runner.keys = lambda: [phone_key(runner, **change)]
    runner.onboarding = SimpleNamespace()  # Any UI action would fail the test.
    with pytest.raises(Failed) as error:
        runner.ensure_phone_key_attachment()
    assert error.value.code == 'KEY_ATTACHMENT_IDENTITY'


@pytest.mark.parametrize('bound', [False, True])
def test_uncertain_attachment_never_replays_or_accepts_missing_envelope(runner, monkeypatch, bound):
    attachment_state(runner)
    runner.artifacts['phone_key_attachment_started'] = True
    runner.keys = lambda: [phone_key(runner, cubeId=17 if bound else None)]
    runner.onboarding = SimpleNamespace()
    monkeypatch.setattr(provision, 'http', lambda *a, **k: b'{"data":[{"id":12}]}')
    with pytest.raises(Blocked) as error:
        runner.ensure_phone_key_attachment()
    assert error.value.code == 'KEY_ATTACHMENT_PENDING'
    assert not runner.artifacts.get('phone_key_attached')


def test_pending_sign_cannot_navigate_to_repair_missing_attachment(runner):
    attachment_state(runner)
    runner.deferred_phone_purpose = 'sign:synthetic-request'
    runner.keys = lambda: [phone_key(runner)]
    runner.onboarding = SimpleNamespace()
    with pytest.raises(Blocked) as error:
        runner.ensure_phone_key_attachment()
    assert error.value.code == 'KEY_ATTACHMENT_PENDING'


def test_sealed_key_resume_preserves_original_public_export(runner):
    runner.artifacts.update(cube_id=17, phone_key_creation_started=True, baseline_key_ids=[])
    runner.onboarding = SimpleNamespace(reconcile_pin=lambda _: None)
    key = phone_key(runner)
    runner.keys = lambda: [key]
    runner.stage_phone_key()
    path = runner.journal.directory / 'phone-key.json'
    original = private_read(path)
    key.update(xpub='', cubeId=17)
    runner.stage_phone_key()
    assert private_read(path) == original
    key['xpub'] = 'synthetic-different-key'
    with pytest.raises(Failed) as error:
        runner.stage_phone_key()
    assert error.value.code == 'KEY_CHANGED'


def signing_recovery(runner, *, modern=False):
    state, purpose = pending_phone(runner, "sign", marker=False)
    runner.intent = deepcopy(runner.artifacts["intent"])
    runner.phone.build = 38
    runner.artifacts.update(desktop_identity="synthetic-desktop", psbt_sha256="c" * 64,
                            descriptor_id="deadbeef", target_fingerprint="aabbccdd", cube_uuid="synthetic-cube", fee_sats=612)
    runner.onboarding.store = SimpleNamespace(load=lambda: deepcopy(state))
    runner.journal.artifact("phone", {"build": 37, "network": "testnet4", "cube_visible": True})
    runner.journal.data["stages"].update(phone={"status": "passed", "code": "", "at": 10},
                                        sign={"status": "blocked", "code": "PHONE_AUTH", "at": 11})
    if modern:
        runner.phone.build = 39
        runner.journal.artifact("approval_context", {"binding": runner.approval_binding(),
            "request_id": runner.request_id, "session_id": runner.artifacts["session_id"],
            "intent_sha256": provision.digest(runner.intent), "fee_sats": 612,
            "device": runner.phone.device_digest, "build": 38,
            "process": {"pid": 123, "bundle_id": "io.coincube.keychainApp"}})
    calls = []
    runner.native_call = lambda *a, **k: {"ready": False, "status": 3, "submitted_signature_count": 0}
    def restart(journal, receipt):
        calls.append("restart")
        receipt["phase"] = "ready_for_review"
        journal.artifact("approval_upgrade_recovery", receipt)
    def approve(*args, **kwargs):
        calls.append("approve")
        assert kwargs["reapproval"] is True
        assert kwargs["session_id"] == runner.artifacts["session_id"]
        assert kwargs["before_approve"]() is True
        assert kwargs["before_pin"]() is True
        receipt = runner.artifacts["approval_upgrade_recovery"]
        receipt["phase"] = "approval_dispatched"
        runner.journal.artifact("approval_upgrade_recovery", receipt)
    runner.phone.restart_for_upgrade = restart
    runner.phone.approve = approve
    runner.stage_phone_setup = lambda: calls.append("account-network")
    runner.stage_phone = lambda: calls.append("cube-review")
    return state, calls


@pytest.mark.parametrize("modern", [False, True])
def test_upgrade_archives_original_evidence_before_stage_overwrite_and_rechecks(runner, modern):
    state, calls = signing_recovery(runner, modern=modern)
    runner.defer_pending_phone_attempt(state)
    original = deepcopy(runner.artifacts["approval_original_attempt"])
    assert original["origin"] == ("approval-context" if modern else "legacy-stage-evidence")
    if not modern:
        assert "process" not in original and "context" not in original
        assert original["sign_stage"]["code"] == "PHONE_AUTH"
    runner.journal.outcome("phone", "passed")
    runner.journal.outcome("sign", "passed")
    runner.defer_pending_phone_attempt(state)
    assert runner.artifacts["approval_original_attempt"] == original
    runner.stage_sign()
    assert calls == ["restart", "account-network", "cube-review", "approve"]
    assert runner.artifacts["approval_started"] is True
    assert runner.artifacts["approval_upgrade_recovery"]["original_attempt"] == original
    calls.clear()
    if modern:
        # Phone now owns current-context validation, including the exact PID.
        runner.phone.resume_first_pin = lambda *a, **k: calls.append("resume-only")
    runner.stage_sign()
    assert calls == (["resume-only"] if modern else [])


@pytest.mark.parametrize("field", ["pending_attempt", "pin_resume_probe", "pin_dispatch_started"])
def test_signing_pin_evidence_never_allows_upgrade_generation(runner, field):
    state, calls = signing_recovery(runner)
    if field == "pending_attempt":
        state[field] = "sign:" + runner.request_id
    else:
        runner.journal.artifact(field, {"status": "started"})
    runner.defer_pending_phone_attempt(state)
    assert "approval_original_attempt" not in runner.artifacts
    runner.stage_sign()
    assert calls == []
    assert "approval_upgrade_recovery" not in runner.artifacts


@pytest.mark.parametrize("change", ["build-missing", "build36", "build38", "build-bool", "phone-not-passed", "sign-not-blocked", "not-auth", "reversed", "missing-time", "missing-hash"])
def test_legacy_adoption_requires_conclusive_ordered_original_evidence(runner, change):
    state, calls = signing_recovery(runner)
    stages = runner.journal.data["stages"]
    if change == "build-missing": runner.artifacts["phone"].pop("build")
    if change == "build36": runner.artifacts["phone"]["build"] = 36
    if change == "build38": runner.artifacts["phone"]["build"] = 38
    if change == "build-bool": runner.artifacts["phone"]["build"] = True
    if change == "phone-not-passed": stages["phone"]["status"] = "blocked"
    if change == "sign-not-blocked": stages["sign"]["status"] = "passed"
    if change == "not-auth": stages["sign"]["code"] = "SIGNING_TIMEOUT"
    if change == "reversed": stages["phone"]["at"] = 12
    if change == "missing-time": stages["sign"].pop("at")
    if change == "missing-hash": runner.artifacts.pop("psbt_sha256")
    runner.defer_pending_phone_attempt(state)
    assert "approval_original_attempt" not in runner.artifacts
    runner.stage_sign()
    assert calls == []


@pytest.mark.parametrize("build", [36, 37, True])
def test_upgrade_requires_strictly_newer_integer_build(runner, build):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    runner.phone.build = build
    runner.stage_sign()
    assert calls == []
    assert "approval_upgrade_recovery" not in runner.artifacts


@pytest.mark.parametrize("field", ["account_identity", "desktop_identity", "request_id", "session_id", "intent", "psbt_sha256",
                                   "descriptor_id", "target_key_id", "target_fingerprint", "vault_id", "fee_sats"])
def test_original_approval_binding_cannot_change(runner, field):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    runner.artifacts[field] = "d" * 64 if field == "psbt_sha256" else "changed"
    with pytest.raises(Failed) as caught:
        runner.upgrade_approval()
    assert caught.value.code == "APPROVAL_CHANGED"
    assert calls == []


@pytest.mark.parametrize("status,count", [(1, 0), (2, 0), (4, 0), (5, 0), (6, 0), (3, 1), (3, None), (3, False), (True, 0)])
def test_upgrade_refuses_anything_except_exact_viewed_and_zero_submitted(runner, status, count):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    runner.native_call = lambda *a, **k: {"ready": False, "status": status, "submitted_signature_count": count}
    with pytest.raises(Blocked) as caught:
        runner.stage_sign()
    assert caught.value.code == "APPROVAL_REMOTE_STATE"
    assert calls == []


def test_signature_arriving_after_process_stop_prevents_another_review(runner):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    responses = iter([{"ready": False, "status": 3, "submitted_signature_count": 0},
                      {"ready": False, "status": 3, "submitted_signature_count": 0},
                      {"ready": True, "status": 6, "submitted_signature_count": 1, "signature_count_min": 2}])
    runner.native_call = lambda *a, **k: next(responses)
    runner.stage_sign()
    assert calls == ["restart"]
    assert runner.artifacts["signature_count_min"] == 2
    assert runner.deferred_phone_purpose == "sign:" + runner.request_id


def test_partial_process_boundary_resumes_same_receipt_without_second_generation(runner):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    def interrupted(journal, receipt):
        receipt["phase"] = "process_stopped"
        journal.artifact("approval_upgrade_recovery", receipt)
        raise Blocked("SYNTHETIC_INTERRUPTION", "Synthetic interruption")
    normal = runner.phone.restart_for_upgrade
    runner.phone.restart_for_upgrade = interrupted
    with pytest.raises(Blocked): runner.stage_sign()
    original = deepcopy(runner.artifacts["approval_upgrade_recovery"]["original_attempt"])
    runner.phone.restart_for_upgrade = normal
    runner.stage_sign()
    assert runner.artifacts["approval_upgrade_recovery"]["original_attempt"] == original
    assert calls == ["restart", "account-network", "cube-review", "approve"]


def test_existing_pin_continuation_never_navigates_or_approves_again(runner):
    state, calls = signing_recovery(runner, modern=True)
    runner.phone.build = 38
    runner.defer_pending_phone_attempt(state)
    def resume(*args, **kwargs):
        calls.append("resume-first-pin")
        assert args[:2] == (runner.request_id, runner.artifacts["session_id"])
        assert kwargs["before_pin"]() is True
    runner.phone.resume_first_pin = resume
    runner.stage_sign()
    assert calls == ["resume-first-pin"]
    assert runner.artifacts["approval_started"] is True


def test_first_approval_keeps_review_and_remote_callbacks_mandatory(runner):
    state, calls = signing_recovery(runner)
    runner.artifacts.pop("approval_started")
    def approve(*args, **kwargs):
        assert kwargs["reapproval"] is False
        assert kwargs["session_id"] == runner.artifacts["session_id"]
        assert kwargs["before_approve"]() and kwargs["before_pin"]()
        calls.append("first-review")
    runner.phone.approve = approve
    runner.stage_sign()
    assert calls == ["first-review"]
    assert runner.artifacts["approval_binding"] == runner.approval_binding()


@pytest.mark.parametrize("field,value", [("binding", {}), ("request_id", "other"), ("session_id", "other"),
    ("intent_sha256", "bad"), ("fee_sats", 613), ("device", "other"), ("build", 37),
    ("process", {"pid": 0, "bundle_id": "io.coincube.keychainApp"}),
    ("process", {"pid": 123, "bundle_id": "other.app"})])
def test_modern_context_must_authenticate_every_recorded_identity(runner, field, value):
    state, calls = signing_recovery(runner, modern=True)
    runner.artifacts["approval_context"][field] = value
    with pytest.raises(Failed) as caught:
        runner.defer_pending_phone_attempt(state)
    assert caught.value.code == "APPROVAL_CHANGED"
    assert calls == []
    assert "approval_original_attempt" not in runner.artifacts


@pytest.mark.parametrize("field,value", [("identity", "foreign"), ("verified", False),
                                        ("pending_attempt", "sign:uncertain")])
def test_credential_changes_after_archive_never_allow_recovery(runner, field, value):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    state[field] = value
    runner.stage_sign()
    assert calls == []
    assert "approval_upgrade_recovery" not in runner.artifacts


@pytest.mark.parametrize("field,value", [("phase", "unknown"), ("current_build", 39), ("original_build", 36), ("binding", {}), ("original_attempt", {})])
def test_saved_recovery_is_not_replaceable(runner, field, value):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    original = runner.artifacts["approval_original_attempt"]
    receipt = {"original_attempt": deepcopy(original), "binding": runner.approval_binding(),
               "original_build": 37, "current_build": 38, "phase": "planned"}
    receipt[field] = value
    runner.journal.artifact("approval_upgrade_recovery", receipt)
    with pytest.raises(Failed): runner.stage_sign()
    assert calls == []
    assert runner.artifacts["approval_upgrade_recovery"] == receipt


def test_account_check_failure_after_restart_cannot_dispatch_approval(runner):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    def mismatch():
        raise Failed("PHONE_ACCOUNT", "Synthetic mismatch")
    runner.stage_phone_setup = mismatch
    with pytest.raises(Failed) as caught: runner.stage_sign()
    assert caught.value.code == "PHONE_ACCOUNT"
    assert calls == ["restart"]
    assert runner.artifacts["approval_started"] is True
    assert runner.artifacts["approval_upgrade_recovery"]["phase"] == "ready_for_review"


def test_status_callback_rejects_binding_change_before_native_call(runner):
    state, calls = signing_recovery(runner)
    runner.defer_pending_phone_attempt(state)
    runner.artifacts["cube_uuid"] = "other-cube"
    runner.native_call = lambda *a, **k: pytest.fail("Binding mismatch must precede remote action")
    with pytest.raises(Failed) as caught: runner.require_viewed_unsubmitted()
    assert caught.value.code == "APPROVAL_CHANGED"
    assert calls == []


def test_pending_key_capacity_refusal_is_read_only_and_preserves_attempt(runner):
    from harness.phone import Phone, BUNDLE
    state, purpose = pending_phone(runner)
    runner.defer_pending_phone_attempt(state)
    driver = SimpleNamespace(execute_script=lambda _: {"bundleId": BUNDLE},
        page_source='<root><node visible="true" label="Key limit reached"/></root>')
    runner.phone = Phone(driver, 38)
    runner.phone.device_digest = "synthetic-phone"
    runner.keys = lambda: []
    before = deepcopy(runner.journal.data)
    with pytest.raises(Blocked) as error:
        runner.stage_phone_key()
    assert error.value.code == "SIGNER_LIMIT"
    assert runner.journal.data == before
    assert state["pending_attempt"] == purpose
    assert runner.deferred_phone_purpose == purpose
