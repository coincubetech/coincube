from types import SimpleNamespace

import pytest

from harness.phone import Phone
from harness.state import Blocked, Failed, Journal


INTENT = {"outputs": [{"address": "tb1fixture-recipient", "sats": 10000},
                      {"address": "tb1fixture-change", "sats": 989388}]}
REVIEW = {"Approve transaction", "testnet4", "612 sats", "10000 sats", "989388 sats",
          "tb1fixture-recipient", "tb1fixture-change"}


def scripted_phone(views):
    phone = Phone(SimpleNamespace(execute_script=lambda *args: None), 35)
    screens = iter(views)
    phone.labels = lambda: next(screens)
    clicks = []
    phone.click = clicks.append
    phone.refresh = lambda: None
    return phone, clicks


@pytest.mark.parametrize("restored,expected", [
    ([{"Signature Submitted", "Done"}, {"Signing Requests", "Back"}], ["Done", "Back"]),
    ([{"Transaction approved", "Done"}], ["Done"]),
    ([REVIEW | {"Back"}], ["Back"]),
    ([{"Enter PIN", "Back", "1", "2"}, REVIEW | {"Back"}], ["Back", "Back"]),
])
def test_fixture_check_resumes_without_approval_or_pin(restored, expected, monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone(restored + [
        {"Home", "Cubes", "Settings"}, {"Testnet4"}, {"Disposable QA Cube"},
    ])
    assert phone.check_fixture("Disposable QA Cube")["cube_visible"] is True
    assert clicks == expected + ["Settings", "Cubes", "Home"]


def test_unknown_restored_screen_never_guesses_navigation(monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([{"Approve", "Try Again", "Done", "Enter PIN"}] * 8)
    with pytest.raises(Blocked) as error:
        phone.restore_navigation()
    assert error.value.code == "PHONE_NAVIGATION"
    assert clicks == []


def test_approval_never_selects_an_unrelated_request(tmp_path):
    phone, clicks = scripted_phone([{"some other request"}])
    journal = Journal(tmp_path / "run", {})
    try:
        with pytest.raises(Blocked) as error:
            phone.approve("fixture", INTENT, 612, None, journal, timeout=0)
        assert error.value.code == "REQUEST_DELIVERY"
        assert "Approve" not in clicks
        assert not journal.data["artifacts"].get("approval_started")
    finally:
        journal.close()


def test_uncertain_approval_never_retries_pin(tmp_path):
    phone, clicks = scripted_phone([{"Testnet4 QA fixture"}, REVIEW])
    journal = Journal(tmp_path / "run", {})
    journal.artifact("approval_started", True)
    try:
        with pytest.raises(Blocked) as error:
            phone.approve("fixture", INTENT, 612, None, journal, timeout=2)
        assert error.value.code == "APPROVAL_UNCERTAIN"
        assert "Approve" not in clicks
    finally:
        journal.close()


def test_wrong_phone_summary_never_approves(tmp_path, monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    wrong = REVIEW - {"tb1fixture-recipient"}
    phone, clicks = scripted_phone([{"Testnet4 QA fixture"}] + [wrong] * 8)
    journal = Journal(tmp_path / "run", {})
    try:
        with pytest.raises(Failed) as error:
            phone.approve("fixture", INTENT, 612, None, journal, timeout=2)
        assert error.value.code == "PHONE_REVIEW"
        assert "Approve" not in clicks
    finally:
        journal.close()


def test_real_banner_and_pin_flow_use_exact_labels(tmp_path, monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([{"1 signing request waiting"}, {"Testnet4 QA fixture"}, REVIEW,
                                   {"Enter PIN"}, {"Signature Submitted"}])
    pin = tmp_path / "pin"
    pin.write_text("987654")
    pin.chmod(0o600)
    journal = Journal(tmp_path / "run", {})
    try:
        phone.approve("fixture", INTENT, 612, str(pin), journal, timeout=5)
        assert clicks == ["Home", "1 signing request waiting", "Testnet4 QA fixture", "Approve", *"987654"]
        assert journal.data["artifacts"]["approval_started"] is True
        assert "987654" not in journal.path.read_text()
    finally:
        journal.close()
