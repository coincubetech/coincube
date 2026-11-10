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
    ([{"Signature Submitted", "Done"}], ["Done"]),
    ([{"Transaction approved", "Done"}], ["Done"]),
    ([REVIEW | {"Back"}], ["Back"]),
    ([{"Enter PIN", "Back", "1", "2"}, REVIEW | {"Back"}], ["Back", "Back"]),
    ([{"Create new signer key", "Create Key", "Cancel", "Home", "Cubes", "Settings"}], ["Cancel"]),
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
    phone, clicks = scripted_phone([{"Approve", "Try Again", "Done", "Enter PIN"}] * 16)
    with pytest.raises(Blocked) as error:
        phone.restore_navigation()
    assert error.value.code == "PHONE_NAVIGATION"
    assert clicks == []


def test_result_then_unlabeled_pending_page_restarts_without_reset(monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([
        {"Signature Submitted", "Done"}, {"Pending Requests"}, set(),
        {"Home", "Cubes", "Settings"}, {"Testnet4"}, {"Disposable QA Cube"},
    ])
    lifecycle = []
    phone.driver.terminate_app = lambda bundle: lifecycle.append(("terminate", bundle))
    phone.driver.activate_app = lambda bundle: lifecycle.append(("activate", bundle))
    phone.check_fixture("Disposable QA Cube")
    assert clicks == ["Done", "Settings", "Cubes", "Home"]
    assert lifecycle == [("terminate", "io.coincube.keychainApp"), ("activate", "io.coincube.keychainApp")]


def test_pending_page_restart_is_bounded(monkeypatch):
    monkeypatch.setattr("harness.phone.time.sleep", lambda _: None)
    phone, clicks = scripted_phone([{"Pending Requests"}] * 16)
    lifecycle = []
    phone.driver.terminate_app = lambda _: lifecycle.append("terminate")
    phone.driver.activate_app = lambda _: lifecycle.append("activate")
    with pytest.raises(Blocked) as error:
        phone.restore_navigation()
    assert error.value.code == "PHONE_NAVIGATION"
    assert lifecycle == ["terminate", "activate"]
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


def test_key_creation_success_returns_home_without_retrying_creation(monkeypatch):
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: None)
    phone, clicks = scripted_phone([{'Signer key created', 'Back to home'}, {'Home', 'Cubes', 'Settings'}])
    phone.restore_navigation()
    assert clicks == ['Back to home']


@pytest.mark.parametrize('survives', [True, False])
def test_pin_resume_probe_records_only_observed_lifecycle_survival(tmp_path, monkeypatch, survives):
    ticks = iter([0, 0, 4])
    monkeypatch.setattr('harness.phone.time.monotonic', lambda: next(ticks))
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: None)
    phone, clicks = scripted_phone([{'Enter PIN'} if survives else {'Approve transaction'}] * 2)
    lifecycle = []
    phone.driver.execute_script = lambda *args: lifecycle.append(args)
    phone.driver.activate_app = lambda bundle: lifecycle.append(('activate', bundle))
    journal = Journal(tmp_path / 'probe', {})
    try:
        if survives:
            phone.verify_pin_resume(journal)
            result = journal.data['artifacts']['pin_resume_probe']
            assert result['status'] == 'passed'
            assert result['home_refresh_proven'] is False
        else:
            with pytest.raises(Failed) as error:
                phone.verify_pin_resume(journal)
            assert error.value.code == 'PIN_CHALLENGE_DISMISSED'
        assert clicks == []
        assert lifecycle[0] == ('mobile: backgroundApp', {'seconds': 1})
        assert lifecycle[1] == ('activate', 'io.coincube.keychainApp')
    finally:
        journal.close()


def test_merged_setup_card_matches_only_complete_title_line():
    from appium.webdriver.common.appiumby import AppiumBy
    clicked, predicates = [], []
    def find(by, predicate):
        assert by == AppiumBy.IOS_PREDICATE
        predicates.append(predicate)
        return [SimpleNamespace(click=lambda: clicked.append(True))]
    phone = Phone(SimpleNamespace(find_elements=find), 36)
    phone.click_card('Use a PIN')
    assert clicked == [True]
    assert 'BEGINSWITH "Use a PIN\\n"' in predicates[0]
    assert 'CONTAINS' not in predicates[0]
    assert 'visible == true AND enabled == true' in predicates[0]
    assert 'XCUIElementTypeStaticText' in predicates[0]
    with pytest.raises(Failed):
        phone.click_card('1')
    assert len(predicates) == 1


def test_merged_card_selector_refuses_ambiguous_ancestors():
    phone = Phone(SimpleNamespace(find_elements=lambda *args: [object(), object()]), 36)
    with pytest.raises(Blocked) as error:
        phone.click_card('Use a PIN')
    assert error.value.code == 'PHONE_SELECTOR'


def test_setup_card_waits_for_readiness_without_broadening_predicate(monkeypatch):
    waits, clicks, queries = [], [], []
    results = iter([[], [], [SimpleNamespace(click=lambda: clicks.append(True))]])
    def find(by, predicate):
        queries.append((by, predicate))
        return next(results)
    monkeypatch.setattr('harness.phone.time.sleep', waits.append)
    phone = Phone(SimpleNamespace(find_elements=find), 37)
    phone.click_card('Use a PIN')
    assert waits == [.25, .25]
    assert clicks == [True]
    assert len(set(queries)) == 1


def test_setup_card_absence_is_bounded_and_never_clicks():
    phone = Phone(SimpleNamespace(find_elements=lambda *args: []), 37)
    with pytest.raises(Blocked) as error:
        phone.click_card('Use a PIN', timeout=0)
    assert error.value.code == 'PHONE_SELECTOR'


def test_setup_card_never_retries_an_uncertain_click(monkeypatch):
    clicks = []
    def click():
        clicks.append(True)
        raise RuntimeError('Synthetic lost click response')
    monkeypatch.setattr('harness.phone.time.sleep', lambda _: pytest.fail('must not retry'))
    phone = Phone(SimpleNamespace(find_elements=lambda *args: [SimpleNamespace(click=click)]), 37)
    with pytest.raises(RuntimeError):
        phone.click_card('Use a PIN')
    assert clicks == [True]


def test_gesture_card_uses_only_explicit_container_and_exact_title_line():
    queries, clicks = [], []
    def find(by, predicate):
        queries.append(predicate)
        return [SimpleNamespace(click=lambda: clicks.append(True))]
    phone = Phone(SimpleNamespace(find_elements=find), 37)
    control = phone.card_control('QA synthetic cube', container=True)
    assert clicks == []  # Caller can record durable intent before dispatch.
    control.click()
    assert clicks == [True]
    assert 'type == "XCUIElementTypeOther"' in queries[0]
    assert 'BEGINSWITH "QA synthetic cube\\n"' in queries[0]
    assert 'CONTAINS' not in queries[0]
