import json
from pathlib import Path
from types import SimpleNamespace
import uuid

import pytest

from harness.clients import GENESIS, Core, assert_transaction, sats
from harness.runner import Runner, load_config
from harness.state import Blocked, Failed, Journal, atomic_write, private_read

TXID = "a" * 64
OUTPOINT = "b" * 64 + ":1"


def private(path, text):
    path.write_text(text)
    path.chmod(0o600)
    return str(path)


@pytest.fixture
def configured(tmp_path):
    config = {
        "version": 1, "run_id": "synthetic-001",
        "connect_rest": "https://api.coincube.io", "connect_grpc": "https://grpc.coincube.io:443",
        "native_binary": "/synthetic/native-helper",
        "fixture": {"profile_dir": "/synthetic/profile", "psbt_file": private(tmp_path / "input.psbt", "synthetic-psbt"),
                    "descriptor_file": private(tmp_path / "descriptor", "synthetic-descriptor"), "expected_txid": TXID,
                    "cube_id": 1, "vault_id": "2", "target_key_id": "3", "target_fingerprint": "aabbccdd"},
        "intent": {"txid": TXID, "inputs": [OUTPOINT], "outputs": [{"address": "tb1recipient", "sats": 10000},
                    {"address": "tb1change", "sats": 989388}], "maximum_fee_sats": 1000},
        "core": {"url": "http://127.0.0.1:48332", "cookie_file": "/synthetic/cookie"},
        "ios": {"minimum_build": 35, "cube_label": "Fixture"}, "cube_creation": True,
    }
    return config, tmp_path


@pytest.fixture
def runner(configured):
    config, path = configured
    result = Runner(config, path / "run")
    yield result
    result.journal.close()


def tx():
    return {"txid": TXID, "vin": [{"txid": "b" * 64, "vout": 1}],
            "vout": [{"value": "0.00010000", "scriptPubKey": {"address": "tb1recipient"}},
                     {"value": "0.00989388", "scriptPubKey": {"address": "tb1change"}}]}


def test_private_checkpoint_is_atomic_and_locked(tmp_path):
    one = Journal(tmp_path / "run", {"intent": "synthetic"})
    one.artifact("session_id", "synthetic-session")
    assert (one.path.stat().st_mode & 0o777) == 0o600
    with pytest.raises(Blocked, match="Another process"):
        Journal(tmp_path / "run", {"intent": "synthetic"})
    one.close()
    two = Journal(tmp_path / "run", {"intent": "synthetic"})
    assert two.data["artifacts"]["session_id"] == "synthetic-session"
    two.close()


def test_changed_fixture_cannot_reuse_run(configured):
    config, path = configured
    one = Runner(config, path / "run")
    one.journal.close()
    Path(config["fixture"]["psbt_file"]).write_text("different bytes")
    with pytest.raises(Failed, match="fixture changed"):
        Runner(config, path / "run")


def test_world_readable_secret_and_symlink_rejected(tmp_path):
    path = tmp_path / "secret"
    path.write_text("fixture-secret")
    path.chmod(0o644)
    with pytest.raises(Blocked):
        private_read(path)
    path.chmod(0o600)
    link = tmp_path / "link"
    link.symlink_to(path)
    with pytest.raises(Blocked):
        private_read(link)


@pytest.mark.parametrize("field,value", [("connect_rest", "https://dev-api.coincube.io"),
                                        ("connect_grpc", "http://grpc.coincube.io:443"),
                                        ("version", 2)])
def test_wrong_environment_rejected(configured, field, value):
    config, path = configured
    config[field] = value
    file = private(path / "config.json", json.dumps(config))
    with pytest.raises(Failed):
        load_config(file)


@pytest.mark.parametrize("url", ["http://example.com", "http://localhost:8332", "https://127.0.0.1", "http://user:secret@127.0.0.1", "http://127.0.0.1?token=secret"])
def test_core_rpc_never_sends_cookie_off_loopback(configured, url):
    config, path = configured
    config["core"]["url"] = url
    with pytest.raises(Failed, match="loopback"):
        load_config(private(path / "config.json", json.dumps(config)))


def test_valid_config_and_satoshi_precision(configured):
    config, path = configured
    assert load_config(private(path / "config.json", json.dumps(config))) == config
    assert sats("0.00000612") == 612
    with pytest.raises(Failed):
        sats("0.000000001")


def test_chain_checks_genesis_not_only_testnet_prefix():
    core = Core({"url": "http://127.0.0.1", "cookie_file": "/synthetic"})
    core.call = lambda method, *args: {"chain": "testnet4", "initialblockdownload": False} if method == "getblockchaininfo" else "wrong-genesis"
    with pytest.raises(Failed, match="Testnet4"):
        core.chain()
    core.call = lambda method, *args: {"chain": "testnet4", "initialblockdownload": False} if method == "getblockchaininfo" else GENESIS
    core.chain()


@pytest.mark.parametrize("mutation", ["txid", "input", "recipient", "amount", "extra_output"])
def test_exact_transaction_binding(runner, mutation):
    actual = tx()
    assert_transaction(actual, runner.intent)
    if mutation == "txid": actual["txid"] = "c" * 64
    if mutation == "input": actual["vin"][0]["vout"] = 2
    if mutation == "recipient": actual["vout"][0]["scriptPubKey"]["address"] = "tb1other"
    if mutation == "amount": actual["vout"][0]["value"] = "0.001"
    if mutation == "extra_output": actual["vout"].append(actual["vout"][0])
    with pytest.raises(Failed):
        assert_transaction(actual, runner.intent)


def test_lost_request_response_keeps_same_id_across_restart(runner, monkeypatch):
    runner.core.chain = lambda: None
    calls = []
    def lost(action, **fields):
        calls.append(fields["request_id"])
        raise Blocked("TIMEOUT", "synthetic timeout")
    runner.native.call = lost
    with pytest.raises(Blocked): runner.stage_request()
    first_id = runner.request_id
    runner.journal.close()
    resumed = Runner(runner.config, runner.journal.directory)
    resumed.core.chain = lambda: None
    def found(action, **fields):
        calls.append(fields["request_id"])
        return {"session_id": "same-session"}
    resumed.native.call = found
    resumed.stage_request()
    assert calls == [first_id, first_id]
    assert resumed.artifacts["session_id"] == "same-session"
    resumed.journal.close()


def test_server_cannot_replace_existing_session(runner):
    runner.journal.artifact("session_id", "first")
    runner.core.chain = lambda: None
    runner.native.call = lambda *args, **kwargs: {"session_id": "second"}
    with pytest.raises(Failed, match="different session"):
        runner.stage_request()


def test_collect_before_phone_avoids_repeat_approval(runner, monkeypatch):
    runner.collect_once = lambda: True
    monkeypatch.setattr("harness.runner.attach", lambda *args: pytest.fail("must not reopen phone approval"))
    runner.stage_sign()


def test_approval_resume_keeps_polling_delayed_signature(runner, monkeypatch):
    runner.journal.artifact("approval_started", True)
    responses = iter([False, False, True])
    runner.collect_once = lambda: next(responses)
    monkeypatch.setattr("harness.runner.attach", lambda *args: pytest.fail("must not retry approval"))
    monkeypatch.setattr("harness.runner.time.sleep", lambda _: None)
    runner.stage_sign()
    runner.stage_collect()


def test_stage_dependency_failure_is_not_success(runner):
    with pytest.raises(Failed, match="order"):
        runner.run("broadcast")
    runner.stage_chain = lambda: (_ for _ in ()).throw(Blocked("SYNC", "fixture node unavailable"))
    with pytest.raises(Blocked): runner.run("chain")
    with pytest.raises(Blocked, match="earlier stage"): runner.run("cube_creation")
    assert runner.journal.data["stages"]["chain"]["status"] == "blocked"


def prepare_broadcast(runner, monkeypatch, *, allow=True, lose_response=False):
    runner.journal.artifact("fee_sats", 612)
    private(runner.journal.directory / "signed.psbt", "signed-fixture")
    runner.core.chain = lambda: None
    runner.core.known = lambda _: False
    monkeypatch.setattr("harness.runner.public_transaction", lambda _: None)
    calls = []
    def rpc(method, *params):
        calls.append(method)
        if method == "finalizepsbt": return {"complete": True, "hex": "synthetic-signed-hex"}
        if method == "decoderawtransaction": return tx()
        if method == "testmempoolaccept": return [{"txid": TXID, "allowed": allow, "fees": {"base": "0.00000612"}}]
        if method == "sendrawtransaction":
            assert runner.artifacts["broadcast_hex_sha256"]
            if lose_response: raise Blocked("TIMEOUT", "synthetic response lost")
            return TXID
        raise AssertionError(method)
    runner.core.call = rpc
    return calls


def test_broadcast_requires_mempool_acceptance(runner, monkeypatch):
    calls = prepare_broadcast(runner, monkeypatch, allow=False)
    with pytest.raises(Blocked, match="preflight"): runner.stage_broadcast()
    assert "sendrawtransaction" not in calls


def test_lost_broadcast_reconciles_without_sending_again(runner, monkeypatch):
    calls = prepare_broadcast(runner, monkeypatch, lose_response=True)
    with pytest.raises(Blocked): runner.stage_broadcast()
    assert calls.count("sendrawtransaction") == 1
    runner.core.known = lambda _: True
    runner.stage_broadcast()
    assert calls.count("sendrawtransaction") == 1
    assert runner.artifacts["broadcast_txid"] == TXID


def test_changed_signed_bytes_cannot_be_rebroadcast(runner, monkeypatch):
    calls = prepare_broadcast(runner, monkeypatch)
    runner.journal.artifact("broadcast_hex_sha256", "different-hash")
    with pytest.raises(Failed, match="bytes changed"): runner.stage_broadcast()
    assert "sendrawtransaction" not in calls


def test_broadcast_records_exact_transaction(runner, monkeypatch):
    prepare_broadcast(runner, monkeypatch)
    runner.stage_broadcast()
    assert runner.artifacts["broadcast_txid"] == TXID


def test_selected_live_missing_prerequisite_exits_two_and_reports_blocked(configured):
    import os
    import subprocess
    import sys
    config, path = configured
    file = private(path / "config.json", json.dumps(config))
    suite = Path(__file__).resolve().parents[1]
    env = dict(os.environ, PYTEST_DISABLE_PLUGIN_AUTOLOAD="1")
    result = subprocess.run([sys.executable, "-m", "pytest", str(suite / "test_acceptance.py"),
                             "--qa-config", file, "--qa-run-dir", str(path / "live-run"),
                             "--qa-preflight", "--tb=short", "-q"], capture_output=True, env=env, timeout=15)
    assert result.returncode == 2
    assert b"TESTNET4 BLOCKED" in result.stdout
    assert b"synthetic-psbt" not in result.stdout + result.stderr
    checkpoint = json.loads((path / "live-run/checkpoint.json").read_text())
    assert checkpoint["stages"]["chain"]["code"] == "PRIVATE_FILE_UNAVAILABLE"
